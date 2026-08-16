// ABOUTME: Shared server state — per-provider scrape-plane scrapers, provider-tagged sessions,
// ABOUTME: and flow_id-keyed pending interactive logins (ephemeral scrapers holding parked permits)
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::queue::{QueuedScraper, SciotteLimiter, ScrapePermit};
use dravr_sciotte::scraper::ChromeScraper;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{interval as tokio_interval, MissedTickBehavior};
use tracing::{info, warn};

/// Decorator applied to each ephemeral login-plane scraper at construction.
///
/// The seam through which the binary attaches its vision model (a
/// `vision`-feature concern) without this crate depending on that feature:
/// the binary passes `|s| s.with_llm(...)` when vision login is enabled.
pub type LoginScraperDecorator = Box<dyn Fn(ChromeScraper) -> ChromeScraper + Send + Sync>;

/// Outer scraper type used by handlers: a TTL cache wrapping the queue-gated
/// Chrome scraper. Cache hits skip the backpressure limiter entirely; only
/// cache misses and login flows consume a slot.
pub type AppScraper = CachedScraper<QueuedScraper<ChromeScraper>>;

/// Type alias for the shared state handle used across the server.
///
/// `dravr-tronc` 0.5 shares state as `Arc<S>`, so the mutable session store lives
/// behind a per-field interior `RwLock` rather than an outer one.
pub type SharedState = Arc<ServerState>;

/// One provider this instance serves: its scrape-plane scraper plus the config
/// used to build ephemeral login scrapers and drive interactive login pages.
struct ProviderRuntime {
    scraper: AppScraper,
    config: ProviderConfig,
}

/// A session held in the transient store, tagged with the provider it belongs
/// to so scrape requests route to the right provider's scraper (ADR-021).
#[derive(Clone)]
pub struct SessionEntry {
    /// The authenticated browser session (cookies).
    pub session: AuthSession,
    /// Provider name the session authenticates against (e.g. `"garmin"`).
    pub provider: String,
}

/// Interior-mutable session store: the session map plus the latest-session
/// pointer, co-locked so the two stay consistent across updates.
#[derive(Default)]
struct SessionStore {
    by_id: HashMap<String, SessionEntry>,
    latest_session_id: Option<String>,
}

/// An in-flight interactive login parked between HTTP steps.
///
/// Owns an **ephemeral** [`ChromeScraper`] (whose internal pending-login slot
/// holds the live browser awaiting OTP/2FA) and the [`ScrapePermit`] acquired
/// at flow start — so every pending login counts as exactly one Chrome slot
/// until it terminates or the TTL reaper drops it (freeing browser + permit).
/// This is the server-side equivalent of the platform's per-user
/// `PENDING_OTP_SCRAPERS`: N users (or one user on two providers) can log in
/// concurrently, bounded by the limiter's `max_concurrent`.
pub struct LoginFlow {
    /// Ephemeral scraper holding the parked browser for this flow.
    pub scraper: ChromeScraper,
    /// Provider this login authenticates against.
    pub provider: String,
    /// Concurrency slot held for the whole flow lifetime.
    pub permit: ScrapePermit,
    created_at: Instant,
}

/// Why a login-flow lookup failed — mapped to an HTTP status by the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowLookupError {
    /// No pending flow matches (unknown id, or none exist).
    NotFound,
    /// No id given and several flows are pending — the caller must say which.
    Ambiguous,
}

/// Why a provider could not be resolved for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderResolveError {
    /// The named provider is not served by this instance.
    Unknown(String),
    /// No provider named and more than one is served — the caller must pick.
    Ambiguous,
}

/// Central server state: per-provider scrape-plane scrapers, the shared
/// backpressure limiter, the provider-tagged multi-session store, and the
/// flow_id-keyed pending interactive logins.
pub struct ServerState {
    providers: HashMap<String, ProviderRuntime>,
    limiter: Arc<SciotteLimiter>,
    sessions: RwLock<SessionStore>,
    login_flows: Mutex<HashMap<String, LoginFlow>>,
    login_decorator: Option<LoginScraperDecorator>,
    /// Scraper settings for the ephemeral scrapers a login builds.
    ///
    /// Held rather than rebuilt from the environment, so a caller's configuration
    /// governs logins as well as scrapes. Rebuilding meant an embedder could pass an
    /// explicit config, see it honoured for every scrape, and have it silently ignored
    /// for the one operation that drives a real provider's login page.
    scraper_config: ScraperConfig,
}

impl ServerState {
    /// Create server state serving the given providers.
    ///
    /// Each `(config, scraper)` pair is keyed by the config's provider name.
    /// `login_decorator` (when vision login is enabled) is applied to every
    /// ephemeral scraper built via [`new_login_scraper`](Self::new_login_scraper).
    pub fn new(
        providers: Vec<(ProviderConfig, AppScraper)>,
        limiter: Arc<SciotteLimiter>,
        login_decorator: Option<LoginScraperDecorator>,
        scraper_config: ScraperConfig,
    ) -> Self {
        let providers = providers
            .into_iter()
            .map(|(config, scraper)| {
                (
                    config.provider.name.clone(),
                    ProviderRuntime { scraper, config },
                )
            })
            .collect();
        Self {
            providers,
            limiter,
            sessions: RwLock::new(SessionStore::default()),
            login_flows: Mutex::new(HashMap::new()),
            login_decorator,
            scraper_config,
        }
    }

    /// Names of the providers this instance serves.
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// Scrape-plane scraper for a provider, if served.
    pub fn scraper_for(&self, provider: &str) -> Option<&AppScraper> {
        self.providers.get(provider).map(|p| &p.scraper)
    }

    /// Provider config for a provider, if served.
    pub fn config_for(&self, provider: &str) -> Option<&ProviderConfig> {
        self.providers.get(provider).map(|p| &p.config)
    }

    /// Iterate `(provider name, scraper)` pairs — e.g. for per-provider cache
    /// stats in the health endpoint.
    pub fn scrapers(&self) -> impl Iterator<Item = (&str, &AppScraper)> {
        self.providers
            .iter()
            .map(|(name, rt)| (name.as_str(), &rt.scraper))
    }

    /// Resolve the provider for a request: an explicit name must be served;
    /// no name resolves to the sole provider when exactly one is served.
    pub fn resolve_provider(
        &self,
        requested: Option<&str>,
    ) -> Result<String, ProviderResolveError> {
        requested.map_or_else(
            || {
                if self.providers.len() == 1 {
                    Ok(self.providers.keys().next().cloned().unwrap_or_default())
                } else {
                    Err(ProviderResolveError::Ambiguous)
                }
            },
            |name| {
                if self.providers.contains_key(name) {
                    Ok(name.to_owned())
                } else {
                    Err(ProviderResolveError::Unknown(name.to_owned()))
                }
            },
        )
    }

    /// Whether this instance serves exactly one provider. Disk session
    /// persistence (a single untagged file) is only meaningful then.
    pub fn single_provider(&self) -> bool {
        self.providers.len() == 1
    }

    /// Build an ephemeral login-plane scraper for `provider`: a bare
    /// [`ChromeScraper`] (no cache/queue wrappers — the flow owns its permit
    /// directly) with the vision decorator applied when configured.
    pub fn new_login_scraper(&self, provider: &str) -> Option<ChromeScraper> {
        let config = self.config_for(provider)?.clone();
        let scraper = ChromeScraper::new(self.scraper_config.clone(), config);
        Some(match &self.login_decorator {
            Some(decorate) => decorate(scraper),
            None => scraper,
        })
    }

    /// Get a reference to the shared backpressure limiter
    pub const fn limiter(&self) -> &Arc<SciotteLimiter> {
        &self.limiter
    }

    /// Look up a session entry by ID, returning an owned copy.
    pub async fn get_session_entry(&self, session_id: &str) -> Option<SessionEntry> {
        self.sessions.read().await.by_id.get(session_id).cloned()
    }

    /// Look up a session by ID, returning an owned copy of the session only.
    pub async fn get_session(&self, session_id: &str) -> Option<AuthSession> {
        self.get_session_entry(session_id).await.map(|e| e.session)
    }

    /// Add a new session for a provider (or replace one with the same ID).
    pub async fn add_session(&self, session: AuthSession, provider: String) {
        let id = session.session_id.clone();
        let mut store = self.sessions.write().await;
        store
            .by_id
            .insert(id.clone(), SessionEntry { session, provider });
        store.latest_session_id = Some(id);
    }

    /// Remove a session by ID, returning it if it existed
    pub async fn remove_session(&self, session_id: &str) -> Option<AuthSession> {
        let mut store = self.sessions.write().await;
        if store.latest_session_id.as_deref() == Some(session_id) {
            store.latest_session_id = None;
        }
        store.by_id.remove(session_id).map(|e| e.session)
    }

    /// List all active session IDs
    pub async fn list_session_ids(&self) -> Vec<String> {
        self.sessions.read().await.by_id.keys().cloned().collect()
    }

    /// List `(session id, provider)` pairs for all active sessions.
    pub async fn list_sessions(&self) -> Vec<(String, String)> {
        self.sessions
            .read()
            .await
            .by_id
            .iter()
            .map(|(id, e)| (id.clone(), e.provider.clone()))
            .collect()
    }

    /// Get the number of active sessions
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.by_id.len()
    }

    /// Latest session entry (or the only one), with its provider.
    pub async fn session_entry(&self) -> Option<SessionEntry> {
        let store = self.sessions.read().await;
        store
            .latest_session_id
            .as_ref()
            .and_then(|id| store.by_id.get(id))
            .or_else(|| store.by_id.values().next())
            .cloned()
    }

    /// Backward compatibility: return the latest session (or the only session)
    pub async fn session(&self) -> Option<AuthSession> {
        self.session_entry().await.map(|e| e.session)
    }

    /// Clear all sessions
    pub async fn clear_sessions(&self) {
        let mut store = self.sessions.write().await;
        store.by_id.clear();
        store.latest_session_id = None;
    }

    /// Park an in-flight interactive login under `flow_id` so the follow-up
    /// `submit-otp` / `select-2fa` request can resume it.
    pub async fn park_login_flow(
        &self,
        flow_id: String,
        scraper: ChromeScraper,
        provider: String,
        permit: ScrapePermit,
    ) {
        let previous = self.login_flows.lock().await.insert(
            flow_id,
            LoginFlow {
                scraper,
                provider,
                permit,
                created_at: Instant::now(),
            },
        );
        if previous.is_some() {
            // Same flow re-parked between steps — expected, not a collision.
            info!("Login flow re-parked for next step");
        }
    }

    /// Take a pending login flow for its next step. `flow_id = None` falls
    /// back to the sole pending flow (single-login callers never need ids);
    /// with several pending, the caller must name one.
    pub async fn take_login_flow(
        &self,
        flow_id: Option<&str>,
    ) -> Result<(String, LoginFlow), FlowLookupError> {
        let mut flows = self.login_flows.lock().await;
        match flow_id {
            Some(id) => flows.remove_entry(id).ok_or(FlowLookupError::NotFound),
            None => match flows.len() {
                0 => Err(FlowLookupError::NotFound),
                1 => {
                    let id = flows.keys().next().cloned().unwrap_or_default();
                    flows.remove_entry(&id).ok_or(FlowLookupError::NotFound)
                }
                _ => Err(FlowLookupError::Ambiguous),
            },
        }
    }

    /// Number of pending interactive login flows.
    pub async fn login_flow_count(&self) -> usize {
        self.login_flows.lock().await.len()
    }

    /// Drop pending login flows older than `ttl`. Dropping a flow releases its
    /// browser (Chrome dies with the scraper) and its permit (slot frees).
    pub async fn evict_stale_login_flows(&self, ttl: Duration) -> usize {
        let mut flows = self.login_flows.lock().await;
        let before = flows.len();
        flows.retain(|flow_id, flow| {
            let stale = flow.created_at.elapsed() > ttl;
            if stale {
                warn!(
                    flow_id = %flow_id,
                    provider = %flow.provider,
                    age_secs = flow.created_at.elapsed().as_secs(),
                    "Evicting stale pending login flow (browser + permit released)"
                );
            }
            !stale
        });
        before - flows.len()
    }

    /// Spawn the background reaper that evicts stale pending login flows.
    /// Returns the [`JoinHandle`] so callers can abort it during shutdown.
    pub fn spawn_login_flow_reaper(
        self: &Arc<Self>,
        ttl: Duration,
        interval: Duration,
    ) -> JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio_interval(interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            info!(
                ttl_secs = ttl.as_secs(),
                interval_secs = interval.as_secs(),
                "Login flow reaper started"
            );
            loop {
                ticker.tick().await;
                me.evict_stale_login_flows(ttl).await;
            }
        })
    }
}

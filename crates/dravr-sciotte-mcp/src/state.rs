// ABOUTME: Shared server state holding the cached+queued scraper and multi-session store
// ABOUTME: Thread-safe via Arc<RwLock> for concurrent access from transport handlers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::sync::Arc;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::queue::{QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use tokio::sync::RwLock;

/// Outer scraper type used by handlers: a TTL cache wrapping the queue-gated
/// Chrome scraper. Cache hits skip the backpressure limiter entirely; only
/// cache misses and login flows consume a slot.
pub type AppScraper = CachedScraper<QueuedScraper<ChromeScraper>>;

/// Type alias for the shared state handle used across the server.
///
/// `dravr-tronc` 0.5 shares state as `Arc<S>`, so the mutable session store lives
/// behind a per-field interior `RwLock` rather than an outer one.
pub type SharedState = Arc<ServerState>;

/// Interior-mutable session store: the session map plus the latest-session
/// pointer, co-locked so the two stay consistent across updates.
#[derive(Default)]
struct SessionStore {
    by_id: HashMap<String, AuthSession>,
    latest_session_id: Option<String>,
}

/// Central server state holding the scraper, the backpressure limiter, and
/// the multi-session authentication store.
///
/// Sessions are keyed by `session_id`. The `latest_session_id` tracks the most
/// recently created session for backward compatibility with single-session callers.
pub struct ServerState {
    scraper: AppScraper,
    limiter: Arc<SciotteLimiter>,
    sessions: RwLock<SessionStore>,
    provider: String,
}

impl ServerState {
    /// Create server state with the given cached+queued scraper, limiter, and the
    /// provider name this instance serves (from its `ProviderConfig`).
    ///
    /// The provider is reported in the `authenticated` login responses so a
    /// platform caller can persist the session under the right provider without
    /// tracking it itself across the multi-step 2FA flow (ADR-021).
    pub fn new(scraper: AppScraper, limiter: Arc<SciotteLimiter>, provider: String) -> Self {
        Self {
            scraper,
            limiter,
            sessions: RwLock::new(SessionStore::default()),
            provider,
        }
    }

    /// Get a reference to the cached scraper
    pub const fn scraper(&self) -> &AppScraper {
        &self.scraper
    }

    /// The provider name this instance serves (e.g. `"garmin"`, `"strava"`).
    #[must_use]
    pub fn provider_name(&self) -> &str {
        &self.provider
    }

    /// Get a reference to the shared backpressure limiter
    pub const fn limiter(&self) -> &Arc<SciotteLimiter> {
        &self.limiter
    }

    /// Look up a session by ID, returning an owned copy.
    pub async fn get_session(&self, session_id: &str) -> Option<AuthSession> {
        self.sessions.read().await.by_id.get(session_id).cloned()
    }

    /// Add a new session (or replace an existing one with the same ID)
    pub async fn add_session(&self, session: AuthSession) {
        let id = session.session_id.clone();
        let mut store = self.sessions.write().await;
        store.by_id.insert(id.clone(), session);
        store.latest_session_id = Some(id);
    }

    /// Remove a session by ID, returning it if it existed
    pub async fn remove_session(&self, session_id: &str) -> Option<AuthSession> {
        let mut store = self.sessions.write().await;
        if store.latest_session_id.as_deref() == Some(session_id) {
            store.latest_session_id = None;
        }
        store.by_id.remove(session_id)
    }

    /// List all active session IDs
    pub async fn list_session_ids(&self) -> Vec<String> {
        self.sessions.read().await.by_id.keys().cloned().collect()
    }

    /// Get the number of active sessions
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.by_id.len()
    }

    /// Backward compatibility: return the latest session (or the only session)
    pub async fn session(&self) -> Option<AuthSession> {
        let store = self.sessions.read().await;
        store
            .latest_session_id
            .as_ref()
            .and_then(|id| store.by_id.get(id))
            .or_else(|| store.by_id.values().next())
            .cloned()
    }

    /// Backward compatibility: set session (adds to the store)
    pub async fn set_session(&self, session: AuthSession) {
        self.add_session(session).await;
    }

    /// Clear all sessions
    pub async fn clear_sessions(&self) {
        let mut store = self.sessions.write().await;
        store.by_id.clear();
        store.latest_session_id = None;
    }
}

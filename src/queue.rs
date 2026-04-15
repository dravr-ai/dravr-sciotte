// ABOUTME: FIFO queue + bounded semaphore gating concurrent Chrome scraper operations
// ABOUTME: Wraps any ActivityScraper with backpressure so spawned browsers can't exhaust the host
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Request queue and backpressure
//!
//! `SciotteLimiter` is a FIFO-fair semaphore-based limiter that caps the number
//! of concurrent Chrome-driven scraper operations and shed excess load before
//! new work is started. It is designed for deployment targets like Cloud Run
//! where spawning unbounded Chrome processes would OOM the container.
//!
//! `QueuedScraper<S>` is a thin wrapper that implements [`ActivityScraper`] by
//! acquiring a permit before delegating to the inner scraper. Multi-step login
//! flows (`credential_login` → `submit_otp` / `select_two_factor`) park the
//! permit across HTTP requests so the in-flight browser counts against the
//! concurrency budget for its full lifetime, not just the first call.
//!
//! A background watchdog evicts parked permits older than the configured TTL,
//! guaranteeing that an abandoned 2FA flow cannot hold a slot forever.

use std::env;
use std::num::ParseIntError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, info, warn};

use crate::error::{LoginResult, ScraperError, ScraperResult};
use crate::models::{
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams,
};
use crate::types::ActivityScraper;

// ============================================================================
// Configuration
// ============================================================================
//
// This module intentionally defines NO numeric defaults. Operators are the
// single source of truth for queue sizing, timeouts, and Retry-After hints;
// the binaries read `DRAVR_SCIOTTE_*` environment variables at startup via
// [`QueueConfig::from_env`] and fail fast if any are missing. Production
// values live in Terraform / .envrc and are reviewable as infra, not code.

/// Environment variable for the maximum number of concurrent scrapes
pub const ENV_MAX_CONCURRENT: &str = "DRAVR_SCIOTTE_MAX_CONCURRENT";
/// Environment variable for the maximum combined queue depth
pub const ENV_MAX_QUEUE_DEPTH: &str = "DRAVR_SCIOTTE_MAX_QUEUE";
/// Environment variable for the acquire timeout in seconds
pub const ENV_ACQUIRE_TIMEOUT_SECS: &str = "DRAVR_SCIOTTE_QUEUE_TIMEOUT_SECS";
/// Environment variable for the parked-permit TTL in seconds
pub const ENV_PARKED_PERMIT_TTL_SECS: &str = "DRAVR_SCIOTTE_PARKED_PERMIT_TTL_SECS";
/// Environment variable for the watchdog tick interval in seconds
pub const ENV_WATCHDOG_INTERVAL_SECS: &str = "DRAVR_SCIOTTE_WATCHDOG_INTERVAL_SECS";
/// Environment variable for the `Retry-After` hint on queue-full / acquire-timeout rejections
pub const ENV_RETRY_AFTER_HINT_SECS: &str = "DRAVR_SCIOTTE_RETRY_AFTER_HINT_SECS";
/// Environment variable for the `Retry-After` hint when the limiter is closed (shutdown)
pub const ENV_CLOSED_RETRY_AFTER_SECS: &str = "DRAVR_SCIOTTE_CLOSED_RETRY_AFTER_SECS";

/// Runtime configuration for [`SciotteLimiter`]. Every field is mandatory and
/// must be supplied by the caller — the crate ships no numeric defaults.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Hard cap on concurrently running scrapes. This maps directly to the
    /// number of simultaneous Chrome processes that can be alive inside the
    /// limiter's scope.
    pub max_concurrent: usize,
    /// Combined cap on in-flight + waiting scrapes. When the depth exceeds
    /// this value the limiter fast-rejects new requests so the server does
    /// not accumulate an unbounded backlog of pending futures.
    pub max_queue_depth: usize,
    /// Upper bound on how long a waiting request will block before being
    /// rejected with [`LimiterError::AcquireTimeout`].
    pub acquire_timeout: Duration,
    /// Upper bound on how long a permit can stay parked across a multi-step
    /// flow (e.g. the user walks away from the 2FA screen). After this the
    /// watchdog drops the permit so the slot can be reused.
    pub parked_permit_ttl: Duration,
    /// Interval at which the watchdog scans for stale parked permits.
    pub watchdog_interval: Duration,
    /// `Retry-After` hint emitted on [`LimiterError::QueueFull`] and
    /// [`LimiterError::AcquireTimeout`] rejections.
    pub retry_after_hint: Duration,
    /// `Retry-After` hint emitted on [`LimiterError::Closed`] rejections.
    pub closed_retry_after: Duration,
}

/// Error returned by [`QueueConfig::from_env`] when a required variable is
/// missing or cannot be parsed. The binary is expected to surface this to
/// the operator and abort startup.
#[derive(Debug, Error)]
pub enum QueueConfigError {
    /// A required environment variable is not set.
    #[error("missing required environment variable `{0}`")]
    Missing(&'static str),
    /// A required environment variable failed to parse as an integer.
    #[error("environment variable `{name}` is not a valid integer: {source}")]
    Parse {
        /// Name of the offending variable
        name: &'static str,
        /// Underlying parse error
        #[source]
        source: ParseIntError,
    },
    /// `max_queue_depth` is smaller than `max_concurrent`, which would
    /// starve the running pool.
    #[error(
        "max_queue_depth ({queue}) must be >= max_concurrent ({concurrent}) — \
         otherwise active scrapes count against a queue that cannot hold them"
    )]
    QueueSmallerThanConcurrency {
        /// Configured max concurrent value
        concurrent: usize,
        /// Configured queue depth value
        queue: usize,
    },
    /// A numeric field was set to zero where zero is not a legal value.
    #[error("environment variable `{0}` must be greater than zero")]
    ZeroValue(&'static str),
}

impl QueueConfig {
    /// Read every required field from environment variables and fail fast on
    /// missing or malformed values. Call this once from your binary's `main`.
    pub fn from_env() -> Result<Self, QueueConfigError> {
        let max_concurrent = required_usize(ENV_MAX_CONCURRENT)?;
        if max_concurrent == 0 {
            return Err(QueueConfigError::ZeroValue(ENV_MAX_CONCURRENT));
        }
        let max_queue_depth = required_usize(ENV_MAX_QUEUE_DEPTH)?;
        if max_queue_depth < max_concurrent {
            return Err(QueueConfigError::QueueSmallerThanConcurrency {
                concurrent: max_concurrent,
                queue: max_queue_depth,
            });
        }

        let acquire_timeout = required_duration_secs(ENV_ACQUIRE_TIMEOUT_SECS)?;
        let parked_permit_ttl = required_duration_secs(ENV_PARKED_PERMIT_TTL_SECS)?;
        let watchdog_interval = required_duration_secs(ENV_WATCHDOG_INTERVAL_SECS)?;
        if watchdog_interval.is_zero() {
            return Err(QueueConfigError::ZeroValue(ENV_WATCHDOG_INTERVAL_SECS));
        }
        let retry_after_hint = required_duration_secs(ENV_RETRY_AFTER_HINT_SECS)?;
        let closed_retry_after = required_duration_secs(ENV_CLOSED_RETRY_AFTER_SECS)?;

        Ok(Self {
            max_concurrent,
            max_queue_depth,
            acquire_timeout,
            parked_permit_ttl,
            watchdog_interval,
            retry_after_hint,
            closed_retry_after,
        })
    }
}

fn required_usize(name: &'static str) -> Result<usize, QueueConfigError> {
    let raw = env::var(name).map_err(|_| QueueConfigError::Missing(name))?;
    raw.parse::<usize>()
        .map_err(|source| QueueConfigError::Parse { name, source })
}

fn required_duration_secs(name: &'static str) -> Result<Duration, QueueConfigError> {
    let raw = env::var(name).map_err(|_| QueueConfigError::Missing(name))?;
    let secs = raw
        .parse::<u64>()
        .map_err(|source| QueueConfigError::Parse { name, source })?;
    Ok(Duration::from_secs(secs))
}

// ============================================================================
// Errors
// ============================================================================

/// Errors produced by the scraper limiter when a request cannot be admitted.
///
/// Each variant carries the configured `Retry-After` hint so the HTTP layer
/// can propagate it to clients without reaching back into the limiter.
#[derive(Debug, Error)]
pub enum LimiterError {
    /// The combined running + waiting depth is at the configured cap and the
    /// request was fast-rejected without queueing.
    #[error("scraper queue is full (depth {depth}, max {max})")]
    QueueFull {
        /// Depth observed when the rejection was issued
        depth: usize,
        /// Configured maximum depth
        max: usize,
        /// Retry-After hint to propagate to clients
        retry_after: Duration,
    },

    /// The request waited the full `acquire_timeout` without getting a permit.
    #[error("timed out waiting for a scraper slot after {}s", .timeout.as_secs())]
    AcquireTimeout {
        /// Configured acquire timeout
        timeout: Duration,
        /// Retry-After hint to propagate to clients
        retry_after: Duration,
    },

    /// No permit was available at the time of a non-blocking `try_acquire`.
    /// Distinct from [`AcquireTimeout`] because the caller explicitly opted
    /// out of waiting.
    #[error("no scraper permit available (try_acquire)")]
    NoCapacity {
        /// Retry-After hint to propagate to clients
        retry_after: Duration,
    },

    /// The underlying semaphore has been closed (typically during shutdown).
    #[error("scraper limiter has been shut down")]
    Closed {
        /// Retry-After hint to propagate to clients
        retry_after: Duration,
    },
}

impl LimiterError {
    /// `Retry-After` value in seconds for HTTP 503 responses, read from the
    /// hint carried inside the error variant.
    #[must_use]
    pub const fn retry_after_secs(&self) -> u64 {
        match self {
            Self::QueueFull { retry_after, .. }
            | Self::AcquireTimeout { retry_after, .. }
            | Self::NoCapacity { retry_after }
            | Self::Closed { retry_after } => retry_after.as_secs(),
        }
    }

    /// Convert into a [`ScraperError::Busy`] suitable for bubbling up through
    /// the `ActivityScraper` trait.
    #[must_use]
    pub fn into_scraper_error(self) -> ScraperError {
        let retry_after_secs = self.retry_after_secs();
        ScraperError::Busy {
            reason: self.to_string(),
            retry_after_secs,
        }
    }
}

// ============================================================================
// Permit RAII
// ============================================================================

/// An acquired scraper slot. Dropping this guard releases the underlying
/// semaphore permit and decrements the queue depth atomically.
pub struct ScrapePermit {
    permit: Option<OwnedSemaphorePermit>,
    depth: Arc<AtomicUsize>,
    acquired_at: Instant,
}

impl ScrapePermit {
    /// When the permit was acquired. Useful for watchdog staleness checks.
    #[must_use]
    pub const fn acquired_at(&self) -> Instant {
        self.acquired_at
    }
}

impl Drop for ScrapePermit {
    fn drop(&mut self) {
        // The owned semaphore permit has its own Drop which releases the slot.
        // Take it first so we never double-decrement.
        drop(self.permit.take());
        self.depth.fetch_sub(1, Ordering::AcqRel);
    }
}

// ============================================================================
// Limiter
// ============================================================================

struct ParkedSlot {
    permit: ScrapePermit,
    parked_at: Instant,
}

/// FIFO-fair limiter for Chrome-backed scraping.
///
/// Holds a semaphore sized to the allowed concurrency, an atomic depth
/// counter for fast-reject, and an optional parked permit slot used by
/// multi-step login flows.
pub struct SciotteLimiter {
    semaphore: Arc<Semaphore>,
    config: QueueConfig,
    depth: Arc<AtomicUsize>,
    parked: Mutex<Option<ParkedSlot>>,
}

impl SciotteLimiter {
    /// Build a limiter from an explicit configuration. This is the only
    /// constructor — there is no `from_env` fallback. Binaries should read
    /// [`QueueConfig::from_env`] once at startup and pass the result here.
    #[must_use]
    pub fn new(config: QueueConfig) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            depth: Arc::new(AtomicUsize::new(0)),
            parked: Mutex::new(None),
            config,
        })
    }

    /// Configuration used by this limiter.
    #[must_use]
    pub const fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Current observed depth (running + waiting).
    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }

    /// Number of free semaphore permits right now.
    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Acquire a permit, waiting up to `acquire_timeout` if the semaphore is
    /// saturated. Fast-rejects with [`LimiterError::QueueFull`] when the
    /// combined depth already exceeds `max_queue_depth`.
    pub async fn acquire(&self) -> Result<ScrapePermit, LimiterError> {
        let prior = self.depth.fetch_add(1, Ordering::AcqRel);
        if prior >= self.config.max_queue_depth {
            self.depth.fetch_sub(1, Ordering::AcqRel);
            return Err(LimiterError::QueueFull {
                depth: prior,
                max: self.config.max_queue_depth,
                retry_after: self.config.retry_after_hint,
            });
        }

        let sem = Arc::clone(&self.semaphore);
        let permit = match time::timeout(self.config.acquire_timeout, sem.acquire_owned()).await {
            Ok(Ok(p)) => p,
            Ok(Err(_closed)) => {
                self.depth.fetch_sub(1, Ordering::AcqRel);
                return Err(LimiterError::Closed {
                    retry_after: self.config.closed_retry_after,
                });
            }
            Err(_elapsed) => {
                self.depth.fetch_sub(1, Ordering::AcqRel);
                return Err(LimiterError::AcquireTimeout {
                    timeout: self.config.acquire_timeout,
                    retry_after: self.config.retry_after_hint,
                });
            }
        };

        debug!(
            depth = self.depth(),
            available = self.available_permits(),
            "Scraper permit acquired"
        );
        Ok(ScrapePermit {
            permit: Some(permit),
            depth: Arc::clone(&self.depth),
            acquired_at: Instant::now(),
        })
    }

    /// Non-blocking permit acquisition for low-priority background work.
    /// Returns [`LimiterError::NoCapacity`] immediately if no permit is
    /// available.
    pub fn try_acquire(&self) -> Result<ScrapePermit, LimiterError> {
        let prior = self.depth.fetch_add(1, Ordering::AcqRel);
        if prior >= self.config.max_queue_depth {
            self.depth.fetch_sub(1, Ordering::AcqRel);
            return Err(LimiterError::QueueFull {
                depth: prior,
                max: self.config.max_queue_depth,
                retry_after: self.config.retry_after_hint,
            });
        }

        let sem = Arc::clone(&self.semaphore);
        match sem.try_acquire_owned() {
            Ok(permit) => Ok(ScrapePermit {
                permit: Some(permit),
                depth: Arc::clone(&self.depth),
                acquired_at: Instant::now(),
            }),
            Err(TryAcquireError::NoPermits) => {
                self.depth.fetch_sub(1, Ordering::AcqRel);
                Err(LimiterError::NoCapacity {
                    retry_after: self.config.retry_after_hint,
                })
            }
            Err(TryAcquireError::Closed) => {
                self.depth.fetch_sub(1, Ordering::AcqRel);
                Err(LimiterError::Closed {
                    retry_after: self.config.closed_retry_after,
                })
            }
        }
    }

    /// Park a permit for the duration of a multi-step login flow. Any permit
    /// already parked is dropped (its flow is considered abandoned).
    pub async fn park(&self, permit: ScrapePermit) {
        let previous = self.parked.lock().await.replace(ParkedSlot {
            permit,
            parked_at: Instant::now(),
        });
        if previous.is_some() {
            warn!("Parked scraper permit overwritten — previous login flow abandoned");
        }
    }

    /// Remove and return the currently parked permit, if any.
    pub async fn take_parked(&self) -> Option<ScrapePermit> {
        self.parked.lock().await.take().map(|slot| slot.permit)
    }

    /// Drop any parked permit older than the configured TTL. Returns whether
    /// an eviction happened. Exposed so tests and watchdog ticks can share
    /// the same logic.
    pub async fn evict_stale_parked(&self) -> bool {
        let mut guard = self.parked.lock().await;
        let should_evict = guard
            .as_ref()
            .is_some_and(|slot| slot.parked_at.elapsed() > self.config.parked_permit_ttl);
        if should_evict {
            if let Some(slot) = guard.take() {
                warn!(
                    age_secs = slot.parked_at.elapsed().as_secs(),
                    ttl_secs = self.config.parked_permit_ttl.as_secs(),
                    "Evicting stale parked scraper permit"
                );
            }
            true
        } else {
            false
        }
    }

    /// Spawn the background watchdog that periodically evicts stale parked
    /// permits. Returns the [`JoinHandle`] so callers can abort it during
    /// shutdown.
    pub fn spawn_watchdog(self: &Arc<Self>) -> JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = time::interval(me.config.watchdog_interval);
            ticker.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            info!(
                interval_secs = me.config.watchdog_interval.as_secs(),
                ttl_secs = me.config.parked_permit_ttl.as_secs(),
                "Scraper limiter watchdog started"
            );
            loop {
                ticker.tick().await;
                me.evict_stale_parked().await;
            }
        })
    }
}

// ============================================================================
// QueuedScraper — ActivityScraper wrapper
// ============================================================================

/// Queue-gated wrapper around an inner [`ActivityScraper`].
///
/// Every trait method acquires a permit from the shared [`SciotteLimiter`]
/// before running. Multi-step login methods transparently park and reclaim
/// the permit so a flow spanning multiple HTTP requests still counts as
/// exactly one Chrome slot for its full lifetime.
pub struct QueuedScraper<S> {
    inner: S,
    limiter: Arc<SciotteLimiter>,
}

impl<S> QueuedScraper<S> {
    /// Wrap an inner scraper with the given limiter.
    pub const fn new(inner: S, limiter: Arc<SciotteLimiter>) -> Self {
        Self { inner, limiter }
    }

    /// Borrow the inner scraper. Consumers that need type-specific accessors
    /// (e.g. `ChromeScraper::provider`) can chain through this method.
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    /// Borrow the shared limiter.
    pub const fn limiter(&self) -> &Arc<SciotteLimiter> {
        &self.limiter
    }
}

/// Classifies a `LoginResult` as either a terminal outcome (Success / Failed)
/// or a continuation that should park the scraper permit.
fn login_result_is_continuation(result: &LoginResult) -> bool {
    matches!(
        result,
        LoginResult::OtpRequired | LoginResult::TwoFactorChoice(_) | LoginResult::NumberMatch(_)
    )
}

#[async_trait]
impl<S: ActivityScraper> ActivityScraper for QueuedScraper<S> {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;
        let outcome = self.inner.browser_login().await;
        drop(permit);
        outcome
    }

    async fn credential_login(
        &self,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;

        let outcome = self.inner.credential_login(email, password, method).await;

        match &outcome {
            Ok(result) if login_result_is_continuation(result) => {
                self.limiter.park(permit).await;
            }
            _ => drop(permit),
        }
        outcome
    }

    async fn submit_otp(&self, code: &str) -> ScraperResult<LoginResult> {
        let permit = match self.limiter.take_parked().await {
            Some(p) => p,
            None => self
                .limiter
                .acquire()
                .await
                .map_err(LimiterError::into_scraper_error)?,
        };

        let outcome = self.inner.submit_otp(code).await;

        match &outcome {
            Ok(result) if login_result_is_continuation(result) => {
                self.limiter.park(permit).await;
            }
            _ => drop(permit),
        }
        outcome
    }

    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult> {
        let permit = match self.limiter.take_parked().await {
            Some(p) => p,
            None => self
                .limiter
                .acquire()
                .await
                .map_err(LimiterError::into_scraper_error)?,
        };

        let outcome = self.inner.select_two_factor(option_id).await;

        match &outcome {
            Ok(result) if login_result_is_continuation(result) => {
                self.limiter.park(permit).await;
            }
            _ => drop(permit),
        }
        outcome
    }

    async fn is_authenticated(&self, session: &AuthSession) -> bool {
        // Authentication probes only read cached cookie state and never
        // launch a browser; they bypass the limiter.
        self.inner.is_authenticated(session).await
    }

    async fn get_activities(
        &self,
        session: &AuthSession,
        params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;
        let outcome = self.inner.get_activities(session, params).await;
        drop(permit);
        outcome
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;
        let outcome = self.inner.get_activity(session, activity_id).await;
        drop(permit);
        outcome
    }

    async fn get_athlete(&self, session: &AuthSession) -> ScraperResult<AthleteProfile> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;
        let outcome = self.inner.get_athlete(session).await;
        drop(permit);
        outcome
    }

    async fn get_daily_summary(
        &self,
        session: &AuthSession,
        params: &HealthParams,
    ) -> ScraperResult<DailySummary> {
        let permit = self
            .limiter
            .acquire()
            .await
            .map_err(LimiterError::into_scraper_error)?;
        let outcome = self.inner.get_daily_summary(session, params).await;
        drop(permit);
        outcome
    }

    async fn close_browser(&self) {
        self.inner.close_browser().await;
    }
}

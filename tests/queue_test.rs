// ABOUTME: Integration tests for the SciotteLimiter + QueuedScraper backpressure primitives
// ABOUTME: Exercises FIFO fairness, queue-full rejection, timeout, parked permits, and watchdog eviction
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dravr_sciotte::error::{LoginResult, ScraperError, ScraperResult, TwoFactorOption};
use dravr_sciotte::models::{
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams,
};
use dravr_sciotte::queue::{LimiterError, QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::ActivityScraper;
use tokio::sync::{Mutex, Notify};
use tokio::time::{self, Instant};

// ============================================================================
// Fake scraper
// ============================================================================

/// Clone-able handle exposing the counters so tests can observe the scraper
/// without holding the scraper itself (which is owned by the `QueuedScraper`).
#[derive(Clone, Default)]
struct FakeStats {
    active: Arc<AtomicU32>,
    peak_active: Arc<AtomicU32>,
    total_started: Arc<AtomicU32>,
}

impl FakeStats {
    fn peak(&self) -> u32 {
        self.peak_active.load(Ordering::Acquire)
    }
    fn started(&self) -> u32 {
        self.total_started.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
enum ScriptedLogin {
    Success,
    OtpRequired,
    TwoFactorChoice,
    Failed,
}

impl ScriptedLogin {
    fn into_result(self) -> LoginResult {
        match self {
            Self::Success => LoginResult::Success(AuthSession {
                session_id: "fake-session".to_owned(),
                cookies: vec![],
                created_at: chrono::Utc::now(),
                expires_at: None,
            }),
            Self::OtpRequired => LoginResult::OtpRequired,
            Self::TwoFactorChoice => LoginResult::TwoFactorChoice(vec![TwoFactorOption {
                id: "otp".to_owned(),
                label: "Authenticator".to_owned(),
            }]),
            Self::Failed => LoginResult::Failed("bad password".to_owned()),
        }
    }
}

struct FakeScriptedScraper {
    stats: FakeStats,
    hold: Duration,
    gate: Option<Arc<Notify>>,
    scripted_login: Mutex<Vec<ScriptedLogin>>,
}

impl FakeScriptedScraper {
    fn new(hold: Duration) -> Self {
        Self {
            stats: FakeStats::default(),
            hold,
            gate: None,
            scripted_login: Mutex::new(Vec::new()),
        }
    }

    fn with_gate(hold: Duration, gate: Arc<Notify>) -> Self {
        Self {
            stats: FakeStats::default(),
            hold,
            gate: Some(gate),
            scripted_login: Mutex::new(Vec::new()),
        }
    }

    fn stats_handle(&self) -> FakeStats {
        self.stats.clone()
    }

    async fn script(&self, items: Vec<ScriptedLogin>) {
        // Store reversed so `pop()` yields in definition order.
        let mut reversed = items;
        reversed.reverse();
        *self.scripted_login.lock().await = reversed;
    }

    async fn next_login(&self) -> LoginResult {
        let mut guard = self.scripted_login.lock().await;
        guard.pop().map_or_else(
            || LoginResult::Failed("no scripted result".to_owned()),
            ScriptedLogin::into_result,
        )
    }

    async fn track_busy_work(&self) {
        let active_now = self.stats.active.fetch_add(1, Ordering::AcqRel) + 1;
        self.stats
            .peak_active
            .fetch_max(active_now, Ordering::AcqRel);
        self.stats.total_started.fetch_add(1, Ordering::AcqRel);

        if let Some(gate) = &self.gate {
            gate.notified().await;
        } else {
            time::sleep(self.hold).await;
        }

        self.stats.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait]
impl ActivityScraper for FakeScriptedScraper {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        self.track_busy_work().await;
        Ok(AuthSession {
            session_id: "fake-browser".to_owned(),
            cookies: vec![],
            created_at: chrono::Utc::now(),
            expires_at: None,
        })
    }

    async fn credential_login(
        &self,
        _email: &str,
        _password: &str,
        _method: &str,
    ) -> ScraperResult<LoginResult> {
        self.track_busy_work().await;
        Ok(self.next_login().await)
    }

    async fn submit_otp(&self, _code: &str) -> ScraperResult<LoginResult> {
        self.track_busy_work().await;
        Ok(self.next_login().await)
    }

    async fn select_two_factor(&self, _option_id: &str) -> ScraperResult<LoginResult> {
        self.track_busy_work().await;
        Ok(self.next_login().await)
    }

    async fn is_authenticated(&self, _session: &AuthSession) -> bool {
        true
    }

    async fn get_activities(
        &self,
        _session: &AuthSession,
        _params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>> {
        self.track_busy_work().await;
        Ok(vec![])
    }

    async fn get_activity(
        &self,
        _session: &AuthSession,
        _activity_id: &str,
    ) -> ScraperResult<Activity> {
        Err(ScraperError::Scraping {
            reason: "not used in tests".to_owned(),
        })
    }

    async fn get_athlete(&self, _session: &AuthSession) -> ScraperResult<AthleteProfile> {
        Err(ScraperError::Scraping {
            reason: "not used in tests".to_owned(),
        })
    }

    async fn get_daily_summary(
        &self,
        _session: &AuthSession,
        _params: &HealthParams,
    ) -> ScraperResult<DailySummary> {
        Err(ScraperError::Scraping {
            reason: "not used in tests".to_owned(),
        })
    }
}

fn test_session() -> AuthSession {
    AuthSession {
        session_id: "test".to_owned(),
        cookies: vec![],
        created_at: chrono::Utc::now(),
        expires_at: None,
    }
}

fn test_config(max_concurrent: usize, max_queue: usize) -> QueueConfig {
    QueueConfig {
        max_concurrent,
        max_queue_depth: max_queue,
        acquire_timeout: Duration::from_millis(200),
        parked_permit_ttl: Duration::from_millis(200),
        watchdog_interval: Duration::from_millis(50),
        retry_after_hint: Duration::from_millis(500),
        closed_retry_after: Duration::from_secs(1),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[tokio::test]
async fn limiter_caps_concurrent_scrapes() {
    let limiter = SciotteLimiter::new(test_config(2, 16));
    let inner = FakeScriptedScraper::new(Duration::from_millis(80));
    let stats = inner.stats_handle();
    let scraper = Arc::new(QueuedScraper::new(inner, limiter));

    let session = test_session();
    let params = ActivityParams::default();

    let mut handles = Vec::with_capacity(6);
    for _ in 0..6_u32 {
        let s = Arc::clone(&scraper);
        let sess = session.clone();
        let p = params.clone();
        handles.push(tokio::spawn(async move {
            s.get_activities(&sess, &p).await.expect("activities ok");
        }));
    }
    for h in handles {
        h.await.expect("task joined");
    }

    assert!(
        stats.peak() <= 2,
        "peak concurrency {} exceeded cap 2",
        stats.peak()
    );
    assert_eq!(stats.started(), 6, "every request must eventually run");
}

#[tokio::test]
async fn limiter_rejects_when_queue_full() {
    let limiter = SciotteLimiter::new(QueueConfig {
        max_concurrent: 1,
        max_queue_depth: 2,
        acquire_timeout: Duration::from_secs(3),
        parked_permit_ttl: Duration::from_secs(5),
        watchdog_interval: Duration::from_secs(1),
        retry_after_hint: Duration::from_secs(2),
        closed_retry_after: Duration::from_secs(5),
    });
    let inner = FakeScriptedScraper::new(Duration::from_millis(80));
    let scraper = Arc::new(QueuedScraper::new(inner, Arc::clone(&limiter)));

    let session = test_session();
    let params = ActivityParams::default();

    // Saturate: 1 in-flight + 1 waiting. The third request must be
    // fast-rejected with a Busy error.
    let s1 = Arc::clone(&scraper);
    let sess1 = session.clone();
    let p1 = params.clone();
    let first = tokio::spawn(async move { s1.get_activities(&sess1, &p1).await });

    let s2 = Arc::clone(&scraper);
    let sess2 = session.clone();
    let p2 = params.clone();
    let second = tokio::spawn(async move { s2.get_activities(&sess2, &p2).await });

    // Give both a chance to enqueue behind the semaphore
    time::sleep(Duration::from_millis(10)).await;
    assert_eq!(limiter.depth(), 2);

    let rejected = scraper.get_activities(&session, &params).await;
    assert!(
        matches!(rejected, Err(ScraperError::Busy { .. })),
        "third call should be rejected with Busy, got {rejected:?}"
    );

    first.await.expect("first joined").expect("ok");
    second.await.expect("second joined").expect("ok");
}

#[tokio::test]
async fn acquire_timeout_emits_busy_error() {
    let gate = Arc::new(Notify::new());
    let limiter = SciotteLimiter::new(test_config(1, 16));
    let inner = FakeScriptedScraper::with_gate(Duration::from_millis(0), Arc::clone(&gate));
    let scraper = Arc::new(QueuedScraper::new(inner, Arc::clone(&limiter)));

    let s1 = Arc::clone(&scraper);
    let blocker = tokio::spawn(async move {
        s1.get_activities(&test_session(), &ActivityParams::default())
            .await
    });
    time::sleep(Duration::from_millis(20)).await;

    let started = Instant::now();
    let waiter_result = scraper
        .get_activities(&test_session(), &ActivityParams::default())
        .await;
    let elapsed = started.elapsed();

    assert!(
        matches!(waiter_result, Err(ScraperError::Busy { .. })),
        "waiter should Busy-out, got {waiter_result:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(150),
        "waiter returned too early: {elapsed:?}"
    );

    gate.notify_waiters();
    blocker.await.expect("blocker joined").expect("ok");
}

#[tokio::test]
async fn fifo_ordering_across_waiters() {
    let gate = Arc::new(Notify::new());
    let limiter = SciotteLimiter::new(QueueConfig {
        max_concurrent: 1,
        max_queue_depth: 16,
        acquire_timeout: Duration::from_secs(2),
        parked_permit_ttl: Duration::from_secs(10),
        watchdog_interval: Duration::from_secs(1),
        retry_after_hint: Duration::from_secs(2),
        closed_retry_after: Duration::from_secs(5),
    });
    let inner = FakeScriptedScraper::with_gate(Duration::from_millis(0), Arc::clone(&gate));
    let scraper = Arc::new(QueuedScraper::new(inner, Arc::clone(&limiter)));

    // Hold the slot with one request, then line up three more in order.
    let s0 = Arc::clone(&scraper);
    let head = tokio::spawn(async move {
        s0.get_activities(&test_session(), &ActivityParams::default())
            .await
    });
    time::sleep(Duration::from_millis(20)).await;

    let completion_order = Arc::new(Mutex::new(Vec::<u32>::new()));
    let mut handles = Vec::new();
    for idx in 1_u32..=3 {
        let s = Arc::clone(&scraper);
        let order = Arc::clone(&completion_order);
        let launched = tokio::spawn(async move {
            s.get_activities(&test_session(), &ActivityParams::default())
                .await
                .expect("ok");
            order.lock().await.push(idx);
        });
        handles.push(launched);
        // Enforce enqueue ordering deterministically.
        time::sleep(Duration::from_millis(15)).await;
    }

    // Release the head; each notify_one frees one waiter.
    gate.notify_one();
    head.await.expect("head joined").expect("ok");
    for _ in 0..3 {
        gate.notify_one();
        time::sleep(Duration::from_millis(20)).await;
    }
    for h in handles {
        h.await.expect("waiter joined");
    }

    let order = completion_order.lock().await.clone();
    assert_eq!(order, vec![1, 2, 3], "FIFO ordering violated: {order:?}");
}

#[tokio::test]
async fn credential_login_parks_permit_on_otp_required() {
    let limiter = SciotteLimiter::new(test_config(1, 16));
    let inner = FakeScriptedScraper::new(Duration::from_millis(5));
    inner
        .script(vec![ScriptedLogin::OtpRequired, ScriptedLogin::Success])
        .await;
    let scraper = QueuedScraper::new(inner, Arc::clone(&limiter));

    // First call returns OtpRequired (parks permit).
    let first = scraper
        .credential_login("a@b.c", "pw", "email")
        .await
        .expect("ok");
    assert!(matches!(first, LoginResult::OtpRequired));
    assert_eq!(
        limiter.available_permits(),
        0,
        "permit must remain parked after OtpRequired"
    );

    // Follow-up submit_otp uses the parked permit (no new acquire needed)
    // and completes with Success, releasing it.
    let second = scraper.submit_otp("123456").await.expect("ok");
    assert!(matches!(second, LoginResult::Success(_)));
    assert_eq!(
        limiter.available_permits(),
        1,
        "permit must be released on terminal outcome"
    );
}

#[tokio::test]
async fn failed_login_releases_permit_immediately() {
    let limiter = SciotteLimiter::new(test_config(1, 16));
    let inner = FakeScriptedScraper::new(Duration::from_millis(5));
    inner.script(vec![ScriptedLogin::Failed]).await;
    let scraper = QueuedScraper::new(inner, Arc::clone(&limiter));

    let result = scraper
        .credential_login("a", "b", "email")
        .await
        .expect("ok");
    assert!(matches!(result, LoginResult::Failed(_)));
    assert_eq!(limiter.available_permits(), 1);
    assert!(limiter.take_parked().await.is_none());
}

#[tokio::test]
async fn watchdog_evicts_stale_parked_permit() {
    let limiter = SciotteLimiter::new(test_config(1, 16));
    let inner = FakeScriptedScraper::new(Duration::from_millis(5));
    inner.script(vec![ScriptedLogin::TwoFactorChoice]).await;
    let scraper = QueuedScraper::new(inner, Arc::clone(&limiter));

    let watchdog = limiter.spawn_watchdog();

    scraper
        .credential_login("a", "b", "email")
        .await
        .expect("ok");
    assert_eq!(limiter.available_permits(), 0);

    // Wait long enough for the watchdog to observe the stale slot.
    time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        limiter.available_permits(),
        1,
        "watchdog must free the stale parked permit"
    );
    watchdog.abort();
}

#[test]
fn limiter_error_carries_configured_retry_after_hint() {
    // Each error variant embeds the caller-supplied retry_after Duration;
    // there is no crate-level default. The HTTP layer reads `retry_after_secs()`
    // which simply returns the embedded value.
    let err = LimiterError::QueueFull {
        depth: 9,
        max: 8,
        retry_after: Duration::from_secs(7),
    };
    assert_eq!(err.retry_after_secs(), 7);

    let err = LimiterError::AcquireTimeout {
        timeout: Duration::from_secs(3),
        retry_after: Duration::from_secs(4),
    };
    assert_eq!(err.retry_after_secs(), 4);

    let err = LimiterError::NoCapacity {
        retry_after: Duration::from_secs(2),
    };
    assert_eq!(err.retry_after_secs(), 2);

    let err = LimiterError::Closed {
        retry_after: Duration::from_secs(60),
    };
    assert_eq!(err.retry_after_secs(), 60);
}

#[test]
fn queue_config_error_shapes_are_user_readable() {
    // `QueueConfig::from_env()` itself is exercised at binary startup, not in
    // integration tests — mutating the process environment from a test is
    // racy under `cargo test`'s parallel harness. This test instead asserts
    // the error display shapes, which is what operators actually see when a
    // container boots with bad config.
    use dravr_sciotte::queue::{QueueConfigError, ENV_MAX_CONCURRENT, ENV_MAX_QUEUE_DEPTH};

    let missing = QueueConfigError::Missing(ENV_MAX_CONCURRENT);
    assert!(missing.to_string().contains(ENV_MAX_CONCURRENT));

    let zero = QueueConfigError::ZeroValue(ENV_MAX_CONCURRENT);
    assert!(zero.to_string().contains(ENV_MAX_CONCURRENT));
    assert!(zero.to_string().contains("greater than zero"));

    let inverted = QueueConfigError::QueueSmallerThanConcurrency {
        concurrent: 4,
        queue: 2,
    };
    assert!(inverted.to_string().contains("max_queue_depth (2)"));
    assert!(inverted.to_string().contains("max_concurrent (4)"));

    // Bad parse: let the library do the work through a synthetic bad value.
    let _ = ENV_MAX_QUEUE_DEPTH;
}

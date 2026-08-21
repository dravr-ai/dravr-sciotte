// ABOUTME: Regression tests for the identity-token gate: anonymous callers 401 on every
// ABOUTME: protected route, /health stays open, and only a loopback bind serves ungated
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::queue::{QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::state::{AppScraper, ServerState};
use dravr_sciotte_server::auth::AUDIENCE_ENV;
use dravr_sciotte_server::router::{build_router, is_loopback_host, router_for_bind};
use serial_test::serial;
use tower::ServiceExt;

/// Build a minimal, browser-lazy state for router wiring tests. `ChromeScraper::new`
/// allocates no browser until a scrape runs, so no tool ever executes here — the auth
/// layer rejects before any handler.
fn test_state() -> Arc<ServerState> {
    let limiter = SciotteLimiter::new(QueueConfig {
        max_concurrent: 1,
        max_queue_depth: 1,
        acquire_timeout: Duration::from_millis(200),
        parked_permit_ttl: Duration::from_millis(200),
        watchdog_interval: Duration::from_millis(50),
        retry_after_hint: Duration::from_millis(500),
        closed_retry_after: Duration::from_secs(1),
    });
    let provider = ProviderConfig::strava_default().expect("embedded strava provider config");
    let chrome = ChromeScraper::new(ScraperConfig::default(), provider.clone());
    let queued = QueuedScraper::new(chrome, Arc::clone(&limiter));
    let cached: AppScraper = CachedScraper::new(queued, &CacheConfig::default());
    Arc::new(ServerState::new(
        vec![(provider, cached)],
        limiter,
        None,
        ScraperConfig::default(),
    ))
}

/// Every protected route must refuse a caller with no identity token, and
/// `/health` must stay open for Cloud Run's probes.
///
/// `/browser/login` is named explicitly because it was the one that got this
/// wrong: it used to be merged without the auth layer, so the WebSocket driving
/// a provider browser login answered unauthenticated on a public URL. Auth is
/// now a single layer over everything except health, so this asserts the
/// property that made that possible is gone.
#[tokio::test]
#[serial]
async fn build_router_refuses_anonymous_on_every_protected_route() {
    // Safe: edition 2021 set_var; this key is only read at router construction.
    env::set_var(AUDIENCE_ENV, "dravr-sciotte-test-audience");

    for (method, path) in [
        (Method::POST, "/mcp"),
        (Method::GET, "/api/athlete"),
        (Method::GET, "/auth/sessions"),
        (Method::GET, "/auth/sessions/abc/export"),
        (Method::GET, "/browser/login"),
        (Method::GET, "/debug/list-page-probe"),
    ] {
        let app = build_router(&test_state()).expect("audience is set, so the router builds");
        let response = app
            .oneshot(
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} answered {} to an anonymous caller, expected 401",
            response.status()
        );
    }

    env::remove_var(AUDIENCE_ENV);
}

/// `/health` stays open, because Cloud Run's startup and liveness probes carry
/// no credentials. If this ever starts requiring a token the container boots and
/// then fails its probe, which presents as a deploy that rolls back for no
/// visible reason.
#[tokio::test]
#[serial]
async fn health_stays_open_for_probes() {
    env::set_var(AUDIENCE_ENV, "dravr-sciotte-test-audience");

    let app = build_router(&test_state()).expect("router builds");
    let health = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(health.status(), StatusCode::OK, "/health must stay open");

    env::remove_var(AUDIENCE_ENV);
}

/// Without an audience the server must not start at all.
///
/// A scraper that does not know which audience to require would accept tokens
/// minted for any other Google service. Refusing to build is the whole point of
/// leaving a fail-open shared key behind.
#[tokio::test]
#[serial]
async fn build_router_fails_closed_without_an_audience() {
    env::remove_var(AUDIENCE_ENV);
    assert!(
        build_router(&test_state()).is_err(),
        "router must refuse to build when the audience is unset"
    );
}

/// The serving policy keys on the bind address: a deployed bind (`0.0.0.0`,
/// the Dockerfile CMD) keeps the token gate — or refuses to start without an
/// audience — while a loopback bind serves ungated, which is the development
/// mode behind `scripts/sciotte-local.sh` where no metadata server exists to
/// mint tokens.
#[tokio::test]
#[serial]
async fn router_for_bind_gates_by_bind_address() {
    // Deployed bind + audience → the gate stands: an anonymous caller 401s.
    env::set_var(AUDIENCE_ENV, "dravr-sciotte-test-audience");
    let gated = router_for_bind("0.0.0.0", &test_state())
        .expect("audience is set, so the gated router builds");
    let response = gated
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/athlete")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a non-loopback bind must keep the identity-token gate"
    );

    // Deployed bind without an audience → refuses to start, same as build_router.
    env::remove_var(AUDIENCE_ENV);
    assert!(
        router_for_bind("0.0.0.0", &test_state()).is_err(),
        "a non-loopback bind without an audience must fail closed"
    );

    // Loopback bind, no audience → serves, and an anonymous request reaches
    // the handler rather than a gate: the empty session list answers 200.
    let dev =
        router_for_bind("127.0.0.1", &test_state()).expect("a loopback bind needs no audience");
    let response = dev
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/auth/sessions")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a loopback bind serves ungated for local development"
    );
}

/// The loopback decision must hold for every loopback literal and reject
/// everything routable — a wrong answer here either breaks local development
/// or, far worse, serves the ungated router on a reachable interface.
#[test]
fn loopback_host_classification() {
    for host in ["127.0.0.1", "127.0.0.53", "::1", "localhost"] {
        assert!(is_loopback_host(host), "{host} is loopback");
    }
    for host in [
        "0.0.0.0",
        "::",
        "10.0.0.7",
        "192.168.1.20",
        "sciotte.example.internal",
    ] {
        assert!(!is_loopback_host(host), "{host} is not loopback");
    }
}

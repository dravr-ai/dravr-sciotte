// ABOUTME: Regression test — the MCP transport honors the DRAVR_SCIOTTE_API_KEY gate
// ABOUTME: unauth POST /mcp -> 401, /health stays open, a correct bearer passes (no auth bypass)
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
use dravr_sciotte_server::router::build_router;
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
    let chrome = ChromeScraper::new(ScraperConfig::default(), provider);
    let queued = QueuedScraper::new(chrome, Arc::clone(&limiter));
    let cached: AppScraper = CachedScraper::new(queued, &CacheConfig::default());
    Arc::new(ServerState::new(cached, limiter))
}

/// The MCP transport must honor the same `DRAVR_SCIOTTE_API_KEY` gate as the REST API:
/// an unauthenticated `POST /mcp` is rejected with 401 when the key is set, `/health`
/// stays open, and a correct bearer token passes the gate.
#[tokio::test]
async fn mcp_route_enforces_api_key_gate() {
    const KEY: &str = "router-mcp-auth-test-key";
    // Safe: edition 2021 set_var; single test mutates this key, removed below.
    env::set_var("DRAVR_SCIOTTE_API_KEY", KEY);

    // Unauthenticated POST /mcp -> 401 (the gate that protects /api/*).
    let app = build_router(test_state());
    let unauth = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        unauth.status(),
        StatusCode::UNAUTHORIZED,
        "unauthenticated /mcp must be rejected"
    );

    // /health stays open even with the key set.
    let app = build_router(test_state());
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

    // A correct bearer token passes the gate (status is not 401).
    let app = build_router(test_state());
    let authed = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/mcp")
                .header("authorization", format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_ne!(
        authed.status(),
        StatusCode::UNAUTHORIZED,
        "correct bearer token must pass the gate"
    );

    env::remove_var("DRAVR_SCIOTTE_API_KEY");
}

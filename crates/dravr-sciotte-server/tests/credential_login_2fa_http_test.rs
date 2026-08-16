// ABOUTME: Drives the real 2FA login chain over the server's HTTP surface, end to end
// ABOUTME: Chrome runs for real against the built-in fixtures — no network, no credentials
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The scraper's own tests cover `credential_login` as a Rust call, and
//! `multi_provider_routing_test` covers routing and flow parking with no scraper run.
//! Nothing joined the two: no test drove a real scrape *through* the HTTP surface and
//! asserted the JSON the platform actually consumes.
//!
//! That gap is why two 2FA regressions were caught by hand rather than by CI. Both fixes
//! (2026-08-14) turned on which page the poll loop was allowed to read, and the only
//! evidence they worked end to end came from a human running curl against a local
//! server. This test is that run, automated.
//!
//! `fake_login` serves the same fixtures the scraper's tests use, from inside the
//! process, so this needs Chrome but no network and no credentials.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::queue::{QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::state::{AppScraper, ServerState};
use dravr_sciotte_server::router::build_router;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Timeouts are generous ceilings, not expected durations: the fixture flow returns as
/// soon as the expected state is detected, so a passing run never pays them. The
/// headroom is for CI runners whose headless Chrome is roughly 3x slower than a laptop.
fn fixture_scraper_config() -> ScraperConfig {
    ScraperConfig {
        // 3, not 1: this drives the OAuth hop (Strava -> Google identifier -> password),
        // and each hop is a window.location navigation the scraper waits out before it
        // looks for the next field. The scraper's own fixture tests can use 1 because
        // they point a custom provider straight at the fixture server; going through
        // strava_default() means paying the real hop count.
        page_load_wait_secs: 3,
        form_interaction_delay_ms: 100,
        email_step_timeout_secs: 30,
        password_step_timeout_secs: 30,
        login_timeout_secs: 120,
        login_poll_interval_ms: 200,
        phone_tap_timeout_secs: 20,
        credential_login_headless: true,
        // Serves the bundled google/strava/garmin fixtures from inside the process.
        fake_login: true,
        ..ScraperConfig::default()
    }
}

/// Clear the API key so the auth middleware lets these requests through.
///
/// Everything else the login needs now rides in the `ScraperConfig` handed to
/// `ServerState::new` — the fixtures included. Before that config was honoured this
/// helper also had to export `DRAVR_SCIOTTE_FAKE_LOGIN` and the step timeouts, because
/// the login path rebuilt its scraper from the environment and silently discarded the
/// caller's settings.
fn configure_login_env() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
}

/// Two concurrent permits, because a 2FA login *parks* holding its permit while it waits
/// for the code or the method choice. With one permit the second case below would queue
/// behind the first flow's park and time out — correct backpressure, useless as a test.
fn fixture_state() -> Arc<ServerState> {
    let limiter = SciotteLimiter::new(QueueConfig {
        max_concurrent: 2,
        max_queue_depth: 4,
        acquire_timeout: Duration::from_secs(30),
        parked_permit_ttl: Duration::from_mins(2),
        watchdog_interval: Duration::from_secs(10),
        retry_after_hint: Duration::from_secs(1),
        closed_retry_after: Duration::from_secs(5),
    });
    let pairs = [
        ProviderConfig::strava_default().expect("embedded strava provider config"),
        ProviderConfig::garmin_default().expect("embedded garmin provider config"),
    ]
    .into_iter()
    .map(|provider| {
        let chrome = ChromeScraper::new(fixture_scraper_config(), provider.clone());
        let queued = QueuedScraper::new(chrome, Arc::clone(&limiter));
        let cached: AppScraper = CachedScraper::new(queued, &CacheConfig::default());
        (provider, cached)
    })
    .collect();
    Arc::new(ServerState::new(
        pairs,
        limiter,
        None,
        fixture_scraper_config(),
    ))
}

async fn body_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Status and body together. Asserting the status alone throws away the only thing that
/// explains a 5xx, which turns a diagnosable failure into the number 500.
async fn split(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    (status, body_json(response).await)
}

fn post(uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Password submit landing on the 2FA chooser, then picking the phone-tap method.
///
/// Pins the whole chain the platform's login modal walks: `two_factor_choice` carrying
/// the provider's own option ids, then `number_match` carrying the digits the user
/// confirms on their device.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chooser_then_phone_tap_reports_the_number_over_http() {
    configure_login_env();
    let app = build_router(fixture_state());

    let login = app
        .clone()
        .oneshot(post(
            "/auth/login-with-credentials",
            &json!({"email": "test@example.com", "password": "2fa-password",
                    "method": "google", "provider": "strava"}),
        ))
        .await
        .unwrap();
    let (status, body) = split(login).await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    assert_eq!(
        body["status"], "two_factor_choice",
        "a chooser page must be reported as a choice, not polled to a timeout: {body}"
    );

    let ids: Vec<&str> = body["options"]
        .as_array()
        .expect("options is an array")
        .iter()
        .filter_map(|o| o["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["app", "otp"],
        "the provider's own option ids must pass through verbatim: {body}"
    );

    let flow_id = body["flow_id"]
        .as_str()
        .expect("a parked flow id")
        .to_owned();

    let selected = app
        .oneshot(post(
            "/auth/select-2fa",
            &json!({"option_id": "app", "flow_id": flow_id}),
        ))
        .await
        .unwrap();
    let (status, body) = split(selected).await;
    assert_eq!(status, StatusCode::OK, "select-2fa failed: {body}");
    assert_eq!(body["status"], "number_match", "got {body}");
    assert_eq!(
        body["number"], "78",
        "the number must be scraped from the page, not fabricated: {body}"
    );
}

/// Password submit landing straight on a TOTP page, with no chooser in between.
///
/// This is the path where the poll can begin *after* the navigation has already landed,
/// making the OTP page the initial URL. Reporting it requires reading a page the loop
/// started on rather than waiting for the URL to change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_totp_reports_otp_required_over_http() {
    configure_login_env();
    let app = build_router(fixture_state());

    let login = app
        .oneshot(post(
            "/auth/login-with-credentials",
            &json!({"email": "test@example.com", "password": "totp-password",
                    "method": "google", "provider": "strava"}),
        ))
        .await
        .unwrap();
    let (status, body) = split(login).await;
    assert_eq!(status, StatusCode::OK, "login failed: {body}");
    assert_eq!(
        body["status"], "otp_required",
        "a TOTP page must be reported, whether or not the poll won the navigation race: {body}"
    );
    assert!(
        body["flow_id"].as_str().is_some_and(|f| !f.is_empty()),
        "the flow must be parked so the code can resume it: {body}"
    );
}

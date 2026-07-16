// ABOUTME: Multi-provider server tests — provider-tagged sessions route to the right scraper,
// ABOUTME: import/export carry the provider, and flow_id login parking resolves/rejects correctly
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use chrono::Utc;
use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::queue::{QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::state::{AppScraper, FlowLookupError, ServerState};
use dravr_sciotte_server::router::build_router;
use serde_json::{json, Value};
use tower::ServiceExt;

/// Build a browser-lazy two-provider state (strava + garmin). `ChromeScraper::new`
/// allocates no browser until a scrape runs, so no Chrome ever launches here.
fn two_provider_state() -> Arc<ServerState> {
    let limiter = SciotteLimiter::new(QueueConfig {
        max_concurrent: 2,
        max_queue_depth: 4,
        acquire_timeout: Duration::from_millis(200),
        parked_permit_ttl: Duration::from_millis(200),
        watchdog_interval: Duration::from_millis(50),
        retry_after_hint: Duration::from_millis(500),
        closed_retry_after: Duration::from_secs(1),
    });
    let pairs = [
        ProviderConfig::strava_default().expect("embedded strava provider config"),
        ProviderConfig::garmin_default().expect("embedded garmin provider config"),
    ]
    .into_iter()
    .map(|provider| {
        let chrome = ChromeScraper::new(ScraperConfig::default(), provider.clone());
        let queued = QueuedScraper::new(chrome, Arc::clone(&limiter));
        let cached: AppScraper = CachedScraper::new(queued, &CacheConfig::default());
        (provider, cached)
    })
    .collect();
    Arc::new(ServerState::new(pairs, limiter, None))
}

fn test_session(id: &str) -> AuthSession {
    AuthSession {
        session_id: id.to_owned(),
        cookies: vec![],
        created_at: Utc::now(),
        expires_at: None,
    }
}

async fn body_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Import tags the session with its provider; export returns the symmetric
/// `{provider, session}` shape the platform persists as session-of-record.
#[tokio::test]
async fn import_export_roundtrip_carries_provider() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
    let app = build_router(two_provider_state());

    let import = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/auth/import-session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"provider": "garmin", "session": test_session("sess-garmin-1")})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(import.status(), StatusCode::OK);
    let imported = body_json(import).await;
    assert_eq!(imported["status"], "imported");
    assert_eq!(imported["provider"], "garmin");
    assert_eq!(imported["session_id"], "sess-garmin-1");

    let export = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/auth/sessions/sess-garmin-1/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(export.status(), StatusCode::OK);
    let exported = body_json(export).await;
    assert_eq!(exported["provider"], "garmin");
    assert_eq!(exported["session"]["session_id"], "sess-garmin-1");
}

/// Importing a session for a provider this instance does not serve is a 400
/// naming the served providers — never a silent mis-route.
#[tokio::test]
async fn import_unknown_provider_rejected_with_available_list() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
    let app = build_router(two_provider_state());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/auth/import-session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"provider": "polar", "session": test_session("sess-x")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"], "unknown_provider");
    assert_eq!(body["available"], json!(["garmin", "strava"]));
}

/// A credential login on a multi-provider instance must name the provider —
/// the reject happens before any permit or Chrome launch.
#[tokio::test]
async fn credential_login_requires_provider_when_multi() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
    let app = build_router(two_provider_state());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/auth/login-with-credentials")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email": "x@y.z", "password": "pw"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = body_json(response).await;
    assert_eq!(body["error"], "provider_required");
    assert_eq!(body["available"], json!(["garmin", "strava"]));
}

/// An OTP submission with no pending login flow is a 404, not a hang or panic.
#[tokio::test]
async fn submit_otp_without_pending_flow_is_404() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
    let app = build_router(two_provider_state());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/auth/submit-otp")
                .header("content-type", "application/json")
                .body(Body::from(json!({"code": "123456"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = body_json(response).await;
    assert_eq!(body["error"], "no_pending_login");
}

/// /health reports the served providers and per-provider cache stats.
#[tokio::test]
async fn health_reports_providers_and_per_provider_cache() {
    env::remove_var("DRAVR_SCIOTTE_API_KEY");
    let app = build_router(two_provider_state());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["providers"], json!(["garmin", "strava"]));
    assert!(body["cache"]["garmin"].is_object());
    assert!(body["cache"]["strava"].is_object());
    assert_eq!(body["pending_logins"], 0);
}

/// Flow parking: the sole pending flow resolves without an id, two pending
/// flows demand one, and a named flow resolves among several.
#[tokio::test]
async fn login_flow_lookup_sole_named_and_ambiguous() {
    let state = two_provider_state();
    let garmin_cfg = ProviderConfig::garmin_default().unwrap();
    let strava_cfg = ProviderConfig::strava_default().unwrap();

    // Sole pending flow resolves with no id.
    let permit = state.limiter().acquire().await.unwrap();
    state
        .park_login_flow(
            "flow-a".to_owned(),
            ChromeScraper::new(ScraperConfig::default(), garmin_cfg.clone()),
            "garmin".to_owned(),
            permit,
        )
        .await;
    let (id, flow) = state.take_login_flow(None).await.unwrap();
    assert_eq!(id, "flow-a");
    assert_eq!(flow.provider, "garmin");
    assert_eq!(state.login_flow_count().await, 0);
    // Release the taken flow's permit before parking two more (max_concurrent=2).
    drop(flow);

    // Two pending flows: no id is ambiguous, a named id resolves.
    let permit_a = state.limiter().acquire().await.unwrap();
    let permit_b = state.limiter().acquire().await.unwrap();
    state
        .park_login_flow(
            "flow-a".to_owned(),
            ChromeScraper::new(ScraperConfig::default(), garmin_cfg),
            "garmin".to_owned(),
            permit_a,
        )
        .await;
    state
        .park_login_flow(
            "flow-b".to_owned(),
            ChromeScraper::new(ScraperConfig::default(), strava_cfg),
            "strava".to_owned(),
            permit_b,
        )
        .await;
    assert!(matches!(
        state.take_login_flow(None).await,
        Err(FlowLookupError::Ambiguous)
    ));
    let (id, flow) = state.take_login_flow(Some("flow-b")).await.unwrap();
    assert_eq!(id, "flow-b");
    assert_eq!(flow.provider, "strava");
    assert!(matches!(
        state.take_login_flow(Some("flow-zzz")).await,
        Err(FlowLookupError::NotFound)
    ));
}

/// The reaper evicts stale flows (freeing browser + permit) and leaves fresh ones.
#[tokio::test]
async fn stale_login_flows_are_evicted() {
    let state = two_provider_state();
    let cfg = ProviderConfig::garmin_default().unwrap();

    let permit = state.limiter().acquire().await.unwrap();
    state
        .park_login_flow(
            "flow-stale".to_owned(),
            ChromeScraper::new(ScraperConfig::default(), cfg),
            "garmin".to_owned(),
            permit,
        )
        .await;
    assert_eq!(state.login_flow_count().await, 1);

    // A generous TTL keeps the fresh flow…
    assert_eq!(
        state.evict_stale_login_flows(Duration::from_hours(1)).await,
        0
    );
    assert_eq!(state.login_flow_count().await, 1);

    // …a zero TTL reaps it, freeing its permit back to the limiter.
    let before = state.limiter().available_permits();
    assert_eq!(state.evict_stale_login_flows(Duration::ZERO).await, 1);
    assert_eq!(state.login_flow_count().await, 0);
    assert_eq!(state.limiter().available_permits(), before + 1);
}

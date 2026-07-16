// ABOUTME: Axum router wiring REST, WebSocket streaming, and MCP HTTP transport
// ABOUTME: Multi-session support via X-Session-Id header, CORS, and session management endpoints
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::DateTime;
use dravr_sciotte::auth;
use dravr_sciotte::models::{ActivityParams, AuthSession, HealthParams};
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::state::{SessionEntry, SharedState};
use dravr_tronc::mcp::transport::http;
use dravr_tronc::McpServer;
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use crate::auth::auth_middleware;
use crate::error_response::scraper_error_response;
use crate::health::health_handler;
use crate::streaming;

/// Build the complete Axum router for the unified server
pub fn build_router(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers(Any);

    let mcp_server = Arc::new(McpServer::new(
        "dravr-sciotte-mcp",
        env!("CARGO_PKG_VERSION"),
        dravr_sciotte_mcp::build_tool_registry(),
        Arc::clone(&state),
    ));
    // Gate the MCP transport behind the same `DRAVR_SCIOTTE_API_KEY` bearer
    // check as the REST API. The MCP tools scrape the same authenticated
    // provider data as `/api/*`, so leaving `/mcp` unauthenticated would be an
    // auth bypass around the REST gate. When the env var is unset the middleware
    // passes through (localhost development mode), matching REST behavior.
    let mcp_router = http::mcp_router(mcp_server).layer(middleware::from_fn(auth_middleware));

    let api_routes = Router::new()
        .route("/auth/login", post(login_handler))
        .route("/auth/status", get(auth_status_handler))
        .route("/auth/sessions", get(list_sessions_handler))
        .route("/auth/sessions/{id}", delete(delete_session_handler))
        // Session-of-record boundary (ADR-021): export hands the full AuthSession
        // to the platform to persist; import re-hydrates this transient store from
        // the platform's durable copy after a scale-to-zero / redeploy.
        .route("/auth/sessions/{id}/export", get(export_session_handler))
        .route("/auth/import-session", post(import_session_handler))
        .route(
            "/auth/login-with-credentials",
            post(streaming::credential_login),
        )
        .route("/auth/submit-otp", post(streaming::submit_otp))
        .route("/auth/select-2fa", post(streaming::select_two_factor))
        .route("/api/athlete", get(athlete_handler))
        .route("/api/activities", get(activities_handler))
        .route("/api/activities/{id}", get(activity_detail_handler))
        .route("/api/daily-summary", get(daily_summary_handler))
        .route("/debug/list-page-probe", get(list_page_probe_handler))
        .layer(middleware::from_fn(auth_middleware))
        .with_state(state.clone());

    let browser_route = Router::new()
        .route("/browser/login", get(streaming::browser_login_ws))
        .with_state(state.clone());

    let health_route = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    Router::new()
        .merge(api_routes)
        .merge(browser_route)
        .merge(mcp_router)
        .merge(health_route)
        .layer(cors)
}

// ============================================================================
// Session resolution helpers
// ============================================================================

/// Extract session ID from the `X-Session-Id` header, falling back to the latest session
fn resolve_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

/// Resolve the request's session entry (session + its provider): by
/// `X-Session-Id` when given, else the latest session.
async fn resolve_session_entry(state: &SharedState, headers: &HeaderMap) -> Option<SessionEntry> {
    match resolve_session_id(headers) {
        Some(id) => state.get_session_entry(&id).await,
        None => state.session_entry().await,
    }
}

/// 401 for a request with no resolvable session.
fn session_not_found_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "session_not_found", "message": "Provide X-Session-Id header or login first."})),
    )
        .into_response()
}

/// 500 for a session whose provider is not served by this instance — only
/// possible when an import named a provider this instance doesn't run.
fn provider_not_served_response(provider: &str, state: &SharedState) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": "provider_not_served",
            "provider": provider,
            "available": state.provider_names(),
        })),
    )
        .into_response()
}

/// Query naming a provider for endpoints that act before any session exists.
#[derive(Deserialize, Default)]
struct ProviderQuery {
    provider: Option<String>,
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// POST /auth/login — launch browser for user login (`?provider=` picks the
/// provider; optional when this instance serves exactly one)
async fn login_handler(
    State(state): State<SharedState>,
    Query(query): Query<ProviderQuery>,
) -> impl IntoResponse {
    let provider = match state.resolve_provider(query.provider.as_deref()) {
        Ok(name) => name,
        Err(err) => return streaming::provider_resolve_response(&state, &err),
    };
    let Some(scraper) = state.scraper_for(&provider) else {
        return provider_not_served_response(&provider, &state);
    };

    let session = match scraper.browser_login().await {
        Ok(s) => s,
        Err(e) => return scraper_error_response(&e),
    };

    if state.single_provider() {
        if let Err(e) = auth::save_session(&session).await {
            tracing::warn!(error = %e, "Failed to persist session to disk");
        }
    }

    let session_id = session.session_id.clone();
    state.add_session(session, provider.clone()).await;

    info!(provider = %provider, "Login successful, session established");
    Json(json!({
        "status": "authenticated",
        "session_id": session_id,
        "provider": provider,
    }))
    .into_response()
}

/// GET /auth/status — check authentication status (supports `X-Session-Id` header)
async fn auth_status_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(entry) = resolve_session_entry(&state, &headers).await {
        let Some(scraper) = state.scraper_for(&entry.provider) else {
            return provider_not_served_response(&entry.provider, &state);
        };
        let authenticated = scraper.is_authenticated(&entry.session).await;
        Json(json!({
            "authenticated": authenticated,
            "session_id": entry.session.session_id,
            "provider": entry.provider,
            "created_at": entry.session.created_at.to_rfc3339(),
        }))
        .into_response()
    } else {
        Json(json!({
            "authenticated": false,
            "message": "No active session. POST /auth/login or connect to /browser/login."
        }))
        .into_response()
    }
}

/// GET /auth/sessions — list all active sessions with their providers
async fn list_sessions_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let sessions = state.list_sessions().await;
    Json(json!({
        "count": sessions.len(),
        "sessions": sessions.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        "providers": sessions
            .iter()
            .map(|(id, provider)| json!({"session_id": id, "provider": provider}))
            .collect::<Vec<_>>(),
    }))
}

/// DELETE /auth/sessions/:id — remove a specific session
async fn delete_session_handler(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    if state.remove_session(&session_id).await.is_some() {
        Json(json!({"status": "removed", "session_id": session_id}))
    } else {
        Json(json!({"error": "session_not_found", "session_id": session_id}))
    }
}

/// GET /auth/sessions/:id/export — return the full [`AuthSession`] (cookies) so a
/// platform caller can persist it as the durable session-of-record.
///
/// This server holds sessions only transiently (in-memory + ephemeral Cloud Run
/// disk); the platform's encrypted store is authoritative across restarts and
/// redeploys. After a credential login the platform exports the session here and
/// persists it, then re-hydrates via `/auth/import-session` when this instance no
/// longer has it (ADR-021 session-of-record boundary).
async fn export_session_handler(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    state.get_session_entry(&session_id).await.map_or_else(
        || {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "session_not_found", "session_id": session_id})),
            )
                .into_response()
        },
        |entry| {
            Json(json!({
                "provider": entry.provider,
                "session": entry.session,
            }))
            .into_response()
        },
    )
}

/// Body for `/auth/import-session`: the session plus the provider it belongs
/// to, so scrapes route to the right provider's scraper (symmetric with the
/// export shape).
#[derive(Deserialize)]
struct ImportSessionRequest {
    provider: String,
    session: AuthSession,
}

/// POST /auth/import-session — re-hydrate the in-memory store from a
/// platform-supplied [`AuthSession`] tagged with its provider.
///
/// Lets a subsequent scrape run after this instance has scaled to zero or been
/// redeployed: the platform re-passes the durable session it persisted at login,
/// and the returned `session_id` is used on the `X-Session-Id` header for the
/// following `/api/*` call. Idempotent — a repeat import of the same session id
/// simply overwrites the stored copy.
async fn import_session_handler(
    State(state): State<SharedState>,
    Json(request): Json<ImportSessionRequest>,
) -> impl IntoResponse {
    if state.scraper_for(&request.provider).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unknown_provider",
                "provider": request.provider,
                "available": state.provider_names(),
            })),
        )
            .into_response();
    }
    let session_id = request.session.session_id.clone();
    state.add_session(request.session, request.provider.clone()).await;
    info!(session_id = %session_id, provider = %request.provider, "Session imported from platform");
    Json(json!({"status": "imported", "session_id": session_id, "provider": request.provider}))
        .into_response()
}

// ============================================================================
// Activity Handlers
// ============================================================================

/// GET /api/athlete — get authenticated user's profile (supports `X-Session-Id` header)
async fn athlete_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(entry) = resolve_session_entry(&state, &headers).await else {
        return session_not_found_response();
    };
    let Some(scraper) = state.scraper_for(&entry.provider) else {
        return provider_not_served_response(&entry.provider, &state);
    };

    match scraper.get_athlete(&entry.session).await {
        Ok(profile) => Json(json!(profile)).into_response(),
        Err(e) => scraper_error_response(&e),
    }
}

#[derive(Deserialize, Default)]
struct ActivityQuery {
    limit: Option<u32>,
    sport_type: Option<String>,
    detail: Option<bool>,
    /// Only return activities after this Unix epoch (seconds) — historical window lower bound.
    after: Option<i64>,
    /// Only return activities before this Unix epoch (seconds) — historical window upper bound.
    before: Option<i64>,
}

/// GET /api/activities — list scraped activities (supports `X-Session-Id` header)
async fn activities_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<ActivityQuery>,
) -> impl IntoResponse {
    let Some(entry) = resolve_session_entry(&state, &headers).await else {
        return session_not_found_response();
    };
    let Some(scraper) = state.scraper_for(&entry.provider) else {
        return provider_not_served_response(&entry.provider, &state);
    };

    let params = ActivityParams {
        limit: query.limit,
        before: query.before.and_then(|ts| DateTime::from_timestamp(ts, 0)),
        after: query.after.and_then(|ts| DateTime::from_timestamp(ts, 0)),
        sport_type: query.sport_type,
        enrich_details: query.detail.unwrap_or(false),
        ..Default::default()
    };

    match scraper.get_activities(&entry.session, &params).await {
        Ok(activities) => Json(json!({
            "count": activities.len(),
            "activities": activities,
        }))
        .into_response(),
        Err(e) => scraper_error_response(&e),
    }
}

#[derive(Deserialize, Default)]
struct ActivityDetailQuery {
    /// When `true`, return the unparsed JSON the provider's `js_extract`
    /// produced — bypasses the typed `SciotteActivity` deserialization so
    /// callers see the provider's raw DTO shape (e.g. Garmin Connect's
    /// `lapDTOs` / `activitySplits` camelCase arrays). Debug use.
    #[serde(default)]
    raw: bool,
}

/// GET /api/activities/:id — get single activity detail (supports `X-Session-Id` header)
async fn activity_detail_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ActivityDetailQuery>,
) -> impl IntoResponse {
    let Some(entry) = resolve_session_entry(&state, &headers).await else {
        return session_not_found_response();
    };
    let Some(scraper) = state.scraper_for(&entry.provider) else {
        return provider_not_served_response(&entry.provider, &state);
    };

    if query.raw {
        return match scraper.get_activity_raw(&entry.session, &id).await {
            Ok(value) => Json(value).into_response(),
            Err(e) => scraper_error_response(&e),
        };
    }

    match scraper.get_activity(&entry.session, &id).await {
        Ok(activity) => Json(json!(activity)).into_response(),
        Err(e) => scraper_error_response(&e),
    }
}

// ============================================================================
// Health Summary Handler
// ============================================================================

#[derive(Deserialize)]
struct DailySummaryQuery {
    date: String,
}

/// GET /api/daily-summary?date=YYYY-MM-DD — get daily health/wellness summary
async fn daily_summary_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<DailySummaryQuery>,
) -> impl IntoResponse {
    let Some(entry) = resolve_session_entry(&state, &headers).await else {
        return session_not_found_response();
    };
    let Some(scraper) = state.scraper_for(&entry.provider) else {
        return provider_not_served_response(&entry.provider, &state);
    };

    let Ok(date) = chrono::NaiveDate::parse_from_str(&query.date, "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("Invalid date format '{}', expected YYYY-MM-DD", query.date)})),
        )
            .into_response();
    };

    let params = HealthParams { date };

    match scraper.get_daily_summary(&entry.session, &params).await {
        Ok(summary) => Json(json!(summary)).into_response(),
        Err(e) => scraper_error_response(&e),
    }
}

// ============================================================================
// Debug — list-page GPS probe
// ============================================================================

/// GET /debug/list-page-probe — dump where Strava embeds activity GPS
/// coordinates on the training-list and dashboard-feed pages, for the
/// session resolved from `X-Session-Id` (or the latest session).
async fn list_page_probe_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(entry) = resolve_session_entry(&state, &headers).await else {
        return session_not_found_response();
    };
    let Some(scraper) = state.scraper_for(&entry.provider) else {
        return provider_not_served_response(&entry.provider, &state);
    };

    match scraper.probe_list_page_for_gps(&entry.session).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => scraper_error_response(&e),
    }
}

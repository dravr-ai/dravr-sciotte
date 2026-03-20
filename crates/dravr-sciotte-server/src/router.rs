// ABOUTME: Axum router wiring REST, WebSocket streaming, and MCP HTTP transport
// ABOUTME: Multi-session support via X-Session-Id header, CORS, and session management endpoints
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::{Any, CorsLayer};
use tracing::info;

use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::state::SharedState;
use dravr_sciotte_mcp::McpServer;

use crate::auth::auth_middleware;
use crate::health::health_handler;
use crate::streaming;

/// Build the complete Axum router for the unified server
pub fn build_router(state: SharedState, mcp_server: Arc<McpServer>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers(Any);

    let api_routes = Router::new()
        .route("/auth/login", post(login_handler))
        .route("/auth/status", get(auth_status_handler))
        .route("/auth/sessions", get(list_sessions_handler))
        .route("/auth/sessions/{id}", delete(delete_session_handler))
        .route(
            "/auth/login-with-credentials",
            post(streaming::credential_login),
        )
        .route("/auth/submit-otp", post(streaming::submit_otp))
        .route("/auth/select-2fa", post(streaming::select_two_factor))
        .route("/api/activities", get(activities_handler))
        .route("/api/activities/{id}", get(activity_detail_handler))
        .layer(middleware::from_fn(auth_middleware))
        .with_state(state.clone());

    let browser_route = Router::new()
        .route("/browser/login", get(streaming::browser_login_ws))
        .with_state(state.clone());

    let mcp_route = Router::new()
        .route(
            "/mcp",
            post(dravr_sciotte_mcp::transport::http::handle_mcp_post),
        )
        .with_state(mcp_server);

    let health_route = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    Router::new()
        .merge(api_routes)
        .merge(browser_route)
        .merge(mcp_route)
        .merge(health_route)
        .layer(cors)
}

// ============================================================================
// Session resolution helper
// ============================================================================

/// Extract session ID from the `X-Session-Id` header, falling back to the latest session
fn resolve_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// POST /auth/login — launch browser for user login
async fn login_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let session = {
        let guard = state.read().await;
        match guard.scraper().browser_login().await {
            Ok(s) => s,
            Err(e) => {
                return Json(json!({"error": format!("Login failed: {e}")})).into_response();
            }
        }
    };

    if let Err(e) = dravr_sciotte::auth::save_session(&session).await {
        tracing::warn!(error = %e, "Failed to persist session to disk");
    }

    let session_id = session.session_id.clone();
    state.write().await.add_session(session);

    info!("Login successful, session established");
    Json(json!({
        "status": "authenticated",
        "session_id": session_id,
    }))
    .into_response()
}

/// GET /auth/status — check authentication status (supports `X-Session-Id` header)
async fn auth_status_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let guard = state.read().await;

    let session =
        resolve_session_id(&headers).map_or_else(|| guard.session(), |id| guard.get_session(&id));

    if let Some(session) = session {
        let authenticated = guard.scraper().is_authenticated(session).await;
        Json(json!({
            "authenticated": authenticated,
            "session_id": session.session_id,
            "created_at": session.created_at.to_rfc3339(),
        }))
    } else {
        Json(json!({
            "authenticated": false,
            "message": "No active session. POST /auth/login or connect to /browser/login."
        }))
    }
}

/// GET /auth/sessions — list all active session IDs
async fn list_sessions_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let guard = state.read().await;
    let session_ids = guard.list_session_ids();
    Json(json!({
        "count": session_ids.len(),
        "sessions": session_ids,
    }))
}

/// DELETE /auth/sessions/:id — remove a specific session
async fn delete_session_handler(
    State(state): State<SharedState>,
    Path(session_id): Path<String>,
) -> impl IntoResponse {
    let mut guard = state.write().await;
    if guard.remove_session(&session_id).is_some() {
        Json(json!({"status": "removed", "session_id": session_id}))
    } else {
        Json(json!({"error": "session_not_found", "session_id": session_id}))
    }
}

// ============================================================================
// Activity Handlers
// ============================================================================

#[derive(Deserialize, Default)]
struct ActivityQuery {
    limit: Option<u32>,
    sport_type: Option<String>,
    detail: Option<bool>,
}

/// GET /api/activities — list scraped activities (supports `X-Session-Id` header)
async fn activities_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(query): Query<ActivityQuery>,
) -> impl IntoResponse {
    let guard = state.read().await;

    let session =
        resolve_session_id(&headers).map_or_else(|| guard.session(), |id| guard.get_session(&id));

    let Some(session) = session else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "session_not_found", "message": "Provide X-Session-Id header or login first."})),
        )
            .into_response();
    };

    let params = ActivityParams {
        limit: query.limit,
        sport_type: query.sport_type,
        enrich_details: query.detail.unwrap_or(false),
        ..Default::default()
    };

    match guard.scraper().get_activities(session, &params).await {
        Ok(activities) => Json(json!({
            "count": activities.len(),
            "activities": activities,
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Scraping failed: {e}")})),
        )
            .into_response(),
    }
}

/// GET /api/activities/:id — get single activity detail (supports `X-Session-Id` header)
async fn activity_detail_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let guard = state.read().await;

    let session =
        resolve_session_id(&headers).map_or_else(|| guard.session(), |sid| guard.get_session(&sid));

    let Some(session) = session else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "session_not_found"})),
        )
            .into_response();
    };

    match guard.scraper().get_activity(session, &id).await {
        Ok(activity) => Json(json!(activity)).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to get activity {id}: {e}")})),
        )
            .into_response(),
    }
}

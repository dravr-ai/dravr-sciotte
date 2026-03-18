// ABOUTME: Axum router wiring REST endpoints and MCP HTTP transport
// ABOUTME: Mounts auth, activity, health, and MCP routes with optional auth middleware
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::state::SharedState;
use dravr_sciotte_mcp::McpServer;

use crate::auth::auth_middleware;
use crate::health::health_handler;

/// Build the complete Axum router for the unified server
pub fn build_router(state: SharedState, mcp_server: Arc<McpServer>) -> Router {
    let api_routes = Router::new()
        .route("/auth/login", post(login_handler))
        .route("/auth/status", get(auth_status_handler))
        .route("/api/activities", get(activities_handler))
        .route("/api/activities/{id}", get(activity_detail_handler))
        .layer(middleware::from_fn(auth_middleware))
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
        .merge(mcp_route)
        .merge(health_route)
}

// ============================================================================
// Auth Handlers
// ============================================================================

/// POST /auth/login — launch browser for user to log in to Strava
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
    state.write().await.set_session(session);

    info!("Browser login successful, session established");
    Json(json!({
        "status": "authenticated",
        "session_id": session_id,
        "message": "Successfully logged in to Strava"
    }))
    .into_response()
}

/// GET /auth/status — check authentication status
async fn auth_status_handler(State(state): State<SharedState>) -> impl IntoResponse {
    let guard = state.read().await;
    if let Some(session) = guard.session() {
        let authenticated = guard.scraper().is_authenticated(session).await;
        Json(json!({
            "authenticated": authenticated,
            "session_id": session.session_id,
            "created_at": session.created_at.to_rfc3339(),
        }))
    } else {
        Json(json!({
            "authenticated": false,
            "message": "No active session. POST /auth/login to authenticate."
        }))
    }
}

// ============================================================================
// Activity Handlers
// ============================================================================

#[derive(Deserialize, Default)]
struct ActivityQuery {
    limit: Option<u32>,
    sport_type: Option<String>,
}

/// GET /api/activities — list scraped activities
async fn activities_handler(
    State(state): State<SharedState>,
    Query(query): Query<ActivityQuery>,
) -> impl IntoResponse {
    let guard = state.read().await;

    let Some(session) = guard.session() else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Not authenticated. POST /auth/login to start."})),
        )
            .into_response();
    };

    let params = ActivityParams {
        limit: query.limit,
        sport_type: query.sport_type,
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

/// GET /api/activities/:id — get single activity detail
async fn activity_detail_handler(
    State(state): State<SharedState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let guard = state.read().await;

    let Some(session) = guard.session() else {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Not authenticated"})),
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

// ABOUTME: Health check endpoint handler
// ABOUTME: Reports authentication status and cache statistics
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use dravr_sciotte_mcp::state::SharedState;

/// GET /health — server health check
pub async fn health_handler(State(state): State<SharedState>) -> Json<Value> {
    let state = state.read().await;
    let has_session = state.session().is_some();
    let cache_stats = state.scraper().stats();

    Json(json!({
        "status": "ok",
        "service": "dravr-sciotte",
        "authenticated": has_session,
        "cache": cache_stats,
    }))
}

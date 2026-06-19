// ABOUTME: Health check endpoint handler
// ABOUTME: Reports session count, cache statistics, and server status
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use dravr_sciotte_mcp::state::SharedState;

/// GET /health — server health check
pub async fn health_handler(State(state): State<SharedState>) -> Json<Value> {
    let session_count = state.session_count().await;
    let session_ids = state.list_session_ids().await;
    let cache_stats = state.scraper().stats();

    Json(json!({
        "status": "ok",
        "service": "dravr-sciotte",
        "sessions": {
            "count": session_count,
            "ids": session_ids,
        },
        "cache": cache_stats,
    }))
}

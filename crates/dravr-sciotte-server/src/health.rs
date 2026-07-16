// ABOUTME: Health check endpoint handler
// ABOUTME: Reports served providers, session count, per-provider cache stats, and pending logins
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use dravr_sciotte_mcp::state::SharedState;

/// GET /health — server health check
pub async fn health_handler(State(state): State<SharedState>) -> Json<Value> {
    let session_count = state.session_count().await;
    let session_ids = state.list_session_ids().await;
    let pending_logins = state.login_flow_count().await;
    let cache_stats: Map<String, Value> = state
        .scrapers()
        .map(|(name, scraper)| (name.to_owned(), json!(scraper.stats())))
        .collect();

    Json(json!({
        "status": "ok",
        "service": "dravr-sciotte",
        "providers": state.provider_names(),
        "sessions": {
            "count": session_count,
            "ids": session_ids,
        },
        "pending_logins": pending_logins,
        "cache": cache_stats,
    }))
}

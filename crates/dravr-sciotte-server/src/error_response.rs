// ABOUTME: Shared HTTP response helpers for scraper errors and backpressure rejections
// ABOUTME: Maps ScraperError::Busy to 503 + Retry-After and builds uniform JSON bodies
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use dravr_sciotte::error::ScraperError;
use serde_json::json;
use tracing::warn;

/// Build a 503 Service Unavailable response with a `Retry-After` header from a
/// [`ScraperError::Busy`]. Accepts the original error so callers can uniformly
/// forward the structured reason to the client.
pub fn busy_response(reason: &str, retry_after_secs: u64) -> Response {
    warn!(
        reason,
        retry_after_secs, "Scraper backpressure — rejecting request"
    );
    let body = Json(json!({
        "error": "scraper_busy",
        "reason": reason,
        "retry_after_secs": retry_after_secs,
    }));
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, body).into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

/// Convert an arbitrary [`ScraperError`] into an HTTP response, applying the
/// 503 + `Retry-After` treatment for [`ScraperError::Busy`] and falling back
/// to a 500 JSON error for every other variant.
pub fn scraper_error_response(error: &ScraperError) -> Response {
    if let ScraperError::Busy {
        reason,
        retry_after_secs,
    } = error
    {
        return busy_response(reason, *retry_after_secs);
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
        .into_response()
}

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

/// Convert an arbitrary [`ScraperError`] into an HTTP response.
///
/// [`ScraperError::Busy`] keeps its 503 + `Retry-After` treatment, the
/// auth-shaped variants answer 401 with a `session_expired` body, and every
/// other variant falls back to a 500 JSON error.
///
/// [`ScraperError::Auth`] and [`ScraperError::SessionExpired`] both mean the
/// held session can no longer act for the athlete (the provider rejected its
/// cookies or bounced the login), so they answer `401` with the same
/// `error` marker family as `session_not_found` — the platform classifies
/// those as "athlete must re-login" and hands out a reconnect link, whereas
/// a `500` reads as a scraper fault and pages an operator for something no
/// operator can fix.
pub fn scraper_error_response(error: &ScraperError) -> Response {
    if let ScraperError::Busy {
        reason,
        retry_after_secs,
    } = error
    {
        return busy_response(reason, *retry_after_secs);
    }
    if matches!(
        error,
        ScraperError::Auth { .. } | ScraperError::SessionExpired { .. }
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "session_expired", "message": error.to_string() })),
        )
            .into_response();
    }
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error.to_string() })),
    )
        .into_response()
}

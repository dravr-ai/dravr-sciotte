// ABOUTME: Bearer token authentication middleware for REST API endpoints
// ABOUTME: Checks DRAVR_SCIOTTE_API_KEY environment variable for optional auth
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

/// Authentication middleware that checks for a valid bearer token.
/// Skipped if `DRAVR_SCIOTTE_API_KEY` is not set.
pub async fn auth_middleware(request: Request, next: Next) -> Response {
    let Some(expected_key) = std::env::var("DRAVR_SCIOTTE_API_KEY").ok() else {
        // No API key configured — skip authentication
        return next.run(request).await;
    };

    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let provided_key = match auth_header {
        Some(header) if header.starts_with("Bearer ") => &header[7..],
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "Missing or invalid Authorization header"})),
            )
                .into_response();
        }
    };

    if provided_key
        .as_bytes()
        .ct_eq(expected_key.as_bytes())
        .into()
    {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "Invalid API key"})),
        )
            .into_response()
    }
}

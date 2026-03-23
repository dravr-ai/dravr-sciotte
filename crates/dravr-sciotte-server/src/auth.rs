// ABOUTME: Bearer token authentication middleware for REST API endpoints
// ABOUTME: Delegates to dravr-tronc shared auth, checks DRAVR_SCIOTTE_API_KEY environment variable
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

/// Environment variable name for the API key
const API_KEY_ENV: &str = "DRAVR_SCIOTTE_API_KEY";

/// Authentication middleware that checks for a valid bearer token.
///
/// The env var is read on every request to allow runtime key rotation
/// without restarting the server. If the variable is not set, all requests
/// are allowed through (localhost development mode). If set, requests must
/// include a matching `Authorization: Bearer <key>` header.
pub async fn auth_middleware(request: Request, next: Next) -> Response {
    dravr_tronc::server::auth::require_auth(API_KEY_ENV, request, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_env_is_correct() {
        assert_eq!(API_KEY_ENV, "DRAVR_SCIOTTE_API_KEY");
    }
}

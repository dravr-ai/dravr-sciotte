// ABOUTME: Google identity-token authentication for the REST API and MCP transport
// ABOUTME: Replaces the shared DRAVR_SCIOTTE_API_KEY bearer, which failed open and had to be rotated by hand
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # Why this replaced a shared key
//!
//! The previous gate delegated to `dravr_tronc::server::auth::require_auth`,
//! which reads `DRAVR_SCIOTTE_API_KEY` and — by design — **passes every request
//! through when that variable is unset**. That default is survivable for a
//! service reachable only from inside a cluster. This one answers on a public
//! URL, so a single missing environment variable turned the whole scraper,
//! including `/auth/sessions/{id}/export`, into an open endpoint.
//!
//! It also depended on every route remembering to carry the layer.
//! `/browser/login` did not, and nothing detected that for as long as it stood.
//!
//! Google already signs a statement of who a caller is. This verifies that
//! instead: no shared secret to rotate, no fail-open default, and a caller
//! identity in every request rather than a password anyone holding it can use.

use std::env;
use std::error::Error as StdError;
use std::fmt;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use dravr_tronc::iam::{require_google_id_token, GoogleIdTokenVerifier};

/// Environment variable naming the audience tokens must be addressed to.
///
/// A stable custom audience configured on the Cloud Run service, not the
/// generated URL, so the caller and this service can be configured from one
/// value that does not depend on what Cloud Run assigns at creation.
pub const AUDIENCE_ENV: &str = "DRAVR_SCIOTTE_AUDIENCE";

/// Why the server refused to start.
#[derive(Debug)]
pub enum AuthConfigError {
    /// [`AUDIENCE_ENV`] was unset or empty.
    MissingAudience,
}

impl fmt::Display for AuthConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAudience => write!(
                f,
                "{AUDIENCE_ENV} is unset — refusing to start a scraper that cannot pin the audience it accepts"
            ),
        }
    }
}

impl StdError for AuthConfigError {}

/// Build the verifier this server gates every protected route on.
///
/// # Errors
///
/// [`AuthConfigError::MissingAudience`] when [`AUDIENCE_ENV`] is unset. Fails
/// closed deliberately: a scraper that does not know which audience to require
/// would accept tokens minted for any other service, and the point of leaving a
/// shared key behind was to stop a missing variable from opening the door.
pub fn verifier_from_env(
    http: reqwest::Client,
) -> Result<Arc<GoogleIdTokenVerifier>, AuthConfigError> {
    let audience = env::var(AUDIENCE_ENV)
        .ok()
        .filter(|a| !a.is_empty())
        .ok_or(AuthConfigError::MissingAudience)?;
    Ok(Arc::new(GoogleIdTokenVerifier::new(audience, http)))
}

/// Authentication middleware requiring a Google-signed identity token.
pub async fn auth_middleware(
    verifier: Arc<GoogleIdTokenVerifier>,
    request: Request,
    next: Next,
) -> Response {
    require_google_id_token(verifier, request, next).await
}

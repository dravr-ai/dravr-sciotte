// ABOUTME: Scraper error types covering auth, browser, scraping, network, cache, and config failures
// ABOUTME: Includes HTTP error response types for REST API compatibility
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;

/// Result type alias for scraper operations
pub type ScraperResult<T> = Result<T, ScraperError>;

/// Errors arising from Strava scraping operations
#[derive(Debug, thiserror::Error)]
pub enum ScraperError {
    /// OAuth or session authentication error
    #[error("auth error: {reason}")]
    Auth {
        /// Detailed failure reason
        reason: String,
    },

    /// Browser launch, navigation, or interaction failure
    #[error("browser error: {reason}")]
    Browser {
        /// Detailed failure reason
        reason: String,
    },

    /// Element not found or page structure changed
    #[error("scraping error: {reason}")]
    Scraping {
        /// Detailed failure reason
        reason: String,
    },

    /// Network timeout or connectivity issue
    #[error("network error: {reason}")]
    Network {
        /// Detailed failure reason
        reason: String,
    },

    /// Configuration error (missing env vars, invalid values)
    #[error("config error: {reason}")]
    Config {
        /// Detailed failure reason
        reason: String,
    },

    /// Session has expired and needs re-authentication
    #[error("session expired: {reason}")]
    SessionExpired {
        /// Detailed reason
        reason: String,
    },

    /// Internal or unexpected error
    #[error("internal error: {reason}")]
    Internal {
        /// Detailed failure reason
        reason: String,
    },
}

impl ScraperError {
    /// Whether this error is transient and may succeed on retry
    pub const fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Network { .. } | Self::Browser { .. } | Self::SessionExpired { .. }
        )
    }
}

/// Result of a credential-based login attempt
#[derive(Debug)]
pub enum LoginResult {
    /// Login succeeded — session cookies captured
    Success(crate::models::AuthSession),
    /// Provider requires a one-time password / 2FA code
    OtpRequired,
    /// Login was rejected (wrong password, account locked, etc.)
    Failed(String),
}

/// HTTP error response body for REST API error responses
#[derive(Debug, serde::Serialize)]
pub struct ErrorResponse {
    /// Error category
    pub error: ErrorBody,
}

/// Inner error body
#[derive(Debug, serde::Serialize)]
pub struct ErrorBody {
    /// Error type identifier
    #[serde(rename = "type")]
    pub error_type: String,
    /// Human-readable error message
    pub message: String,
}

impl ErrorResponse {
    /// Create an error response with the given type and message
    pub fn new(error_type: impl Into<String>, message: impl fmt::Display) -> Self {
        Self {
            error: ErrorBody {
                error_type: error_type.into(),
                message: message.to_string(),
            },
        }
    }
}

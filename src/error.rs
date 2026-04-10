// ABOUTME: Scraper error types covering auth, browser, scraping, network, cache, and config failures
// ABOUTME: Includes HTTP error response types for REST API compatibility
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fmt;

use crate::models::AuthSession;

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
    Success(AuthSession),
    /// Provider requires a one-time password / 2FA code entry
    OtpRequired,
    /// Provider shows multiple 2FA options — let the user choose
    TwoFactorChoice(Vec<TwoFactorOption>),
    /// Provider shows a number matching challenge — user must tap the matching number
    /// on their phone. Contains the number to display and the scraper continues polling
    /// for success in the background.
    NumberMatch(String),
    /// Login was rejected (wrong password, account locked, etc.)
    Failed(String),
}

/// A 2FA method option presented by the provider
#[derive(Debug, Clone, serde::Serialize)]
pub struct TwoFactorOption {
    /// Machine-readable identifier (e.g., "otp", "app", "sms")
    pub id: String,
    /// Human-readable label (e.g., "Google Authenticator", "Tap Yes on your phone")
    pub label: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_result_success_holds_session() {
        let session = AuthSession {
            session_id: "test-123".to_owned(),
            cookies: vec![],
            created_at: chrono::Utc::now(),
            expires_at: None,
        };
        let result = LoginResult::Success(session);
        assert!(matches!(result, LoginResult::Success(s) if s.session_id == "test-123"));
    }

    #[test]
    fn login_result_two_factor_choice_holds_options() {
        let options = vec![
            TwoFactorOption {
                id: "otp".to_owned(),
                label: "Google Authenticator".to_owned(),
            },
            TwoFactorOption {
                id: "app".to_owned(),
                label: "Tap Yes on phone".to_owned(),
            },
        ];
        let result = LoginResult::TwoFactorChoice(options);
        assert!(matches!(result, LoginResult::TwoFactorChoice(opts) if opts.len() == 2));
    }

    #[test]
    fn two_factor_option_serializes() {
        let opt = TwoFactorOption {
            id: "sms".to_owned(),
            label: "Text message".to_owned(),
        };
        let json = serde_json::to_string(&opt).unwrap(); // Safe: test with simple struct
        assert!(json.contains("sms"));
        assert!(json.contains("Text message"));
    }

    #[test]
    fn scraper_error_is_transient() {
        assert!(ScraperError::Network {
            reason: "timeout".to_owned()
        }
        .is_transient());
        assert!(ScraperError::Browser {
            reason: "crash".to_owned()
        }
        .is_transient());
        assert!(!ScraperError::Auth {
            reason: "bad password".to_owned()
        }
        .is_transient());
        assert!(!ScraperError::Config {
            reason: "missing".to_owned()
        }
        .is_transient());
    }
}

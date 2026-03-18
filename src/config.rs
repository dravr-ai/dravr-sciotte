// ABOUTME: Configuration types for OAuth, scraping, and caching
// ABOUTME: Loaded from environment variables with sensible defaults
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::path::PathBuf;

use crate::error::{ScraperError, ScraperResult};

/// Strava OAuth configuration
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Strava OAuth client ID
    pub client_id: String,
    /// Strava OAuth client secret
    pub client_secret: String,
    /// OAuth callback redirect URI
    pub redirect_uri: String,
    /// OAuth scopes (comma-separated per Strava convention)
    pub scopes: String,
}

impl OAuthConfig {
    /// Load OAuth config from environment variables
    pub fn from_env() -> ScraperResult<Self> {
        let client_id = env::var("STRAVA_CLIENT_ID").map_err(|_| ScraperError::Config {
            reason: "STRAVA_CLIENT_ID environment variable is required".to_owned(),
        })?;
        let client_secret = env::var("STRAVA_CLIENT_SECRET").map_err(|_| ScraperError::Config {
            reason: "STRAVA_CLIENT_SECRET environment variable is required".to_owned(),
        })?;
        let redirect_uri = env::var("STRAVA_REDIRECT_URI")
            .unwrap_or_else(|_| "http://localhost:3000/auth/callback".to_owned());
        let scopes = env::var("STRAVA_SCOPES").unwrap_or_else(|_| "activity:read_all".to_owned());

        Ok(Self {
            client_id,
            client_secret,
            redirect_uri,
            scopes,
        })
    }

    /// Build the Strava OAuth authorization URL
    #[must_use]
    pub fn authorize_url(&self, state: &str) -> String {
        format!(
            "https://www.strava.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&approval_prompt=auto",
            self.client_id,
            urlencoding::encode(&self.redirect_uri),
            urlencoding::encode(&self.scopes),
            urlencoding::encode(state),
        )
    }
}

/// Scraper configuration
#[derive(Debug, Clone)]
pub struct ScraperConfig {
    /// Path to Chrome/Chromium binary (auto-detected if not set)
    pub chrome_path: Option<String>,
    /// Whether to run Chrome in headless mode
    pub headless: bool,
    /// Page load timeout in seconds
    pub page_timeout_secs: u64,
    /// Delay between page interactions in milliseconds
    pub interaction_delay_ms: u64,
}

impl Default for ScraperConfig {
    fn default() -> Self {
        Self {
            chrome_path: env::var("CHROME_PATH").ok(),
            headless: true,
            page_timeout_secs: 30,
            interaction_delay_ms: 500,
        }
    }
}

/// Cache configuration
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Cache TTL in seconds
    pub ttl_secs: u64,
    /// Maximum number of cached entries
    pub max_entries: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl_secs: env::var("DRAVR_SCIOTTE_CACHE_TTL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(900),
            max_entries: env::var("DRAVR_SCIOTTE_CACHE_MAX")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(100),
        }
    }
}

/// Directory where encrypted session data is stored
pub fn session_dir() -> PathBuf {
    env::var("DRAVR_SCIOTTE_SESSION_DIR").map_or_else(
        |_| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("dravr-sciotte")
        },
        PathBuf::from,
    )
}

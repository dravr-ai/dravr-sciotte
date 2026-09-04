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

/// Login automation strategy
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LoginMode {
    /// CSS selectors + URL patterns (fast, free, fragile)
    #[default]
    Selector,
    /// LLM screenshot analysis via embacle (resilient, costs per login)
    Vision,
    /// Try selectors first, fall back to vision on failure
    Hybrid,
}

impl LoginMode {
    /// Parse from a string value (env var or config)
    #[must_use]
    pub fn from_str_value(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "vision" => Self::Vision,
            "hybrid" => Self::Hybrid,
            _ => Self::Selector,
        }
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
    /// Interval between URL polls during login detection (ms)
    pub login_poll_interval_ms: u64,
    /// Overall login timeout before giving up (s)
    pub login_timeout_secs: u64,
    /// Wait time after page navigation for JS rendering (s)
    pub page_load_wait_secs: u64,
    /// Delay between form field interactions (ms)
    pub form_interaction_delay_ms: u64,
    /// Timeout waiting for the next login step (e.g., password field after email submit) (s)
    pub email_step_timeout_secs: u64,
    /// Timeout waiting for login result after password submit (s)
    pub password_step_timeout_secs: u64,
    /// Timeout waiting for phone tap / app approval during 2FA (s)
    pub phone_tap_timeout_secs: u64,
    /// Maximum consecutive poll iterations to spend stuck on the same
    /// unresolvable challenge URL (e.g. `/challenge/dp` after the single
    /// "Try another way" click failed to navigate) before returning a
    /// structured `Auth` error instead of polling to the full step
    /// timeout. With the default poll interval (500ms) this caps the
    /// stuck window at a few seconds, letting Hybrid login mode escalate
    /// to the vision fallback quickly rather than waiting out the timeout.
    pub stuck_challenge_max_polls: u32,
    /// Login automation strategy: selector, vision, or hybrid
    pub login_mode: LoginMode,
    /// Whether credential login uses headless Chrome (default: false — Google blocks headless)
    pub credential_login_headless: bool,
    /// Use fake HTML login pages for testing (no real provider interaction)
    pub fake_login: bool,
    /// Base directory for persistent per-session Chrome profiles. When a
    /// scraper call provides a `profile_id`, the actual profile lives at
    /// `{profile_base_dir}/{session_id}` so cookies (`cf_clearance`,
    /// provider bearers) survive across browser launches. Falls back to
    /// `env::temp_dir()/sciotte-profiles` if the env var is unset.
    pub profile_base_dir: PathBuf,
    /// Optional residential proxy URL (Layer 2 anti-bot egress). When set,
    /// `--proxy-server={url}` is passed to Chrome at launch. The literal
    /// `{session_id}` placeholder in the URL is replaced with the caller's
    /// `profile_id` at launch time so each user maps to a sticky residential
    /// IP — required for sustained Garmin/Strava scraping from datacenter
    /// egress (GCP) where Cloudflare Turnstile would otherwise escalate.
    pub proxy_url: Option<String>,
    /// Maximum age (seconds) for an in-flight 2FA/OTP login held between
    /// `credential_login` and the follow-up `submit_otp` /
    /// `select_two_factor` call. Sessions older than this are evicted on
    /// the next access — the held Chrome process is closed via
    /// chromiumoxide's `kill_on_drop`, freeing memory for abandoned flows.
    ///
    /// Defaults to the 30 minutes a provider one-time code is actually valid
    /// for. It was 5, which is shorter than the code the provider just mailed:
    /// a human who fetches the code from their inbox and pastes it back can
    /// easily take longer than that, and the flow was already evicted when they
    /// did — `submit_otp` answered `no_pending_login` and the whole login had to
    /// be repeated, which on Garmin then trips throttling. Holding a Chrome
    /// process for the abandoned case is the cost; a code that cannot be
    /// redeemed inside its own validity window is the alternative.
    pub pending_login_ttl_secs: u64,
    /// Hard cap on list-page "next" clicks during a single activity scrape.
    /// A date-bounded historical scrape pages back through the reverse-
    /// chronological feed until it crosses the requested `after`; this backstop
    /// bounds a runaway (or a never-disabled "next" button) at
    /// `max_scrape_pages * ~20` activities so a deep query can't scroll forever.
    pub max_scrape_pages: u32,
}

impl Default for ScraperConfig {
    fn default() -> Self {
        Self {
            chrome_path: env::var("CHROME_PATH").ok(),
            headless: true,
            page_timeout_secs: env_u64("DRAVR_SCIOTTE_PAGE_TIMEOUT", 30),
            interaction_delay_ms: env_u64("DRAVR_SCIOTTE_INTERACTION_DELAY_MS", 500),
            login_poll_interval_ms: env_u64("DRAVR_SCIOTTE_LOGIN_POLL_INTERVAL_MS", 500),
            login_timeout_secs: env_u64("DRAVR_SCIOTTE_LOGIN_TIMEOUT", 120),
            page_load_wait_secs: env_u64("DRAVR_SCIOTTE_PAGE_LOAD_WAIT", 3),
            form_interaction_delay_ms: env_u64("DRAVR_SCIOTTE_FORM_DELAY_MS", 300),
            email_step_timeout_secs: env_u64("DRAVR_SCIOTTE_EMAIL_STEP_TIMEOUT", 10),
            password_step_timeout_secs: env_u64("DRAVR_SCIOTTE_PASSWORD_STEP_TIMEOUT", 30),
            phone_tap_timeout_secs: env_u64("DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT", 60),
            stuck_challenge_max_polls: env_u32("DRAVR_SCIOTTE_STUCK_CHALLENGE_MAX_POLLS", 12),
            login_mode: env::var("DRAVR_SCIOTTE_LOGIN_MODE")
                .map(|v| LoginMode::from_str_value(&v))
                .unwrap_or_default(),
            credential_login_headless: env::var("DRAVR_SCIOTTE_CREDENTIAL_LOGIN_HEADLESS")
                .is_ok_and(|v| v == "true" || v == "1"),
            fake_login: env::var("DRAVR_SCIOTTE_FAKE_LOGIN").is_ok_and(|v| v == "true" || v == "1"),
            profile_base_dir: env::var("DRAVR_SCIOTTE_PROFILE_DIR")
                .map_or_else(|_| env::temp_dir().join("sciotte-profiles"), PathBuf::from),
            proxy_url: env::var("DRAVR_SCIOTTE_PROXY_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            pending_login_ttl_secs: env_u64("DRAVR_SCIOTTE_PENDING_LOGIN_TTL", 1800),
            max_scrape_pages: env_u32("DRAVR_SCIOTTE_MAX_SCRAPE_PAGES", 250),
        }
    }
}

/// Read a u64 from an environment variable with a default fallback
fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read a u32 from an environment variable with a default fallback
fn env_u32(key: &str, default: u32) -> u32 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_u64_returns_default_when_unset() {
        assert_eq!(env_u64("DRAVR_SCIOTTE_TEST_NONEXISTENT_VAR_12345", 42), 42);
    }

    /// Env keys this test asserts the compiled-in fallbacks for.
    const DEFAULTED_ENV_KEYS: &[&str] = &[
        "DRAVR_SCIOTTE_LOGIN_POLL_INTERVAL_MS",
        "DRAVR_SCIOTTE_LOGIN_TIMEOUT",
        "DRAVR_SCIOTTE_PAGE_LOAD_WAIT",
        "DRAVR_SCIOTTE_FORM_DELAY_MS",
        "DRAVR_SCIOTTE_EMAIL_STEP_TIMEOUT",
        "DRAVR_SCIOTTE_PASSWORD_STEP_TIMEOUT",
        "DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT",
        "DRAVR_SCIOTTE_STUCK_CHALLENGE_MAX_POLLS",
        "DRAVR_SCIOTTE_PENDING_LOGIN_TTL",
    ];

    #[test]
    fn scraper_config_default_values() {
        // `ScraperConfig::default()` layers env overrides onto the compiled-in
        // fallbacks, so asserting them requires clearing those keys first —
        // otherwise this reads whatever the developer's shell exports and is
        // not a defaults test at all. dravr-platform's .envrc sets
        // DRAVR_SCIOTTE_LOGIN_TIMEOUT=300, which failed this test on nothing
        // but a loaded direnv.
        let saved: Vec<(&str, Option<String>)> = DEFAULTED_ENV_KEYS
            .iter()
            .map(|key| (*key, env::var(key).ok()))
            .collect();
        for key in DEFAULTED_ENV_KEYS {
            env::remove_var(key); // Safe: edition 2021 test mutation, restored below
        }

        let config = ScraperConfig::default();

        // Restore before asserting: a failing assertion panics, and leaving the
        // process env clobbered would cascade into every later test.
        for (key, value) in &saved {
            if let Some(value) = value {
                env::set_var(key, value);
            }
        }

        assert_eq!(config.login_poll_interval_ms, 500);
        assert_eq!(config.login_timeout_secs, 120);
        assert_eq!(config.page_load_wait_secs, 3);
        assert_eq!(config.form_interaction_delay_ms, 300);
        assert_eq!(config.email_step_timeout_secs, 10);
        assert_eq!(config.password_step_timeout_secs, 30);
        assert_eq!(config.phone_tap_timeout_secs, 60);
        assert_eq!(config.stuck_challenge_max_polls, 12);
        // 30 minutes: the validity window of the code the provider sends.
        assert_eq!(config.pending_login_ttl_secs, 1800);
    }

    #[test]
    fn scraper_config_env_overrides_the_default() {
        // The complement of the test above: prove the override path is wired,
        // so clearing env there cannot mask a broken env key.
        let key = "DRAVR_SCIOTTE_LOGIN_TIMEOUT";
        let saved = env::var(key).ok();

        env::set_var(key, "417"); // Safe: edition 2021 test mutation, restored below
        let overridden = ScraperConfig::default().login_timeout_secs;

        match saved {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }

        assert_eq!(overridden, 417);
    }

    #[test]
    fn scraper_config_headless_default() {
        let config = ScraperConfig::default();
        assert!(config.headless);
    }

    #[test]
    fn login_mode_default_is_selector() {
        assert_eq!(LoginMode::default(), LoginMode::Selector);
    }

    #[test]
    fn login_mode_from_str() {
        assert_eq!(LoginMode::from_str_value("vision"), LoginMode::Vision);
        assert_eq!(LoginMode::from_str_value("Vision"), LoginMode::Vision);
        assert_eq!(LoginMode::from_str_value("VISION"), LoginMode::Vision);
        assert_eq!(LoginMode::from_str_value("hybrid"), LoginMode::Hybrid);
        assert_eq!(LoginMode::from_str_value("selector"), LoginMode::Selector);
        assert_eq!(LoginMode::from_str_value("anything"), LoginMode::Selector);
        assert_eq!(LoginMode::from_str_value(""), LoginMode::Selector);
    }

    #[test]
    fn scraper_config_default_login_mode() {
        let config = ScraperConfig::default();
        assert_eq!(config.login_mode, LoginMode::Selector);
    }
}

// ABOUTME: Core ActivityScraper trait defining the scraping interface
// ABOUTME: Implemented by the chromiumoxide-based scraper and wrapped by the cache layer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;

use crate::error::{LoginResult, ScraperResult};
use crate::models::{
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams,
};

/// Core trait for Strava training page scraping
///
/// # Integration Modes
///
/// 1. **Programmatic** — use this trait directly in your Rust code
/// 2. **REST API** — run `dravr-sciotte-server serve` for HTTP endpoints
/// 3. **MCP** — run `dravr-sciotte-server --transport stdio` for Claude integration
/// 4. **CLI** — run `dravr-sciotte-server login` / `activities` commands
#[async_trait]
pub trait ActivityScraper: Send + Sync {
    /// Open a visible browser so the user can log in to Strava manually.
    /// Waits for login to complete, captures session cookies, and returns
    /// an authenticated session. No API credentials required.
    async fn browser_login(&self) -> ScraperResult<AuthSession>;

    /// Log in programmatically with email and password.
    ///
    /// The `method` parameter selects the login flow:
    /// - `"email"` — fill the provider's native login form directly
    /// - `"google"` — click the Google OAuth button, fill Google's email/password form
    /// - `"apple"` — click the Apple OAuth button, fill Apple's sign-in form
    ///
    /// If OTP is required, the browser is kept alive for a follow-up `submit_otp` call.
    async fn credential_login(
        &self,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult>;

    /// Submit a one-time password / 2FA code after `credential_login` returned `OtpRequired`.
    /// Reuses the browser page from the credential login attempt, fills the OTP field,
    /// and polls for success or failure.
    async fn submit_otp(&self, code: &str) -> ScraperResult<LoginResult>;

    /// Select a 2FA method after `credential_login` returned `TwoFactorChoice`.
    /// Clicks the chosen option and returns:
    /// - `OtpRequired` if a code entry field appears
    /// - `Success` if the method completes without user input (e.g., phone tap)
    /// - `Failed` if the method is rejected
    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult>;

    /// Check if a session is still valid (cookies not expired)
    async fn is_authenticated(&self, session: &AuthSession) -> bool;

    /// Scrape activities from the Strava training page
    async fn get_activities(
        &self,
        session: &AuthSession,
        params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>>;

    /// Scrape a single activity's detail page
    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity>;

    /// Scrape the authenticated user's profile from the provider
    async fn get_athlete(&self, session: &AuthSession) -> ScraperResult<AthleteProfile>;

    /// Scrape the daily health/wellness summary for a given date.
    ///
    /// Returns structured health data (heart rate, body battery, stress, steps, etc.)
    /// from the provider's daily summary page. Not all providers support this —
    /// returns `ScraperError::Config` if the provider has no health page configured.
    async fn get_daily_summary(
        &self,
        session: &AuthSession,
        params: &HealthParams,
    ) -> ScraperResult<DailySummary>;
}

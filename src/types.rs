// ABOUTME: Core ActivityScraper trait defining the scraping interface
// ABOUTME: Implemented by the chromiumoxide-based scraper and wrapped by the cache layer
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;

use crate::error::ScraperResult;
use crate::models::{Activity, ActivityParams, AuthSession};

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
}

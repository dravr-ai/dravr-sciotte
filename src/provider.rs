// ABOUTME: TOML-based provider configuration for scraping sport activity pages
// ABOUTME: Defines selectors, URLs, and JS extraction rules per provider (Strava, Garmin, etc.)
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::{ScraperError, ScraperResult};

/// Root configuration for a sport activity provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Provider identity and login settings
    pub provider: ProviderIdentity,
    /// List page where activities are displayed in a table/list
    pub list_page: ListPageConfig,
    /// Detail page for a single activity with full metrics
    pub detail_page: DetailPageConfig,
    /// Health/wellness pages keyed by name (e.g., "sleep", "weight").
    /// Each page contributes fields to a single `DailySummary` via JS extraction.
    #[serde(default)]
    pub health_pages: HashMap<String, HealthPageConfig>,
}

/// Configuration for a daily health/wellness summary page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthPageConfig {
    /// URL template with a `{date}` placeholder replaced at runtime with `YYYY-MM-DD`
    pub url_template: String,
    /// JavaScript snippet that extracts health metrics and returns JSON
    pub js_extract: String,
}

/// Provider identity: name, login URL, and how to detect successful login
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderIdentity {
    /// Provider name (e.g., "strava", "garmin")
    pub name: String,
    /// URL of the login page where the user authenticates
    pub login_url: String,
    /// URL patterns that indicate the user is logged in (matched against current URL)
    pub login_success_patterns: Vec<String>,
    /// URL patterns that indicate the user is NOT logged in
    pub login_failure_patterns: Vec<String>,
    /// CSS selector for the email/username input field
    #[serde(default)]
    pub login_email_selector: Option<String>,
    /// CSS selector for the password input field
    #[serde(default)]
    pub login_password_selector: Option<String>,
    /// CSS selector for the login submit button
    #[serde(default)]
    pub login_button_selector: Option<String>,
    /// CSS selector for the login error message element
    #[serde(default)]
    pub login_error_selector: Option<String>,
    /// CSS selector for the OTP/2FA code input field
    #[serde(default)]
    pub login_otp_selector: Option<String>,
    /// CSS selectors for OAuth login buttons, keyed by method name (e.g., "google", "apple")
    #[serde(default)]
    pub login_oauth_buttons: HashMap<String, String>,
    /// Path to a markdown prompt for vision-based page analysis during login
    #[serde(default)]
    pub vision_page_analysis_prompt: Option<String>,
    /// URL of the athlete profile page
    #[serde(default)]
    pub profile_url: Option<String>,
    /// JavaScript snippet that extracts athlete profile data and returns JSON
    #[serde(default)]
    pub profile_js_extract: Option<String>,
}

/// Configuration for the list/training page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPageConfig {
    /// URL of the activity list page
    pub url: String,
    /// CSS selector for each activity row in the table
    pub row_selector: String,
    /// CSS selector for the link element within each row
    pub link_selector: String,
    /// Regex to extract the activity ID from the link href
    pub id_regex: String,
    /// CSS selectors for fields to extract from each row
    pub fields: ListFieldSelectors,
    /// Optional custom JS for list extraction (overrides auto-generated JS from fields)
    #[serde(default)]
    pub js_extract: Option<String>,
    /// Path to a markdown file with vision extraction prompt (used by `VisionScraper`)
    #[serde(default)]
    pub vision_prompt: Option<String>,
}

/// CSS selectors for extracting fields from list page rows
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFieldSelectors {
    /// Selector for the activity name
    pub name: String,
    /// Selector for the sport type
    pub sport_type: String,
    /// Selector for the date
    pub date: String,
    /// Selector for the duration/time
    pub time: String,
    /// Selector for the distance
    pub distance: String,
    /// Selector for the elevation gain
    pub elevation: String,
    /// Selector for the suffer/effort score (optional)
    #[serde(default)]
    pub suffer_score: Option<String>,
}

/// Configuration for the activity detail page
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailPageConfig {
    /// URL template with `{id}` placeholder for the activity ID
    pub url_template: String,
    /// JavaScript snippet that extracts all activity data and returns JSON
    pub js_extract: String,
    /// Path to a markdown file with vision extraction prompt (used by `VisionScraper`)
    #[serde(default)]
    pub vision_prompt: Option<String>,
}

impl ProviderConfig {
    /// Load a provider configuration from a TOML file
    pub fn from_file(path: &std::path::Path) -> ScraperResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| ScraperError::Config {
            reason: format!("Failed to read provider config {}: {e}", path.display()),
        })?;
        Self::from_toml(&content)
    }

    /// Parse a provider configuration from TOML content
    pub fn from_toml(content: &str) -> ScraperResult<Self> {
        toml::from_str(content).map_err(|e| ScraperError::Config {
            reason: format!("Failed to parse provider config: {e}"),
        })
    }

    /// Build the detail page URL for a given activity ID
    pub fn detail_url(&self, activity_id: &str) -> String {
        self.detail_page.url_template.replace("{id}", activity_id)
    }

    /// Build URLs for all configured health pages, substituting the date placeholder.
    /// Returns (name, url) pairs sorted alphabetically by name.
    pub fn health_urls(&self, date: &chrono::NaiveDate) -> Vec<(&str, String)> {
        const DATE_PLACEHOLDER: &str = "{date}";
        let formatted = date.format("%Y-%m-%d").to_string();
        let mut pages: Vec<(&str, String)> = self
            .health_pages
            .iter()
            .map(|(name, hp)| {
                (
                    name.as_str(),
                    hp.url_template.replace(DATE_PLACEHOLDER, &formatted),
                )
            })
            .collect();
        pages.sort_by_key(|(name, _)| *name);
        pages
    }

    /// Generate the JS snippet for extracting activities from the list page.
    ///
    /// Uses double-quote JS strings and `new RegExp()` to avoid quoting conflicts
    /// with CSS selectors that contain single quotes.
    /// Generate the JS snippet for extracting activities from the list page.
    ///
    /// If the provider config has a custom `js_extract` in `list_page`, use that.
    /// Otherwise, auto-generate JS from the CSS selectors in `fields`.
    pub fn list_extraction_js(&self) -> String {
        if let Some(ref custom_js) = self.list_page.js_extract {
            return custom_js.clone();
        }
        let f = &self.list_page.fields;
        let esc = |s: &str| s.replace('"', r#"\""#);

        let suffer_line = self.list_page.fields.suffer_score.as_ref().map_or_else(
            || r#"suffer_score: """#.to_owned(),
            |sel| format!(r#"suffer_score: q(row, "{}"),"#, esc(sel)),
        );

        format!(
            r#"
(function() {{
    var q = function(el, sel) {{ var e = el.querySelector(sel); return e ? e.textContent.trim() : ""; }};
    var rows = document.querySelectorAll("{row_sel}");
    var activities = [];
    var seen = {{}};
    for (var i = 0; i < rows.length; i++) {{
        var row = rows[i];
        var link = row.querySelector("{link_sel}");
        if (!link) continue;
        var href = link.getAttribute("href") || "";
        var idMatch = href.match({id_regex});
        if (!idMatch || seen[idMatch[1]]) continue;
        seen[idMatch[1]] = true;
        activities.push({{
            id: idMatch[1],
            name: q(row, "{name_sel}"),
            type: q(row, "{type_sel}"),
            date: q(row, "{date_sel}"),
            time: q(row, "{time_sel}"),
            distance: q(row, "{dist_sel}"),
            elevation: q(row, "{elev_sel}"),
            {suffer_line}
        }});
    }}
    return JSON.stringify(activities);
}})()
"#,
            row_sel = esc(&self.list_page.row_selector),
            link_sel = esc(&self.list_page.link_selector),
            id_regex = self.list_page.id_regex,
            name_sel = esc(&f.name),
            type_sel = esc(&f.sport_type),
            date_sel = esc(&f.date),
            time_sel = esc(&f.time),
            dist_sel = esc(&f.distance),
            elev_sel = esc(&f.elevation),
            suffer_line = suffer_line,
        )
    }

    /// Return the embedded Strava provider config as a fallback.
    ///
    /// This uses the bundled `providers/strava.toml` which is compiled into the binary.
    /// The config is validated at test time so this will not fail at runtime.
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is malformed (compile-time constant, tested).
    pub fn strava_default() -> Self {
        Self::from_toml(STRAVA_PROVIDER_TOML).expect("strava config") // Safe: static data
    }

    /// Create with the built-in Garmin Connect provider configuration
    ///
    /// # Panics
    ///
    /// Panics if the embedded TOML is malformed (compile-time constant, tested).
    #[must_use]
    pub fn garmin_default() -> Self {
        Self::from_toml(GARMIN_PROVIDER_TOML).expect("garmin config") // Safe: static data
    }
}

/// Embedded default provider configuration for Strava
const STRAVA_PROVIDER_TOML: &str = include_str!("../providers/strava.toml");

/// Embedded default provider configuration for Garmin Connect
const GARMIN_PROVIDER_TOML: &str = include_str!("../providers/garmin.toml");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strava_default() {
        let config = ProviderConfig::strava_default();
        assert_eq!(config.provider.name, "strava");
        assert!(config.list_page.url.contains("athlete/training"));
        assert!(config.detail_page.url_template.contains("{id}"));
    }

    #[test]
    fn detail_url_substitution() {
        let config = ProviderConfig::strava_default();
        let url = config.detail_url("12345");
        assert!(url.contains("12345"));
        assert!(!url.contains("{id}"));
    }

    #[test]
    fn health_urls_substitution() {
        let config = ProviderConfig::from_toml(
            r#"
[provider]
name = "test"
login_url = "http://example.com/login"
login_success_patterns = ["/home"]
login_failure_patterns = ["/login"]

[list_page]
url = "http://example.com/activities"
row_selector = "tr"
link_selector = "a"
id_regex = '/(\d+)/'
[list_page.fields]
name = "td"
sport_type = "td"
date = "td"
time = "td"
distance = "td"
elevation = "td"

[detail_page]
url_template = "http://example.com/activity/{id}"
js_extract = '(function() { return "{}"; })()'

[health_pages.daily_summary]
url_template = "http://example.com/daily-summary/{date}"
js_extract = '(function() { return "{}"; })()'

[health_pages.sleep]
url_template = "http://example.com/sleep/{date}"
js_extract = '(function() { return "{}"; })()'
"#,
        )
        .unwrap(); // Safe: test with valid inline TOML
        let date = chrono::NaiveDate::from_ymd_opt(2026, 3, 30).unwrap(); // Safe: valid date literal
        let urls = config.health_urls(&date);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].0, "daily_summary");
        assert_eq!(urls[0].1, "http://example.com/daily-summary/2026-03-30");
        assert_eq!(urls[1].0, "sleep");
        assert_eq!(urls[1].1, "http://example.com/sleep/2026-03-30");
    }

    #[test]
    fn strava_has_fitness_health_page() {
        let config = ProviderConfig::strava_default();
        assert!(config.health_pages.contains_key("fitness"));
        let urls = config.health_urls(&chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()); // Safe: valid date literal
        assert!(!urls.is_empty());
        assert!(urls[0].1.contains("fitness"));
    }

    #[test]
    fn list_extraction_js_generates() {
        let config = ProviderConfig::strava_default();
        let js = config.list_extraction_js();
        assert!(js.contains("training-activity-row"));
        assert!(js.contains("JSON.stringify"));
    }
}

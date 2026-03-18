// ABOUTME: Chromiumoxide-based sport activity scraper driven by TOML provider configs
// ABOUTME: Implements StravaScraper trait using headless Chrome via CDP with configurable selectors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chrono::{NaiveDateTime, Utc};
use futures::StreamExt;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::ScraperConfig;
use crate::error::{ScraperError, ScraperResult};
use crate::models::{Activity, ActivityParams, AuthSession, CookieData, SportType};
use crate::provider::ProviderConfig;
use crate::types::StravaScraper;

const LOGIN_POLL_INTERVAL_MS: u64 = 500;
const LOGIN_TIMEOUT_SECS: u64 = 120;

/// Chrome-based sport activity scraper driven by a TOML provider configuration.
///
/// The provider config defines login URLs, CSS selectors, and JS extraction scripts
/// so the same engine can scrape different sport platforms.
pub struct ChromeScraper {
    config: ScraperConfig,
    provider: ProviderConfig,
    /// Shared browser instance for headless scraping (lazily created)
    browser: Mutex<Option<Arc<Browser>>>,
}

impl ChromeScraper {
    /// Create a scraper with explicit provider and browser config
    #[must_use]
    pub fn new(config: ScraperConfig, provider: ProviderConfig) -> Self {
        Self {
            config,
            provider,
            browser: Mutex::new(None),
        }
    }

    /// Create with default browser config and the built-in Strava provider
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(ScraperConfig::default(), ProviderConfig::strava_default())
    }

    /// Get or create a headless browser instance for scraping
    async fn get_headless_browser(&self) -> ScraperResult<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        if let Some(browser) = guard.as_ref() {
            return Ok(Arc::clone(browser));
        }

        let browser = launch_browser(&self.config, true).await?;
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));

        info!("Headless browser launched for scraping");
        Ok(browser)
    }

    /// Inject session cookies into a browser page
    async fn inject_cookies(
        &self,
        page: &chromiumoxide::Page,
        session: &AuthSession,
    ) -> ScraperResult<()> {
        for cookie in &session.cookies {
            let mut param = CookieParam::new(&cookie.name, &cookie.value);
            param.domain = Some(cookie.domain.clone());
            param.path = Some(cookie.path.clone());
            param.secure = Some(cookie.secure);
            param.http_only = Some(cookie.http_only);

            page.set_cookie(param)
                .await
                .map_err(|e| ScraperError::Browser {
                    reason: format!("Failed to set cookie {}: {e}", cookie.name),
                })?;
        }

        debug!(count = session.cookies.len(), "Injected session cookies");
        Ok(())
    }

    /// Open a new page with session cookies and navigate to the given URL
    async fn open_authenticated_page(
        &self,
        session: &AuthSession,
        url: &str,
    ) -> ScraperResult<chromiumoxide::Page> {
        let browser = self.get_headless_browser().await?;

        // Navigate to the provider's login page first so cookies are set on the right domain
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms)).await;

        self.inject_cookies(&page, session).await?;

        // Navigate to the actual target URL with cookies set
        page.goto(url).await.map_err(|e| ScraperError::Browser {
            reason: format!("Failed to navigate to {url}: {e}"),
        })?;

        tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms * 2)).await;
        Ok(page)
    }
}

#[async_trait]
impl StravaScraper for ChromeScraper {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        info!(
            provider = %self.provider.provider.name,
            "Launching visible browser for login"
        );

        let browser = launch_browser(&self.config, false).await?;
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open login page: {e}"),
            })?;

        info!("Waiting for user to log in...");
        wait_for_login(&page, &self.provider).await?;

        let cookies = extract_cookies(&page, &self.provider.provider.name).await?;
        if cookies.is_empty() {
            return Err(ScraperError::Auth {
                reason: "No cookies captured after login".to_owned(),
            });
        }

        let session = AuthSession {
            session_id: generate_session_id(),
            cookies,
            created_at: Utc::now(),
            expires_at: None,
        };

        info!(
            cookie_count = session.cookies.len(),
            "Login successful, session captured"
        );
        Ok(session)
    }

    async fn is_authenticated(&self, session: &AuthSession) -> bool {
        if let Some(expires) = session.expires_at {
            if Utc::now() > expires {
                return false;
            }
        }
        !session.cookies.is_empty()
    }

    async fn get_activities(
        &self,
        session: &AuthSession,
        params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>> {
        let page = self
            .open_authenticated_page(session, &self.provider.list_page.url)
            .await?;

        check_session_redirect(&page, &self.provider).await?;

        let target_count = params.limit.unwrap_or(20) as usize;
        let js = self.provider.list_extraction_js();

        // Paginate the training page: the page shows ~20 activities at a time.
        // We scroll to load more rows until we have enough or no new rows appear.
        let mut all_items: Vec<serde_json::Value> = Vec::new();
        let mut previous_count = 0;

        loop {
            match extract_via_js(&page, &js).await {
                Ok(items) => {
                    all_items = items;
                    debug!(count = all_items.len(), "Activities found on page");
                }
                Err(e) => {
                    warn!(error = %e, "List page JS extraction failed");
                    break;
                }
            }

            // Stop if we have enough or no new rows loaded
            if all_items.len() >= target_count || all_items.len() == previous_count {
                break;
            }

            previous_count = all_items.len();

            // Scroll down to trigger loading more activities
            let scroll_result = page
                .evaluate("window.scrollTo(0, document.body.scrollHeight)")
                .await;
            if scroll_result.is_err() {
                break;
            }

            tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms * 2)).await;
        }

        let mut activities = parse_js_activity_items(&all_items);
        apply_activity_filters(&mut activities, params);

        info!(
            count = activities.len(),
            "Activities extracted from list page"
        );

        // Optionally enrich each activity by navigating to its detail page
        if params.enrich_details {
            info!(
                count = activities.len(),
                "Enriching activities from detail pages (this may take a while)"
            );
            let total = activities.len();
            for (i, activity) in activities.iter_mut().enumerate() {
                info!(
                    progress = format!("{}/{}", i + 1, total),
                    id = %activity.id,
                    "Fetching detail page"
                );
                let detail_url = self.provider.detail_url(&activity.id);
                match navigate_and_extract_detail(&page, &detail_url, &self.provider, &self.config)
                    .await
                {
                    Ok(detail) => merge_detail_into_activity(activity, &detail),
                    Err(e) => {
                        warn!(id = %activity.id, error = %e, "Failed to enrich activity");
                    }
                }
            }
        }

        info!(count = activities.len(), "Activities scraped");
        Ok(activities)
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let url = self.provider.detail_url(activity_id);
        info!(url = %url, "Navigating to activity detail page");

        let page = self.open_authenticated_page(session, &url).await?;
        let data = extract_detail_via_js(&page, &self.provider).await?;
        let activity = build_activity_from_detail(activity_id, &data);

        info!(id = activity_id, name = %activity.name, "Activity detail scraped");
        Ok(activity)
    }
}

// ============================================================================
// Browser helpers
// ============================================================================

/// Launch a Chrome browser (visible for login, headless for scraping)
async fn launch_browser(config: &ScraperConfig, headless: bool) -> ScraperResult<Browser> {
    let mut builder = BrowserConfig::builder();

    if headless {
        builder = builder.arg("--headless=new");
    } else {
        builder = builder.with_head();
    }

    builder = builder
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-blink-features=AutomationControlled")
        .window_size(1920, 1080);

    if let Some(ref path) = config.chrome_path {
        builder = builder.chrome_executable(path);
    }

    let browser_config = builder.build().map_err(|e| ScraperError::Browser {
        reason: format!("Failed to configure browser: {e}"),
    })?;

    let (browser, mut handler) =
        Browser::launch(browser_config)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to launch browser: {e}"),
            })?;

    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            debug!(?event, "Browser event");
        }
    });

    Ok(browser)
}

/// Poll the browser page until the user has completed login.
/// Uses the provider's configured URL patterns to detect success/failure.
async fn wait_for_login(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
) -> ScraperResult<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(LOGIN_TIMEOUT_SECS);

    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(ScraperError::Auth {
                reason: format!(
                    "Login timed out after {LOGIN_TIMEOUT_SECS} seconds — close the browser and retry"
                ),
            });
        }

        let url = page
            .url()
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to get page URL: {e}"),
            })?
            .unwrap_or_default();

        let on_failure_page = provider
            .provider
            .login_failure_patterns
            .iter()
            .any(|p| url.contains(p));

        let on_success_page = provider
            .provider
            .login_success_patterns
            .iter()
            .any(|p| url.contains(p));

        if !url.is_empty() && !on_failure_page && on_success_page {
            info!(url = %url, "Login detected");
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(LOGIN_POLL_INTERVAL_MS)).await;
    }
}

/// Extract cookies from a browser page, filtering by provider domain
async fn extract_cookies(
    page: &chromiumoxide::Page,
    provider_name: &str,
) -> ScraperResult<Vec<CookieData>> {
    let cookies = page
        .get_cookies()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get cookies: {e}"),
        })?;

    let result: Vec<CookieData> = cookies
        .iter()
        .filter(|c| c.domain.contains(provider_name) || !c.domain.is_empty())
        .map(|c| CookieData {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            secure: c.secure,
            http_only: c.http_only,
        })
        .collect();

    debug!(count = result.len(), "Extracted cookies");
    Ok(result)
}

/// Check if the browser was redirected to a login page (session expired)
async fn check_session_redirect(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
) -> ScraperResult<()> {
    let url = page
        .url()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get URL: {e}"),
        })?
        .unwrap_or_default();

    let on_failure = provider
        .provider
        .login_failure_patterns
        .iter()
        .any(|p| url.contains(p));

    if on_failure {
        return Err(ScraperError::SessionExpired {
            reason: "Redirected to login page — session cookies expired, re-login required"
                .to_owned(),
        });
    }
    Ok(())
}

// ============================================================================
// JS extraction (generic, driven by provider config)
// ============================================================================

/// Execute a JS snippet on a page and parse the returned JSON array
async fn extract_via_js(
    page: &chromiumoxide::Page,
    js: &str,
) -> ScraperResult<Vec<serde_json::Value>> {
    let result = page
        .evaluate(js)
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("JS evaluation failed: {e}"),
        })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("[]");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse JS result: {e}"),
    })
}

/// Extract detailed activity data using the provider's configured JS snippet
async fn extract_detail_via_js(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
) -> ScraperResult<serde_json::Value> {
    let result = page
        .evaluate(provider.detail_page.js_extract.as_str())
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to extract activity data: {e}"),
        })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("{}");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse activity detail: {e}"),
    })
}

/// Navigate an existing page to an activity detail URL and extract data
async fn navigate_and_extract_detail(
    page: &chromiumoxide::Page,
    url: &str,
    provider: &ProviderConfig,
    config: &ScraperConfig,
) -> ScraperResult<serde_json::Value> {
    page.goto(url).await.map_err(|e| ScraperError::Browser {
        reason: format!("Failed to navigate to {url}: {e}"),
    })?;

    tokio::time::sleep(Duration::from_millis(config.interaction_delay_ms * 2)).await;
    extract_detail_via_js(page, provider).await
}

// ============================================================================
// Activity construction from scraped data
// ============================================================================

/// Build Activity structs from JS-extracted item list
fn parse_js_activity_items(items: &[serde_json::Value]) -> Vec<Activity> {
    items
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            Some(build_activity_from_js_item(id, item))
        })
        .collect()
}

/// Build a single Activity from a JS-extracted list page row.
/// The list page provides: type, date, name, time, distance, elevation, suffer score.
fn build_activity_from_js_item(id: &str, item: &serde_json::Value) -> Activity {
    let sport_type_str = item["type"].as_str().unwrap_or("");
    Activity {
        id: id.to_owned(),
        name: item["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: if sport_type_str.is_empty() {
            SportType::Other("Unknown".to_owned())
        } else {
            SportType::from_strava(sport_type_str)
        },
        start_date: item["date"]
            .as_str()
            .and_then(parse_strava_date)
            .unwrap_or_else(Utc::now),
        duration_seconds: item["time"]
            .as_str()
            .and_then(parse_duration_string)
            .unwrap_or(0),
        distance_meters: item["distance"].as_str().and_then(parse_distance_string),
        elevation_gain: item["elevation"]
            .as_str()
            .and_then(|e| e.replace([',', 'm'], "").trim().parse().ok()),
        average_heart_rate: None,
        max_heart_rate: None,
        average_speed: None,
        max_speed: None,
        calories: None,
        average_power: None,
        max_power: None,
        normalized_power: None,
        average_cadence: None,
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: item["suffer_score"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: None,
        pace: None,
        gap: None,
        elapsed_time_seconds: None,
        device_name: None,
        gear_name: None,
        perceived_exertion: None,
        workout_type: None,
        sport_type_detail: if sport_type_str.is_empty() {
            None
        } else {
            Some(sport_type_str.to_owned())
        },
        segment_efforts: None,
        provider: "scraper".to_owned(),
    }
}

/// Build an Activity from detailed activity page JS extraction.
/// The detail page JS extracts name, type, distance, moving time, pace, relative effort,
/// elevation, calories, elapsed time, heart rates, power, cadence, temperature,
/// humidity, and wind speed.
fn build_activity_from_detail(activity_id: &str, data: &serde_json::Value) -> Activity {
    Activity {
        id: activity_id.to_owned(),
        name: data["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: data["type"].as_str().map_or_else(
            || SportType::Other("Unknown".to_owned()),
            SportType::from_strava,
        ),
        start_date: data["date"]
            .as_str()
            .and_then(parse_strava_date)
            .unwrap_or_else(Utc::now),
        duration_seconds: data["moving_time"]
            .as_str()
            .or_else(|| data["elapsed_time"].as_str())
            .and_then(parse_duration_string)
            .unwrap_or(0),
        distance_meters: data["distance"].as_str().and_then(parse_distance_string),
        elevation_gain: data["elevation"]
            .as_str()
            .and_then(|e| e.replace([',', ' '], "").trim().parse().ok()),
        average_heart_rate: data["avg_hr"]
            .as_str()
            .and_then(|h| h.replace("bpm", "").trim().parse().ok()),
        max_heart_rate: data["max_hr"]
            .as_str()
            .and_then(|h| h.replace("bpm", "").trim().parse().ok()),
        average_speed: data["avg_speed"].as_str().and_then(parse_speed_string),
        max_speed: None,
        calories: data["calories"]
            .as_str()
            .and_then(|c| c.replace(',', "").trim().parse().ok()),
        average_power: data["avg_power"]
            .as_str()
            .and_then(|p| p.replace(['W', 'w'], "").trim().parse().ok()),
        max_power: None,
        normalized_power: None,
        average_cadence: data["cadence"]
            .as_str()
            .and_then(|c| c.replace("rpm", "").replace("spm", "").trim().parse().ok()),
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: data["relative_effort"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: None,
        pace: data["pace"].as_str().map(String::from),
        gap: data["gap"].as_str().map(String::from),
        elapsed_time_seconds: data["elapsed_time"]
            .as_str()
            .and_then(parse_duration_string),
        device_name: data["device"].as_str().map(String::from),
        gear_name: data["gear"].as_str().map(String::from),
        perceived_exertion: data["perceived_exertion"].as_str().map(String::from),
        workout_type: None,
        sport_type_detail: data["type"].as_str().map(String::from),
        segment_efforts: None,
        provider: "scraper".to_owned(),
    }
}

/// Merge detail page data into an activity already populated from the list page
fn merge_detail_into_activity(activity: &mut Activity, detail: &serde_json::Value) {
    // Sport type from the detail page heading (more accurate than list page table)
    if let Some(sport) = detail["type"].as_str() {
        let parsed = SportType::from_strava(sport);
        if !matches!(parsed, SportType::Other(_)) {
            activity.sport_type = parsed;
            activity.sport_type_detail = Some(sport.to_owned());
        }
    }

    // Location from the detail page date line
    if let Some(location) = detail["location"].as_str() {
        let parts: Vec<&str> = location.split(',').map(str::trim).collect();
        if let Some(city) = parts.first() {
            activity.city = Some((*city).to_owned());
        }
        if let Some(region) = parts.get(1) {
            activity.region = Some((*region).to_owned());
        }
    }

    merge_optional_u32(&mut activity.average_heart_rate, detail, "avg_hr", &["bpm"]);
    merge_optional_u32(&mut activity.max_heart_rate, detail, "max_hr", &["bpm"]);
    merge_optional_u32(
        &mut activity.average_cadence,
        detail,
        "cadence",
        &["rpm", "spm", "ppm"],
    );
    merge_optional_u32(&mut activity.calories, detail, "calories", &[","]);
    merge_optional_u32(&mut activity.suffer_score, detail, "relative_effort", &[]);
    merge_optional_u32(
        &mut activity.average_power,
        detail,
        "avg_power",
        &["W", "w"],
    );

    merge_optional_string(&mut activity.pace, detail, "pace");
    merge_optional_string(&mut activity.gap, detail, "gap");
    merge_optional_string(&mut activity.weather, detail, "weather");
    merge_optional_string(&mut activity.wind_direction, detail, "wind_direction");
    merge_optional_string(&mut activity.device_name, detail, "device");
    merge_optional_string(&mut activity.gear_name, detail, "gear");
    merge_optional_string(
        &mut activity.perceived_exertion,
        detail,
        "perceived_exertion",
    );

    merge_optional_f32(
        &mut activity.temperature,
        detail,
        "temperature",
        &["°", "℃", "C"],
    );
    merge_optional_f32(
        &mut activity.feels_like,
        detail,
        "feels_like",
        &["°", "℃", "C"],
    );
    merge_optional_f32(&mut activity.humidity, detail, "humidity", &["%"]);
    merge_optional_f32(&mut activity.wind_speed, detail, "wind_speed", &["km/h"]);

    if activity.elapsed_time_seconds.is_none() {
        activity.elapsed_time_seconds = detail["elapsed_time"]
            .as_str()
            .and_then(parse_duration_string);
    }
}

/// Merge an optional u32 field from detail JSON, stripping given suffixes
fn merge_optional_u32(
    field: &mut Option<u32>,
    data: &serde_json::Value,
    key: &str,
    strip: &[&str],
) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().and_then(|v| {
        let mut s = v.to_owned();
        for suffix in strip {
            s = s.replace(suffix, "");
        }
        s.trim().parse().ok()
    });
}

/// Merge an optional f32 field from detail JSON, stripping given suffixes
fn merge_optional_f32(
    field: &mut Option<f32>,
    data: &serde_json::Value,
    key: &str,
    strip: &[&str],
) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().and_then(|v| {
        let mut s = v.to_owned();
        for suffix in strip {
            s = s.replace(suffix, "");
        }
        s.trim().parse().ok()
    });
}

/// Merge an optional String field from detail JSON
fn merge_optional_string(field: &mut Option<String>, data: &serde_json::Value, key: &str) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().map(String::from);
}

/// Apply sport type and limit filters to an activity list
fn apply_activity_filters(activities: &mut Vec<Activity>, params: &ActivityParams) {
    if let Some(ref sport) = params.sport_type {
        let sport_lower = sport.to_lowercase();
        activities.retain(|a| {
            a.sport_type
                .display_name()
                .to_lowercase()
                .contains(&sport_lower)
                || a.sport_type_detail
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&sport_lower))
        });
    }

    if let Some(limit) = params.limit {
        activities.truncate(limit as usize);
    }
}

// ============================================================================
// String parsing helpers
// ============================================================================

/// Parse date strings from various formats (handles day-of-week prefix like "Wed, 3/18/2026")
fn parse_strava_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Strip day-of-week prefix like "Wed, " or "Mon, "
    let s = if s.len() > 5 && s.chars().nth(3) == Some(',') {
        s[5..].trim()
    } else {
        s
    };

    let formats = [
        "%m/%d/%Y",
        "%Y-%m-%d",
        "%B %d, %Y",
        "%b %d, %Y",
        "%Y-%m-%dT%H:%M:%S",
    ];

    for fmt in &formats {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(ndt.and_utc());
        }
        if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, fmt) {
            return nd.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
        }
    }

    None
}

/// Parse duration strings like "1:23:45" or "45:30" into seconds
fn parse_duration_string(s: &str) -> Option<u64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();

    match parts.len() {
        3 => {
            let hours: u64 = parts[0].parse().ok()?;
            let mins: u64 = parts[1].parse().ok()?;
            let secs: u64 = parts[2].parse().ok()?;
            Some(hours * 3600 + mins * 60 + secs)
        }
        2 => {
            let mins: u64 = parts[0].parse().ok()?;
            let secs: u64 = parts[1].parse().ok()?;
            Some(mins * 60 + secs)
        }
        1 => parts[0].parse().ok(),
        _ => None,
    }
}

/// Parse distance strings like "5.2 km" or "3.1 mi" into meters
fn parse_distance_string(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();

    if s.contains("km") {
        let num_str = s.replace("km", "").replace(',', "").trim().to_owned();
        let km: f64 = num_str.parse().ok()?;
        Some(km * 1000.0)
    } else if s.contains("mi") {
        let num_str = s.replace("mi", "").replace(',', "").trim().to_owned();
        let mi: f64 = num_str.parse().ok()?;
        Some(mi * 1609.344)
    } else if s.contains('m') {
        let num_str = s.replace(['m', ','], "").trim().to_owned();
        num_str.parse().ok()
    } else {
        s.replace(',', "").parse().ok()
    }
}

/// Parse speed strings like "10 km/h" or "6.2 mph" into m/s
fn parse_speed_string(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();
    if s.contains("km/h") || s.contains("kph") {
        let num: f64 = s
            .replace("km/h", "")
            .replace("kph", "")
            .trim()
            .parse()
            .ok()?;
        Some(num / 3.6)
    } else if s.contains("mph") {
        let num: f64 = s.replace("mph", "").trim().parse().ok()?;
        Some(num * 0.447_04)
    } else {
        s.parse().ok()
    }
}

/// Generate a unique session identifier using system time
fn generate_session_id() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration() {
        assert_eq!(parse_duration_string("1:23:45"), Some(5025));
        assert_eq!(parse_duration_string("45:30"), Some(2730));
        assert_eq!(parse_duration_string("3600"), Some(3600));
        assert_eq!(parse_duration_string(""), None);
    }

    #[test]
    fn parse_distance() {
        let d = parse_distance_string("5.2 km").unwrap();
        assert!((d - 5200.0).abs() < 1.0);

        let d = parse_distance_string("3.1 mi").unwrap();
        assert!((d - 4988.967).abs() < 1.0);

        let d = parse_distance_string("800m").unwrap();
        assert!((d - 800.0).abs() < 1.0);
    }

    #[test]
    fn parse_speed() {
        let s = parse_speed_string("10 km/h").unwrap();
        assert!((s - 2.7778).abs() < 0.01);

        let s = parse_speed_string("6.2 mph").unwrap();
        assert!((s - 2.7716).abs() < 0.01);
    }

    #[test]
    fn parse_date() {
        assert!(parse_strava_date("2024-03-15").is_some());
        assert!(parse_strava_date("March 15, 2024").is_some());
        assert!(parse_strava_date("Wed, 3/18/2026").is_some());
        assert!(parse_strava_date("garbage").is_none());
    }

    #[test]
    fn parse_date_with_weekday_prefix() {
        assert!(parse_strava_date("Wed, 3/18/2026").is_some());
        assert!(parse_strava_date("Mon, 1/5/2025").is_some());
    }
}

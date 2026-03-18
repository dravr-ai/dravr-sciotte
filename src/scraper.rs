// ABOUTME: Chromiumoxide-based Strava training page scraper
// ABOUTME: Implements StravaScraper trait using headless Chrome via CDP
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
use crate::types::StravaScraper;

const STRAVA_LOGIN_URL: &str = "https://www.strava.com/login";
const STRAVA_TRAINING_URL: &str = "https://www.strava.com/athlete/training";
const STRAVA_ACTIVITY_URL: &str = "https://www.strava.com/activities";
const LOGIN_POLL_INTERVAL_MS: u64 = 500;
const LOGIN_TIMEOUT_SECS: u64 = 120;

/// Chrome-based Strava scraper implementing the `StravaScraper` trait
pub struct ChromeScraper {
    config: ScraperConfig,
    /// Shared browser instance for headless scraping (lazily created)
    browser: Mutex<Option<Arc<Browser>>>,
}

impl ChromeScraper {
    /// Create a new Chrome scraper with the given configuration
    #[must_use]
    pub fn new(config: ScraperConfig) -> Self {
        Self {
            config,
            browser: Mutex::new(None),
        }
    }

    /// Create with default configuration
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(ScraperConfig::default())
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

        // Navigate to strava.com first so cookies can be set on the domain
        let page = browser
            .new_page(STRAVA_LOGIN_URL)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms)).await;

        self.inject_cookies(&page, session).await?;

        // Now navigate to the actual target URL with cookies set
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
        info!("Launching visible browser for Strava login");

        let browser = launch_browser(&self.config, false).await?;
        let page = browser
            .new_page(STRAVA_LOGIN_URL)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open login page: {e}"),
            })?;

        info!("Waiting for user to log in to Strava...");
        wait_for_login(&page).await?;

        let cookies = extract_strava_cookies(&page).await?;
        if cookies.is_empty() {
            return Err(ScraperError::Auth {
                reason: "No Strava cookies captured after login".to_owned(),
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
            .open_authenticated_page(session, STRAVA_TRAINING_URL)
            .await?;

        check_session_redirect(&page).await?;

        let html = page.content().await.map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to get page content: {e}"),
        })?;

        let activities = match extract_activities_via_js(&page).await {
            Ok(items) if !items.is_empty() => {
                debug!(count = items.len(), "Extracted activities via JS");
                let mut parsed = parse_js_activity_items(&items);
                apply_activity_filters(&mut parsed, params);
                parsed
            }
            Ok(_) => {
                warn!("JS extraction returned empty, parsing HTML instead");
                parse_activity_rows_from_html(&html, params)
            }
            Err(e) => {
                warn!(error = %e, "JS extraction failed, parsing HTML instead");
                parse_activity_rows_from_html(&html, params)
            }
        };

        info!(count = activities.len(), "Activities scraped");
        Ok(activities)
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let url = format!("{STRAVA_ACTIVITY_URL}/{activity_id}");
        info!(url = %url, "Navigating to activity detail page");

        let page = self.open_authenticated_page(session, &url).await?;
        let data = extract_activity_detail_via_js(&page).await?;
        let activity = build_activity_from_detail(activity_id, &data);

        info!(id = activity_id, name = %activity.name, "Activity detail scraped");
        Ok(activity)
    }
}

// ============================================================================
// Browser login helpers
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

/// Poll the browser page until the user has completed Strava login.
/// Detects login completion by checking if the URL moves away from /login.
async fn wait_for_login(page: &chromiumoxide::Page) -> ScraperResult<()> {
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

        // User is logged in when they're redirected away from login/session pages
        let is_logged_in = !url.is_empty()
            && !url.contains("/login")
            && !url.contains("/session")
            && !url.contains("/oauth")
            && (url.contains("/dashboard")
                || url.contains("/athlete")
                || url.contains("/onboarding")
                || url.contains("strava.com/feed"));

        if is_logged_in {
            info!(url = %url, "Login detected, user redirected to Strava");
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(LOGIN_POLL_INTERVAL_MS)).await;
    }
}

/// Extract Strava cookies from a browser page
async fn extract_strava_cookies(page: &chromiumoxide::Page) -> ScraperResult<Vec<CookieData>> {
    let cookies = page
        .get_cookies()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get cookies: {e}"),
        })?;

    let result: Vec<CookieData> = cookies
        .iter()
        .filter(|c| c.domain.contains("strava.com"))
        .map(|c| CookieData {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            secure: c.secure,
            http_only: c.http_only,
        })
        .collect();

    debug!(count = result.len(), "Extracted Strava cookies");
    Ok(result)
}

/// Check if the browser was redirected to a login page (session expired)
async fn check_session_redirect(page: &chromiumoxide::Page) -> ScraperResult<()> {
    let url = page
        .url()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get URL: {e}"),
        })?
        .unwrap_or_default();

    if url.contains("/login") || url.contains("/session") {
        return Err(ScraperError::SessionExpired {
            reason: "Redirected to login page — session cookies expired, re-login required"
                .to_owned(),
        });
    }
    Ok(())
}

// ============================================================================
// JS extraction
// ============================================================================

/// JavaScript snippet to extract activity rows from the training page
const JS_EXTRACT_ACTIVITIES: &str = r#"
(function() {
    const rows = document.querySelectorAll(
        '[data-react-class="ActivityList"] tr, .training-activity-row, table.activities tbody tr'
    );
    const activities = [];
    rows.forEach(function(row) {
        const link = row.querySelector('a[href*="/activities/"]');
        if (!link) return;
        const href = link.getAttribute('href') || '';
        const idMatch = href.match(/\/activities\/(\d+)/);
        if (!idMatch) return;
        const cells = row.querySelectorAll('td');
        const getText = (idx) => cells[idx] ? cells[idx].textContent.trim() : '';
        activities.push({
            id: idMatch[1],
            name: link.textContent.trim() || getText(0),
            type: row.getAttribute('data-activity-type') || getText(1),
            date: row.getAttribute('data-date') || getText(2),
            distance: getText(3),
            time: getText(4),
            elevation: getText(5),
        });
    });
    return JSON.stringify(activities);
})()
"#;

/// JavaScript snippet to extract detailed activity data from a single activity page
const JS_EXTRACT_ACTIVITY_DETAIL: &str = r#"
(function() {
    const data = {};
    const title = document.querySelector(
        'h1.activity-name, .activity-name h1, [data-testid="activity_name"]'
    );
    data.name = title ? title.textContent.trim() : '';
    const typeEl = document.querySelector(
        '[data-testid="activity_type"], .activity-type-icon'
    );
    data.type = typeEl ? (typeEl.getAttribute('title') || typeEl.textContent.trim()) : '';
    const stats = document.querySelectorAll(
        '.inline-stats li, [class*="stat"], .activity-stats .stat'
    );
    stats.forEach(function(stat) {
        const label = stat.querySelector('.label, .stat-label, dt');
        const value = stat.querySelector('.stat-text, .stat-value, dd, strong');
        if (label && value) {
            data[label.textContent.trim().toLowerCase()] = value.textContent.trim();
        }
    });
    return JSON.stringify(data);
})()
"#;

/// Extract activity items from the training page via JS evaluation
async fn extract_activities_via_js(
    page: &chromiumoxide::Page,
) -> ScraperResult<Vec<serde_json::Value>> {
    let result =
        page.evaluate(JS_EXTRACT_ACTIVITIES)
            .await
            .map_err(|e| ScraperError::Scraping {
                reason: format!("JS evaluation failed: {e}"),
            })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("[]");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse JS result: {e}"),
    })
}

/// Extract detailed activity data from a single activity page via JS evaluation
async fn extract_activity_detail_via_js(
    page: &chromiumoxide::Page,
) -> ScraperResult<serde_json::Value> {
    let result = page
        .evaluate(JS_EXTRACT_ACTIVITY_DETAIL)
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to extract activity data: {e}"),
        })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("{}");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse activity detail: {e}"),
    })
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

/// Build a single Activity from a JS-extracted training row item
fn build_activity_from_js_item(id: &str, item: &serde_json::Value) -> Activity {
    Activity {
        id: id.to_owned(),
        name: item["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: item["type"].as_str().map_or_else(
            || SportType::Other("Unknown".to_owned()),
            SportType::from_strava,
        ),
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
        suffer_score: None,
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        workout_type: None,
        sport_type_detail: item["type"].as_str().map(String::from),
        segment_efforts: None,
        provider: "strava-scraper".to_owned(),
    }
}

/// Build an Activity from detailed activity page JS extraction
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
        duration_seconds: data["time"]
            .as_str()
            .or_else(|| data["elapsed time"].as_str())
            .or_else(|| data["moving time"].as_str())
            .and_then(parse_duration_string)
            .unwrap_or(0),
        distance_meters: data["distance"].as_str().and_then(parse_distance_string),
        elevation_gain: data["elevation"]
            .as_str()
            .or_else(|| data["elev gain"].as_str())
            .and_then(|e| e.replace([',', 'm'], "").trim().parse().ok()),
        average_heart_rate: parse_hr_field(data, "avg hr")
            .or_else(|| parse_hr_field(data, "heart rate")),
        max_heart_rate: parse_hr_field(data, "max hr"),
        average_speed: data["avg speed"].as_str().and_then(parse_speed_string),
        max_speed: data["max speed"].as_str().and_then(parse_speed_string),
        calories: data["calories"]
            .as_str()
            .and_then(|c| c.replace(',', "").trim().parse().ok()),
        average_power: parse_power_field(data, "avg power")
            .or_else(|| parse_power_field(data, "power")),
        max_power: parse_power_field(data, "max power"),
        normalized_power: None,
        average_cadence: data["cadence"]
            .as_str()
            .or_else(|| data["avg cadence"].as_str())
            .and_then(|c| c.replace("rpm", "").replace("spm", "").trim().parse().ok()),
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: data["relative effort"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        workout_type: None,
        sport_type_detail: data["type"].as_str().map(String::from),
        segment_efforts: None,
        provider: "strava-scraper".to_owned(),
    }
}

/// Parse a heart rate field from scraped data, stripping "bpm" suffix
fn parse_hr_field(data: &serde_json::Value, key: &str) -> Option<u32> {
    data[key]
        .as_str()
        .and_then(|h| h.replace("bpm", "").trim().parse().ok())
}

/// Parse a power field from scraped data, stripping "W"/"w" suffix
fn parse_power_field(data: &serde_json::Value, key: &str) -> Option<u32> {
    data[key]
        .as_str()
        .and_then(|p| p.replace(['W', 'w'], "").trim().parse().ok())
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
// HTML parsing (used when JS extraction is unavailable)
// ============================================================================

/// Parse activity rows from training page HTML content
fn parse_activity_rows_from_html(html: &str, params: &ActivityParams) -> Vec<Activity> {
    let mut activities = Vec::new();

    for line in html.lines() {
        let line = line.trim();
        if !line.contains("/activities/") || !line.contains("data-field-name") {
            continue;
        }

        if let Some(id) = extract_between(line, "/activities/", "\"") {
            if id.chars().all(|c| c.is_ascii_digit()) {
                activities.push(build_activity_stub(id));
            }
        }
    }

    apply_activity_filters(&mut activities, params);
    activities
}

/// Build a minimal Activity stub from an HTML-extracted activity ID
fn build_activity_stub(id: &str) -> Activity {
    Activity {
        id: id.to_owned(),
        name: "Untitled".to_owned(),
        sport_type: SportType::Other("Unknown".to_owned()),
        start_date: Utc::now(),
        duration_seconds: 0,
        distance_meters: None,
        elevation_gain: None,
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
        suffer_score: None,
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        workout_type: None,
        sport_type_detail: None,
        segment_efforts: None,
        provider: "strava-scraper".to_owned(),
    }
}

// ============================================================================
// String parsing helpers
// ============================================================================

/// Extract the substring between two delimiters
fn extract_between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = s.find(start)? + start.len();
    let rest = &s[start_idx..];
    let end_idx = rest.find(end)?;
    Some(&rest[..end_idx])
}

/// Parse Strava date strings (various formats)
fn parse_strava_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    let formats = [
        "%Y-%m-%d",
        "%B %d, %Y",
        "%b %d, %Y",
        "%m/%d/%Y",
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
        assert!(parse_strava_date("garbage").is_none());
    }

    #[test]
    fn extract_between_works() {
        assert_eq!(
            extract_between("href=\"/activities/123\"", "/activities/", "\""),
            Some("123")
        );
        assert_eq!(extract_between("no match", "foo", "bar"), None);
    }
}

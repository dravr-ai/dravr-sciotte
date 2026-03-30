// ABOUTME: Exploration test for Garmin Connect health pages (sleep, recovery, body battery)
// ABOUTME: Validates whether sciotte's authenticated session can access health-specific endpoints

use std::env;
use std::time::Duration;

use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use dravr_sciotte::auth::{load_session, save_session};
use dravr_sciotte::browser_utils::{inject_cookies, launch_browser};
use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, ChromeScraper};

/// Garmin SSO login URL — used as initial navigation target before cookie injection
const GARMIN_LOGIN_URL: &str = "https://sso.garmin.com/portal/sso/en-US/sign-in?clientId=GarminConnect&service=https%3A%2F%2Fconnect.garmin.com%2Fmodern";

/// Health page URLs to probe — both /modern/ (legacy) and /app/ (current) paths
const HEALTH_PAGES: &[(&str, &str)] = &[
    ("sleep-modern", "https://connect.garmin.com/modern/sleep"),
    ("sleep-app", "https://connect.garmin.com/app/sleep"),
    (
        "daily-summary-modern",
        "https://connect.garmin.com/modern/daily-summary",
    ),
    (
        "daily-summary-app",
        "https://connect.garmin.com/app/daily-summary",
    ),
];

/// Garmin internal API endpoints — both legacy /proxy/ and current /gc-api/ prefix.
/// The /gc-api/ endpoints were discovered via Performance API network capture.
const API_ENDPOINTS: &[&str] = &[
    // Legacy /proxy/ prefix (returns empty {} — deprecated)
    "https://connect.garmin.com/proxy/wellness-service/wellness/dailySummary/latest",
    // Current /gc-api/ prefix
    "https://connect.garmin.com/gc-api/wellness-service/wellness/syncTimestamp",
    "https://connect.garmin.com/gc-api/usersummary-service/usersummary/dailySummariesCount",
    // Date-parameterized variants filled at runtime — see test_api_calls()
];

/// JS to extract sleep metrics from the sleep page DOM
const SLEEP_EXTRACT_JS: &str = r#"(function() {
    var data = {};
    var score = document.querySelector('[class*="sleepScore"], [class*="sleep-score"], [class*="Score"]');
    if (score) data.sleep_score = score.textContent.trim();
    var duration = document.querySelector('[class*="sleepDuration"], [class*="totalSleep"], [class*="duration"]');
    if (duration) data.duration = duration.textContent.trim();
    var stages = document.querySelectorAll('[class*="sleepStage"], [class*="stage"]');
    data.stages = [];
    stages.forEach(function(s) { data.stages.push(s.textContent.trim()); });
    data.page_title = document.title;
    data.body_text_length = document.body.textContent.length;
    return JSON.stringify(data);
})()"#;

/// JS to capture API URLs from the Performance API after page load
const NETWORK_CAPTURE_JS: &str = r"JSON.stringify(
    performance.getEntriesByType('resource')
        .filter(function(e) {
            return e.name.includes('proxy') || e.name.includes('wellness')
                || e.name.includes('usersummary') || e.name.includes('sleep')
                || e.name.includes('health');
        })
        .map(function(e) { return { url: e.name, type: e.initiatorType }; })
)";

/// JS to extract daily summary metrics (body battery, stress, HR, steps, sleep)
const DAILY_SUMMARY_JS: &str = r#"(function() {
    var data = {};
    var bb = document.querySelector('[class*="bodyBattery"], [class*="body-battery"], [class*="BodyBattery"]');
    if (bb) data.body_battery = bb.textContent.trim();
    var stress = document.querySelector('[class*="stress"], [class*="Stress"]');
    if (stress) data.stress = stress.textContent.trim();
    var hr = document.querySelector('[class*="heartRate"], [class*="heart-rate"], [class*="HeartRate"]');
    if (hr) data.heart_rate = hr.textContent.trim();
    var steps = document.querySelector('[class*="steps"], [class*="Steps"]');
    if (steps) data.steps = steps.textContent.trim();
    var sleep = document.querySelector('[class*="sleep"], [class*="Sleep"]');
    if (sleep) data.sleep_summary = sleep.textContent.trim();
    data.page_title = document.title;
    data.body_text_length = document.body.textContent.length;
    return JSON.stringify(data);
})()"#;

/// JS to probe the daily summary DOM structure — dumps class names, text, and hierarchy
/// for selector discovery
const DOM_PROBE_JS: &str = r#"(function() {
    var result = {};

    // Find all elements with data-testid or meaningful class patterns
    var widgets = document.querySelectorAll('[class*="Widget"], [class*="widget"], [class*="Card"], [class*="card"], [class*="Summary"], [class*="summary"], [data-testid]');
    result.widgets = [];
    widgets.forEach(function(w) {
        var text = w.textContent.trim().substring(0, 200);
        if (text.length > 5) {
            result.widgets.push({
                tag: w.tagName,
                class: w.className.substring(0, 150),
                testid: w.getAttribute('data-testid') || '',
                text_preview: text
            });
        }
    });

    // Find numeric values with units (likely health metrics)
    var allElements = document.querySelectorAll('span, div, p, h1, h2, h3, h4');
    result.metric_candidates = [];
    var seen = {};
    allElements.forEach(function(el) {
        var t = el.textContent.trim();
        if (t.length < 50 && /\d+/.test(t) && !seen[t]) {
            var cls = el.className || '';
            if (typeof cls !== 'string') cls = cls.toString();
            if (cls.length > 0 || el.getAttribute('data-testid')) {
                seen[t] = true;
                result.metric_candidates.push({
                    tag: el.tagName,
                    class: cls.substring(0, 100),
                    testid: el.getAttribute('data-testid') || '',
                    text: t,
                    parent_class: (el.parentElement && el.parentElement.className || '').substring(0, 100)
                });
            }
        }
    });

    // Dump the top-level page sections
    var sections = document.querySelectorAll('section, [role="region"], main > div > div');
    result.sections = [];
    sections.forEach(function(s) {
        var cls = s.className || '';
        if (typeof cls !== 'string') cls = cls.toString();
        result.sections.push({
            tag: s.tagName,
            class: cls.substring(0, 150),
            child_count: s.children.length,
            text_preview: s.textContent.trim().substring(0, 100)
        });
    });

    return JSON.stringify(result, null, 2);
})()"#;

// ============================================================================
// Helpers
// ============================================================================

/// Save a PNG screenshot to /tmp and log the path
async fn take_screenshot(page: &chromiumoxide::Page, label: &str) {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .build();
    match page.screenshot(params).await {
        Ok(data) => {
            let path = format!("/tmp/sciotte-garmin-{label}.png");
            if tokio::fs::write(&path, &data).await.is_ok() {
                eprintln!("  Screenshot: {path} ({} bytes)", data.len());
            }
        }
        Err(e) => eprintln!("  Screenshot failed: {e}"),
    }
}

/// Open a new tab with injected session cookies and navigate to `url`.
/// Mirrors `ChromeScraper::open_authenticated_page`: navigate to login domain
/// first so cookies bind to the right domain, then redirect to target.
async fn open_page(
    browser: &chromiumoxide::browser::Browser,
    session: &AuthSession,
    url: &str,
    wait_secs: u64,
) -> chromiumoxide::Page {
    let page = browser
        .new_page(GARMIN_LOGIN_URL)
        .await
        .expect("open browser tab");
    tokio::time::sleep(Duration::from_millis(500)).await;
    inject_cookies(&page, session)
        .await
        .expect("inject cookies");
    page.goto(url).await.expect("navigate to target URL");
    tokio::time::sleep(Duration::from_secs(wait_secs)).await;
    page
}

/// Evaluate JavaScript on a page and return the result as a string
async fn eval_js(page: &chromiumoxide::Page, js: &str) -> String {
    match page.evaluate(js).await {
        Ok(r) => match r.value().cloned().unwrap_or(serde_json::Value::Null) {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        },
        Err(e) => format!("error: {e}"),
    }
}

/// Authenticate with Garmin Connect.
///
/// Tries in order:
/// 1. Reuse a saved encrypted session from disk
/// 2. Credential login with `GARMIN_EMAIL` + `GARMIN_PASSWORD` env vars
/// 3. Manual browser login (opens a visible Chrome window)
async fn get_session(scraper: &ChromeScraper) -> AuthSession {
    // Try saved session
    if let Ok(Some(session)) = load_session().await {
        if scraper.is_authenticated(&session).await {
            eprintln!("Reusing saved session ({})", session.session_id);
            return session;
        }
        eprintln!("Saved session expired");
    }

    // Try credential login
    if let (Ok(email), Ok(password)) = (env::var("GARMIN_EMAIL"), env::var("GARMIN_PASSWORD")) {
        eprintln!("Authenticating via credentials...");
        match scraper.credential_login(&email, &password, "email").await {
            Ok(LoginResult::Success(session)) => {
                save_session(&session).await.ok();
                eprintln!("Credential login succeeded");
                return session;
            }
            Ok(LoginResult::OtpRequired) => {
                eprintln!("OTP required, falling back to browser login");
            }
            Ok(other) => {
                eprintln!("Credential login: {other:?}, falling back to browser login");
            }
            Err(e) => {
                eprintln!("Credential login failed: {e}, falling back to browser login");
            }
        }
    }

    // Manual browser login
    eprintln!("Opening visible browser for manual Garmin login...");
    let session = scraper.browser_login().await.expect("browser login");
    save_session(&session).await.ok();
    session
}

/// Make an API call with session cookies and optional bearer token, log the result
async fn api_call(
    client: &reqwest::Client,
    url: &str,
    cookie_header: &str,
    bearer_token: Option<&str>,
) {
    eprintln!("\n  GET {url}");
    let mut req = client
        .get(url)
        .header("Cookie", cookie_header)
        .header("Referer", "https://connect.garmin.com/app/daily-summary")
        .header("NK", "NT");

    if let Some(token) = bearer_token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let preview_len = body.len().min(500);
            eprintln!("  Status: {status}");
            eprintln!("  Body ({} chars): {}", body.len(), &body[..preview_len]);
        }
        Err(e) => eprintln!("  Failed: {e}"),
    }
}

/// Validate the session by navigating to the known-good activities page
async fn validate_session(browser: &chromiumoxide::browser::Browser, session: &AuthSession) {
    eprintln!("== SESSION VALIDATION ==");
    let page = open_page(
        browser,
        session,
        "https://connect.garmin.com/modern/activities",
        5,
    )
    .await;
    let url = eval_js(&page, "window.location.href").await;
    let title = eval_js(&page, "document.title").await;
    eprintln!("  Activities page URL:   {url}");
    eprintln!("  Activities page title: {title}");
    take_screenshot(&page, "activities-validation").await;

    let valid = !url.contains("sso.garmin.com") && !title.contains("moment");
    eprintln!(
        "  Session valid: {valid}{}",
        if valid {
            ""
        } else {
            " (redirected to SSO or Cloudflare challenge)"
        }
    );
}

/// Test 1: Navigate to each health page and report whether it loaded
async fn test_health_navigation(browser: &chromiumoxide::browser::Browser, session: &AuthSession) {
    eprintln!("\n== TEST 1: Health page navigation ==");
    for (name, url) in HEALTH_PAGES {
        let page = open_page(browser, session, url, 5).await;
        let landed = eval_js(&page, "window.location.href").await;
        let redirected = landed.contains("sso.garmin.com");
        let blocked = eval_js(&page, "document.title").await.contains("moment");
        eprintln!("\n  [{name}]");
        eprintln!("  Requested: {url}");
        eprintln!("  Landed:    {landed}");
        eprintln!(
            "  Status:    {}",
            if redirected {
                "REDIRECTED to SSO"
            } else if blocked {
                "BLOCKED by Cloudflare"
            } else {
                "LOADED"
            }
        );
        let info = eval_js(
            &page,
            r"JSON.stringify({title: document.title, body_len: document.body.textContent.length})",
        )
        .await;
        eprintln!("  Info: {info}");
        take_screenshot(&page, name).await;
    }
}

/// Tests 2-4: JS extraction on sleep and daily summary pages
async fn test_js_extraction(browser: &chromiumoxide::browser::Browser, session: &AuthSession) {
    eprintln!("\n== TEST 2: Sleep data extraction ==");
    let sleep_page = open_page(browser, session, "https://connect.garmin.com/app/sleep", 6).await;
    eprintln!("  Result: {}", eval_js(&sleep_page, SLEEP_EXTRACT_JS).await);
    take_screenshot(&sleep_page, "sleep-extracted").await;

    eprintln!("\n== TEST 3: Network request capture ==");
    let ds_page = open_page(
        browser,
        session,
        "https://connect.garmin.com/app/daily-summary",
        8,
    )
    .await;
    eprintln!(
        "  API URLs: {}",
        eval_js(&ds_page, NETWORK_CAPTURE_JS).await
    );
    take_screenshot(&ds_page, "daily-summary-network").await;

    eprintln!("\n== TEST 4: Daily summary extraction ==");
    eprintln!("  Result: {}", eval_js(&ds_page, DAILY_SUMMARY_JS).await);

    // Deep DOM probe — dump widget structure for selector discovery
    eprintln!("\n== TEST 4b: DOM structure probe ==");
    eprintln!("{}", eval_js(&ds_page, DOM_PROBE_JS).await);
}

/// Test 5: Direct HTTP API calls with extracted session cookies
async fn test_api_calls(session: &AuthSession) {
    eprintln!("\n== TEST 5: Direct API calls ==");
    let cookie_header: String = session
        .cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ");

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let client = reqwest::Client::new();

    // Extract JWT_WEB token — /gc-api/ endpoints may require it as Bearer auth
    let jwt = session
        .cookies
        .iter()
        .find(|c| c.name == "JWT_WEB")
        .map(|c| c.value.as_str());
    eprintln!("  JWT_WEB present: {}", jwt.is_some());

    // Extract user GUID from GARMIN-SSO-CUST-GUID cookie
    let guid = session
        .cookies
        .iter()
        .find(|c| c.name == "GARMIN-SSO-CUST-GUID")
        .map(|c| c.value.clone())
        .unwrap_or_default();
    eprintln!("  User GUID: {guid}");

    // Legacy /proxy/ (cookies only — for comparison)
    eprintln!("\n  --- Legacy /proxy/ (cookies only) ---");
    for endpoint in API_ENDPOINTS {
        api_call(&client, endpoint, &cookie_header, None).await;
    }

    // /gc-api/ with cookies only (expect 403)
    let gc_api_url = format!(
        "https://connect.garmin.com/gc-api/usersummary-service/usersummary/daily/{guid}?calendarDate={today}"
    );
    eprintln!("\n  --- /gc-api/ cookies only (expect 403) ---");
    api_call(&client, &gc_api_url, &cookie_header, None).await;

    // /gc-api/ with cookies + JWT Bearer (expect 200)
    eprintln!("\n  --- /gc-api/ cookies + JWT Bearer ---");
    let gc_api_endpoints = [
        format!("https://connect.garmin.com/gc-api/usersummary-service/usersummary/daily/{guid}?calendarDate={today}"),
        format!("https://connect.garmin.com/gc-api/wellnessactivity-service/activity/summary/{today}"),
        format!("https://connect.garmin.com/gc-api/wellness-service/wellness/dailySleepData/{today}"),
        format!("https://connect.garmin.com/gc-api/wellness-service/wellness/dailySummary?calendarDate={today}"),
        "https://connect.garmin.com/gc-api/wellness-service/wellness/syncTimestamp".to_owned(),
    ];
    for endpoint in &gc_api_endpoints {
        api_call(&client, endpoint, &cookie_header, jwt).await;
    }
}

/// Log all cookies in the session for debugging
fn log_cookies(session: &AuthSession) {
    eprintln!("\n== SESSION COOKIES ==");
    for cookie in &session.cookies {
        eprintln!(
            "  {} = {}... (domain: {}, secure: {}, httponly: {})",
            cookie.name,
            &cookie.value[..cookie.value.len().min(20)],
            cookie.domain,
            cookie.secure,
            cookie.http_only
        );
    }
}

// ============================================================================
// Main exploration test
// ============================================================================

/// Explore whether sciotte's authenticated Garmin session can reach health pages.
///
/// Requires either:
/// - A saved session (`~/.config/dravr-sciotte/session.enc`)
/// - Environment variables: `GARMIN_EMAIL` + `GARMIN_PASSWORD`
///
/// Run: `cargo test --test garmin_health -- --nocapture`
#[tokio::test]
async fn explore_garmin_health_pages() {
    let has_creds = env::var("GARMIN_EMAIL").is_ok() && env::var("GARMIN_PASSWORD").is_ok();
    let has_session = matches!(load_session().await, Ok(Some(_)));

    if !has_creds && !has_session {
        eprintln!("Set GARMIN_EMAIL + GARMIN_PASSWORD or save a session first. Skipping.");
        return;
    }

    let provider = ProviderConfig::garmin_default();
    let config = ScraperConfig::default();
    let scraper = ChromeScraper::new(config.clone(), provider);
    let session = get_session(&scraper).await;
    eprintln!("Session: {} cookies\n", session.cookies.len());

    let browser = launch_browser(&config, true)
        .await
        .expect("launch headless browser");

    validate_session(&browser, &session).await;
    test_health_navigation(&browser, &session).await;
    test_js_extraction(&browser, &session).await;
    test_api_calls(&session).await;
    log_cookies(&session);

    eprintln!("\nDone. Screenshots at /tmp/sciotte-garmin-*.png");
}

// ABOUTME: Thin sciotte-specific adapters over the shared dravr-browser automation primitives
// ABOUTME: Delegates launch/stealth/CDP input to dravr-browser; keeps sciotte's cookie-session + cookie-dialog helpers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;

use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chromiumoxide::{Browser, Page};
use chrono::Utc;
use dravr_browser::{BrowserLaunchConfig, StealthOptions};
use tracing::debug;

use crate::config::ScraperConfig;
use crate::error::{ScraperError, ScraperResult};
use crate::models::{AuthSession, CookieData};
use crate::script_loader;

/// JS regex (source) matching the provider data-API responses sciotte captures.
/// The stealth hook tees responses whose request URL matches this pattern into
/// `window.__dravrCaptures`, which provider `js_extract` snippets then read.
const CAPTURE_URL_PATTERN: &str = "activity-service|workout-service|gc-api|training_activities";

/// Launch a Chrome browser for scraping.
///
/// Honors `DRAVR_SCIOTTE_CONNECT_URL` (attach to an externally-launched Chrome
/// via CDP) for backward compatibility, otherwise spawns a new browser against
/// the persistent profile selected by `profile_id`.
pub async fn launch_browser(
    config: &ScraperConfig,
    headless: bool,
    profile_id: Option<&str>,
) -> ScraperResult<Browser> {
    if let Ok(url) = env::var("DRAVR_SCIOTTE_CONNECT_URL") {
        if !url.is_empty() {
            return dravr_browser::connect_browser(&url)
                .await
                .map_err(Into::into);
        }
    }

    // Fields sciotte does not own come from dravr-browser's env-driven defaults
    // (`DRAVR_BROWSER_*`): sandbox off, which is `--no-sandbox` as root in a
    // container, and the launch timeout. Spreading them means a field added
    // upstream does not break this literal — 0.1.1 added two at once.
    let launch = BrowserLaunchConfig {
        chrome_path: config.chrome_path.clone(),
        headless,
        profile_base_dir: config.profile_base_dir.clone(),
        proxy_url: config.proxy_url.clone(),
        user_agent: None,
        ..BrowserLaunchConfig::default()
    };
    dravr_browser::launch_browser(&launch, profile_id)
        .await
        .map_err(Into::into)
}

/// Apply stealth + the sciotte API-capture hook to an already-open page.
///
/// Use when a page is opened separately (e.g. `browser.new_page`) and stealth
/// must be registered before its first navigation.
pub async fn apply_minimal_stealth(page: &Page) -> ScraperResult<()> {
    dravr_browser::apply_stealth(page, &StealthOptions::capture(CAPTURE_URL_PATTERN))
        .await
        .map_err(Into::into)
}

/// Open a new page with stealth + the sciotte API-capture hook applied before
/// navigation, then navigate to `url`.
pub async fn open_page_with_stealth(browser: &Browser, url: &str) -> ScraperResult<Page> {
    dravr_browser::open_page_with_stealth(
        browser,
        url,
        &StealthOptions::capture(CAPTURE_URL_PATTERN),
    )
    .await
    .map_err(Into::into)
}

/// Inject session cookies into a browser page.
pub async fn inject_cookies(page: &Page, session: &AuthSession) -> ScraperResult<()> {
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

/// Capture cookies from the current page and build an `AuthSession`.
pub async fn capture_session(page: &Page) -> ScraperResult<AuthSession> {
    let cookies = page
        .get_cookies()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get cookies: {e}"),
        })?;

    let cookie_data: Vec<CookieData> = cookies
        .iter()
        .map(|c| CookieData {
            name: c.name.clone(),
            value: c.value.clone(),
            domain: c.domain.clone(),
            path: c.path.clone(),
            secure: c.secure,
            http_only: c.http_only,
        })
        .collect();

    if cookie_data.is_empty() {
        return Err(ScraperError::Auth {
            reason: "No cookies captured after login".to_owned(),
        });
    }

    Ok(AuthSession {
        session_id: generate_session_id(),
        cookies: cookie_data,
        created_at: Utc::now(),
        expires_at: None,
    })
}

/// Generate a unique session identifier.
#[must_use]
pub fn generate_session_id() -> String {
    dravr_browser::generate_session_id()
}

/// Click at coordinates (x, y) via CDP mouse press + release.
pub async fn cdp_click_at(page: &Page, x: f64, y: f64) -> ScraperResult<()> {
    dravr_browser::cdp_click_at(page, x, y)
        .await
        .map_err(Into::into)
}

/// Select all text in the focused element and delete it via CDP key events.
pub async fn cdp_select_all_delete(page: &Page) {
    dravr_browser::cdp_select_all_delete(page).await;
}

/// Fill an input field using CDP click-to-focus + `InsertText`.
pub async fn fill_input_field(page: &Page, selector: &str, value: &str) -> ScraperResult<()> {
    dravr_browser::fill_input_field(page, selector, value)
        .await
        .map_err(Into::into)
}

/// Get the center coordinates of an element matching the selector.
pub async fn get_element_center(page: &Page, selector: &str) -> ScraperResult<(f64, f64)> {
    dravr_browser::get_element_center(page, selector)
        .await
        .map_err(Into::into)
}

/// Check if a visible element matching the selector exists in the DOM.
pub async fn element_exists(page: &Page, selector: &str) -> bool {
    dravr_browser::element_exists(page, selector).await
}

/// Click an element matching the given CSS selector (supports `text:` prefix).
pub async fn click_element(page: &Page, selector: &str) -> ScraperResult<()> {
    dravr_browser::click_element(page, selector)
        .await
        .map_err(Into::into)
}

/// Auto-dismiss cookie consent dialogs (`Cookiebot`, `CookieFirst`, generic accept buttons).
pub async fn dismiss_cookie_dialog(page: &Page) {
    let dismiss_js = script_loader::loader().load("dismiss_cookie.js").await;
    if let Ok(result) = page.evaluate(dismiss_js).await {
        let val = result
            .value()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        tracing::info!(result = %val, "Cookie dialog auto-dismiss attempt");
    }
}

/// Read visible text from an element, returning `None` if element is missing or hidden.
pub async fn read_visible_text(page: &Page, selector: &str) -> Option<String> {
    dravr_browser::read_visible_text(page, selector).await
}

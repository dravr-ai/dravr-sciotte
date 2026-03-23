// ABOUTME: Shared browser automation utilities for Chrome CDP interaction
// ABOUTME: Used by both ChromeScraper and VisionScraper for browser launch, cookies, and input
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::network::CookieParam;
use chrono::Utc;
use futures::StreamExt;
use tracing::debug;

use crate::config::ScraperConfig;
use crate::error::{ScraperError, ScraperResult};
use crate::js_utils::escape_js_selector;
use crate::models::{AuthSession, CookieData};

/// Launch a Chrome browser with the given configuration
pub async fn launch_browser(config: &ScraperConfig, headless: bool) -> ScraperResult<Browser> {
    let mut builder = BrowserConfig::builder();

    if headless {
        builder = builder.arg("--headless=new");
    } else {
        builder = builder
            .with_head()
            .arg("--disable-features=WebAuthentication");
    }

    // Use a unique temp profile directory to avoid SingletonLock conflicts
    // when multiple browser instances run concurrently
    let profile_dir = std::env::temp_dir().join(format!(
        "sciotte-chrome-{}",
        std::process::id()
            + std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
    ));

    builder = builder
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-blink-features=AutomationControlled")
        .user_data_dir(profile_dir)
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

/// Inject session cookies into a browser page
pub async fn inject_cookies(
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

/// Capture cookies from the current page and build an `AuthSession`
pub async fn capture_session(page: &chromiumoxide::Page) -> ScraperResult<AuthSession> {
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

/// Generate a unique session identifier using system time
pub fn generate_session_id() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

/// Click at coordinates (x, y) via CDP mouse press + release
pub async fn cdp_click_at(page: &chromiumoxide::Page, x: f64, y: f64) -> ScraperResult<()> {
    let press = DispatchMouseEventParams {
        r#type: DispatchMouseEventType::MousePressed,
        x,
        y,
        modifiers: None,
        timestamp: None,
        button: Some(MouseButton::Left),
        buttons: None,
        click_count: Some(1),
        force: None,
        tangential_pressure: None,
        tilt_x: None,
        tilt_y: None,
        twist: None,
        delta_x: None,
        delta_y: None,
        pointer_type: None,
    };
    page.execute(press)
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("CDP mouse press failed: {e}"),
        })?;

    let release = DispatchMouseEventParams {
        r#type: DispatchMouseEventType::MouseReleased,
        x,
        y,
        modifiers: None,
        timestamp: None,
        button: Some(MouseButton::Left),
        buttons: None,
        click_count: Some(1),
        force: None,
        tangential_pressure: None,
        tilt_x: None,
        tilt_y: None,
        twist: None,
        delta_x: None,
        delta_y: None,
        pointer_type: None,
    };
    page.execute(release)
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("CDP mouse release failed: {e}"),
        })?;
    Ok(())
}

/// Select all text in the focused element and delete it via CDP key events
pub async fn cdp_select_all_delete(page: &chromiumoxide::Page) {
    let select_all = DispatchKeyEventParams {
        r#type: DispatchKeyEventType::KeyDown,
        modifiers: Some(if cfg!(target_os = "macos") { 4 } else { 2 }),
        timestamp: None,
        text: None,
        unmodified_text: None,
        key_identifier: None,
        code: Some("KeyA".to_owned()),
        key: Some("a".to_owned()),
        windows_virtual_key_code: None,
        native_virtual_key_code: None,
        auto_repeat: None,
        is_keypad: None,
        is_system_key: None,
        location: None,
        commands: None,
    };
    let _ = page.execute(select_all).await;

    let backspace = DispatchKeyEventParams {
        r#type: DispatchKeyEventType::KeyDown,
        modifiers: None,
        timestamp: None,
        text: None,
        unmodified_text: None,
        key_identifier: None,
        code: Some("Backspace".to_owned()),
        key: Some("Backspace".to_owned()),
        windows_virtual_key_code: None,
        native_virtual_key_code: None,
        auto_repeat: None,
        is_keypad: None,
        is_system_key: None,
        location: None,
        commands: None,
    };
    let _ = page.execute(backspace).await;
}

/// Fill an input field using CDP click-to-focus + `InsertText`.
///
/// Uses CDP mouse events to click the input (getting native focus), then
/// selects all existing text and replaces it via `Input.insertText`.
pub async fn fill_input_field(
    page: &chromiumoxide::Page,
    selector: &str,
    value: &str,
) -> ScraperResult<()> {
    let (x, y) = get_element_center(page, selector).await?;

    cdp_click_at(page, x, y).await?;
    cdp_select_all_delete(page).await;

    page.execute(InsertTextParams::new(value))
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to insert text into '{selector}': {e}"),
        })?;

    let _ = page
        .evaluate("document.activeElement.dispatchEvent(new Event('change', {bubbles: true}))")
        .await;

    debug!(selector, "Input field filled via CDP InsertText");
    Ok(())
}

/// Get the center coordinates of an element matching the selector
pub async fn get_element_center(
    page: &chromiumoxide::Page,
    selector: &str,
) -> ScraperResult<(f64, f64)> {
    let escaped = escape_js_selector(selector);
    let js = format!(
        r#"(function() {{
            var selectors = "{escaped}".split(",").map(function(s) {{ return s.trim(); }});
            var el = null;
            for (var i = 0; i < selectors.length; i++) {{
                el = document.querySelector(selectors[i]);
                if (el) break;
            }}
            if (!el) return null;
            var r = el.getBoundingClientRect();
            return JSON.stringify({{x: r.x + r.width / 2, y: r.y + r.height / 2}});
        }})()"#
    );

    let result = page.evaluate(js).await.map_err(|e| ScraperError::Browser {
        reason: format!("Failed to locate '{selector}': {e}"),
    })?;

    let coords_str = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .ok_or_else(|| ScraperError::Scraping {
            reason: format!("Element not found for selector: {selector}"),
        })?;

    let coords: serde_json::Value =
        serde_json::from_str(&coords_str).map_err(|e| ScraperError::Browser {
            reason: format!("Failed to parse element coordinates: {e}"),
        })?;

    Ok((
        coords["x"].as_f64().unwrap_or(0.0),
        coords["y"].as_f64().unwrap_or(0.0),
    ))
}

/// Check if a visible element matching the selector exists in the DOM
pub async fn element_exists(page: &chromiumoxide::Page, selector: &str) -> bool {
    let escaped = escape_js_selector(selector);
    let js = format!(
        r#"(function() {{
            var selectors = "{escaped}".split(",").map(function(s) {{ return s.trim(); }});
            for (var i = 0; i < selectors.length; i++) {{
                var el = document.querySelector(selectors[i]);
                if (el) {{
                    var r = el.getBoundingClientRect();
                    if (r.width > 0 && r.height > 0) return "found";
                }}
            }}
            return "not_found";
        }})()"#
    );
    page.evaluate(js)
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(|s| s == "found")))
        .unwrap_or(false)
}

/// Click an element matching the given CSS selector.
///
/// Supports comma-separated fallback selectors and a `text:` prefix for
/// matching by button text content (e.g., `text:Sign In With Google`).
pub async fn click_element(page: &chromiumoxide::Page, selector: &str) -> ScraperResult<()> {
    let escaped_selector = escape_js_selector(selector);
    let js = format!(
        r#"(function() {{
            var parts = "{escaped_selector}".split(",").map(function(s) {{ return s.trim(); }});
            for (var i = 0; i < parts.length; i++) {{
                var sel = parts[i];
                if (sel.indexOf("text:") === 0) {{
                    var text = sel.substring(5);
                    var buttons = document.querySelectorAll("button, a, [role=button]");
                    for (var j = 0; j < buttons.length; j++) {{
                        if (buttons[j].textContent.trim().indexOf(text) !== -1) {{
                            buttons[j].click();
                            return "clicked";
                        }}
                    }}
                }} else {{
                    var el = document.querySelector(sel);
                    if (el) {{ el.click(); return "clicked"; }}
                }}
            }}
            return "not_found";
        }})()"#
    );

    let result = page.evaluate(js).await.map_err(|e| ScraperError::Browser {
        reason: format!("Failed to click '{selector}': {e}"),
    })?;

    let status = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    if status == "not_found" {
        return Err(ScraperError::Scraping {
            reason: format!("Button not found for selector: {selector}"),
        });
    }

    debug!(selector, "Element clicked");
    Ok(())
}

/// Auto-dismiss cookie consent dialogs (`Cookiebot`, `CookieFirst`, generic accept buttons)
pub async fn dismiss_cookie_dialog(page: &chromiumoxide::Page) {
    let dismiss_js = r#"
        (function() {
            // Cookiebot
            var btn = document.querySelector('#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll')
                || document.querySelector('[data-cookiefirst-action="accept"]')
                || document.querySelector('button[id*="accept"], button[class*="accept"]');
            if (btn) { btn.click(); return 'dismissed'; }
            // Text-based fallback: find any button with Accept All / Tout accepter
            var allButtons = document.querySelectorAll('button, a, [role=button]');
            for (var i = 0; i < allButtons.length; i++) {
                var text = allButtons[i].textContent.trim();
                if (text === 'Accept All' || text === 'Tout accepter' || text === 'Accept all'
                    || text === 'Accepter tout' || text === 'Accept All Cookies') {
                    allButtons[i].click();
                    return 'dismissed_text';
                }
            }
            // Iframe fallback
            var iframes = document.querySelectorAll('iframe');
            for (var j = 0; j < iframes.length; j++) {
                try {
                    var doc = iframes[j].contentDocument;
                    if (doc) {
                        var b = doc.querySelector('#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll')
                            || doc.querySelector('button[id*="accept"]');
                        if (b) { b.click(); return 'dismissed_iframe'; }
                    }
                } catch(e) {}
            }
            return 'not_found';
        })()
    "#;
    if let Ok(result) = page.evaluate(dismiss_js).await {
        let val = result
            .value()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        tracing::info!(result = %val, "Cookie dialog auto-dismiss attempt");
    }
}

/// Read visible text from an element, returning `None` if element is missing or hidden
pub async fn read_visible_text(page: &chromiumoxide::Page, selector: &str) -> Option<String> {
    let escaped = escape_js_selector(selector);
    let js = format!(
        r#"(function() {{
            var selectors = "{escaped}".split(",").map(function(s) {{ return s.trim(); }});
            for (var i = 0; i < selectors.length; i++) {{
                var el = document.querySelector(selectors[i]);
                if (el && el.offsetParent !== null) {{
                    var text = el.textContent.trim();
                    if (text) return text;
                }}
            }}
            return "";
        }})()"#
    );
    let result = page.evaluate(js).await.ok()?;
    let text = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

// ABOUTME: Canary test for the JS/CDP-level signals Google Sign-In's loader checks
// ABOUTME: Catches future chromiumoxide bumps (or stealth changes) that re-introduce them
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Automation-detection regression guard.
//!
//! Drives sciotte's real `launch_browser` + `apply_minimal_stealth` against
//! a local HTML fixture (`tests/fixtures/canary/index.html`) that
//! reproduces the exact signals Google's `accounts.google.com/v3/signin/identifier`
//! loader checks before deciding the browser is "secure". The canary
//! page records each signal's status into `window.__signals`; this test
//! reads the map back via `page.evaluate` and asserts that the
//! production-critical ones return clean.
//!
//! What the test covers (each signal has a citation in the fixture):
//! - `cdp_runtime_enable` — Castle.io's Error.stack getter trick that
//!   fires whenever CDP's `Runtime.enable` is active.
//! - `webdriver_set` — `navigator.webdriver === true`. Should be hidden
//!   by `.hide()` (which emits `--disable-blink-features=AutomationControlled`)
//!   and/or our stealth payload.
//! - `plugins_empty` — empty `PluginArray` (headless tell).
//! - `languages_empty` — empty `navigator.languages` array (headless tell).
//! - `webgl_swiftshader` — software-rendered `WebGL` (collected for observability,
//!   not asserted: the `WebGL` spoof was removed to fix Cloudflare 427s, so a
//!   GPU-less host legitimately reports `SwiftShader` — see the body for details).
//! - `notif_mismatch` — Permissions API `notifications` returns 'granted'
//!   while `Notification.permission` returns 'denied'.
//! - `ua_headless_substring` — `HeadlessChrome` in the User-Agent string.
//!
//! When this test fails, the eprintln! before the assertion dumps the
//! full signal map so the operator can see exactly which probe leaked.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dravr_sciotte::browser_utils::{apply_minimal_stealth, launch_browser};
use dravr_sciotte::config::ScraperConfig;
use tokio::fs;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::sleep;

/// Spawn a minimal HTTP/1.1 server backed by `tests/fixtures/`.
async fn start_fixture_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let dir = fixtures_dir.clone();
            tokio::spawn(handle_request(stream, dir));
        }
    });

    (addr, handle)
}

async fn handle_request(stream: TcpStream, fixtures_dir: PathBuf) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 4096];
    let (mut reader, mut writer) = stream.into_split();

    let n = reader.read(&mut buf).await.unwrap_or(0);
    if n == 0 {
        return;
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let file_path = path
        .split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/');
    let full_path = fixtures_dir.join(file_path);

    let (status, content_type, body) = if full_path.exists() && full_path.is_file() {
        let body = fs::read(&full_path).await.unwrap_or_default();
        let ct = if Path::new(file_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html"))
        {
            "text/html"
        } else {
            "application/octet-stream"
        };
        ("200 OK", ct, body)
    } else {
        ("404 Not Found", "text/plain", b"Not Found".to_vec())
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.write_all(&body).await;
}

fn test_config() -> ScraperConfig {
    ScraperConfig {
        page_load_wait_secs: 1,
        form_interaction_delay_ms: 100,
        email_step_timeout_secs: 10,
        password_step_timeout_secs: 10,
        login_timeout_secs: 30,
        login_poll_interval_ms: 200,
        phone_tap_timeout_secs: 5,
        credential_login_headless: true,
        ..ScraperConfig::default()
    }
}

/// Drive sciotte's real launch path against the canary fixture and
/// dump `window.__signals` after the page settles.
#[tokio::test]
async fn no_automation_signals_leak() {
    let (addr, _server) = start_fixture_server().await;
    let canary_url = format!("http://{addr}/canary/index.html");

    let config = test_config();
    let browser = launch_browser(&config, true, None)
        .await
        .expect("launch headless browser");

    let page = browser.new_page("about:blank").await.expect("new_page");
    apply_minimal_stealth(&page).await.expect("apply stealth");
    page.goto(canary_url).await.expect("navigate to canary");

    // Wait for window.__canary_ready (the fixture sets this 200ms after
    // its synchronous probes finish, giving microtask-scheduled signals
    // — Promise.then for CDP serialization, .then for Permissions.query —
    // time to land).
    let mut ready = false;
    for _ in 0..30 {
        let outcome = page
            .evaluate("window.__canary_ready === true")
            .await
            .ok()
            .and_then(|r| r.value().and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        if outcome {
            ready = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "canary fixture didn't become ready within 3s");

    let signals_json = page
        .evaluate("JSON.stringify(window.__signals || {})")
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(String::from)))
        .unwrap_or_default();
    let signals: serde_json::Value =
        serde_json::from_str(&signals_json).unwrap_or(serde_json::Value::Null);

    eprintln!("\n=== window.__signals ===\n{signals:#}\n");

    // Drop the page before any assertion so a panic releases its Chrome
    // ref before the browser closes (Browser.close in scraper.rs handles
    // the production path).
    drop(page);

    // The signals we MUST keep clean. Each carries a citation — see the
    // fixture for what each probes.
    assert_signal_clean(
        &signals,
        "cdp_runtime_enable",
        "Castle.io: Google's loader fires the Error.stack getter trick \
         when CDP Runtime.enable is active.",
    );
    assert_signal_clean(
        &signals,
        "webdriver_set",
        "Stealth payload + --disable-blink-features=AutomationControlled \
         must hide navigator.webdriver=true.",
    );
    assert_signal_clean(
        &signals,
        "plugins_empty",
        "Stealth payload must spoof navigator.plugins to a non-empty array.",
    );
    assert_signal_clean(
        &signals,
        "languages_empty",
        "Stealth payload must populate navigator.languages.",
    );
    // `chrome_runtime_missing` and `webgl_swiftshader` are intentionally not
    // asserted. Commit 0db1717 removed the `window.chrome.runtime` stub (its
    // `Object.defineProperty` left a detectable `.toString()` trace) together with
    // the WebGL renderer spoof shipped alongside it — the pair tripped Cloudflare
    // 427s in production. The stealth payload now leans on Chrome launch flags
    // rather than JS-level overrides, so without the spoof a GPU-less host (the CI
    // runner and the Cloud Run deploy alike) reports a SwiftShader WebGL renderer;
    // re-introducing either assertion without re-introducing the 427 regression is
    // not possible with the current strategy. Both signals remain in the collected
    // map above for observability.
    assert_signal_clean(
        &signals,
        "notif_mismatch",
        "Permissions.query('notifications') must agree with Notification.permission \
         (DataDome 2025 — headless reports granted-while-denied).",
    );
    assert_signal_clean(
        &signals,
        "ua_headless_substring",
        "User-Agent must not contain 'HeadlessChrome' substring. \
         If this fails, check that `--user-agent=` is being passed via the \
         tuple form `.arg((\"user-agent\", ua))` and not the old shell-string \
         form `.arg(\"--user-agent=...\")` — chromiumoxide 0.9+ silently mangles \
         the latter into a four-dash flag Chrome ignores.",
    );
}

/// Fail with a readable message that includes the citation when a
/// canary signal is set.
fn assert_signal_clean(signals: &serde_json::Value, key: &str, citation: &str) {
    let triggered = signals
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    assert!(
        !triggered,
        "automation signal '{key}' triggered → CDP-driven Chrome detectable. \
         Why this matters: {citation}\nFull signal map: {signals:#}"
    );
}

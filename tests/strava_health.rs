// ABOUTME: Exploration test for Strava wellness pages (Fitness & Freshness, Training Log)
// ABOUTME: Probes what health/wellness data Strava exposes for extraction
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::time::Duration;

use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use dravr_sciotte::auth::{load_session, save_session};
use dravr_sciotte::browser_utils::{inject_cookies, launch_browser};
use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, ChromeScraper};
use tokio::fs;
use tokio::time::sleep;

const STRAVA_LOGIN_URL: &str = "https://www.strava.com/login";

async fn take_screenshot(page: &chromiumoxide::Page, label: &str) {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .build();
    match page.screenshot(params).await {
        Ok(data) => {
            let path = format!("/tmp/sciotte-strava-{label}.png");
            if fs::write(&path, &data).await.is_ok() {
                eprintln!("  Screenshot: {path} ({} bytes)", data.len());
            }
        }
        Err(e) => eprintln!("  Screenshot failed: {e}"),
    }
}

async fn open_page(
    browser: &Browser,
    session: &AuthSession,
    url: &str,
    wait_secs: u64,
) -> chromiumoxide::Page {
    let page = browser
        .new_page(STRAVA_LOGIN_URL)
        .await
        .expect("open browser tab");
    sleep(Duration::from_millis(500)).await;
    inject_cookies(&page, session)
        .await
        .expect("inject cookies");
    page.goto(url).await.expect("navigate to target URL");
    sleep(Duration::from_secs(wait_secs)).await;
    page
}

async fn eval_js(page: &chromiumoxide::Page, js: &str) -> String {
    match page.evaluate(js).await {
        Ok(r) => match r.value().cloned().unwrap_or(serde_json::Value::Null) {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        },
        Err(e) => format!("error: {e}"),
    }
}

async fn get_session(scraper: &ChromeScraper) -> AuthSession {
    if let Ok(Some(session)) = load_session().await {
        if scraper.is_authenticated(&session).await {
            eprintln!("Reusing saved session ({})", session.session_id);
            return session;
        }
    }

    if let (Ok(email), Ok(password)) = (env::var("STRAVA_EMAIL"), env::var("STRAVA_PASSWORD")) {
        eprintln!("Authenticating via credentials...");
        match scraper.credential_login(&email, &password, "google").await {
            Ok(LoginResult::Success(session)) => {
                save_session(&session).await.ok();
                return session;
            }
            Ok(other) => eprintln!("Credential login: {other:?}, falling back to browser login"),
            Err(e) => eprintln!("Credential login failed: {e}, falling back to browser login"),
        }
    }

    eprintln!("Opening visible browser for manual Strava login...");
    let session = scraper.browser_login().await.expect("browser login");
    save_session(&session).await.ok();
    session
}

/// Probe a page: navigate, screenshot, dump title/URL/body length
async fn probe_page(browser: &Browser, session: &AuthSession, name: &str, url: &str) {
    let page = open_page(browser, session, url, 5).await;
    let landed = eval_js(&page, "window.location.href").await;
    let title = eval_js(&page, "document.title").await;
    let body_len = eval_js(&page, "document.body.textContent.length.toString()").await;
    eprintln!("\n  [{name}]");
    eprintln!("  Requested: {url}");
    eprintln!("  Landed:    {landed}");
    eprintln!("  Title:     {title}");
    eprintln!("  Body len:  {body_len}");
    take_screenshot(&page, name).await;
}

/// Deep DOM probe for a page — dump elements with classes, text
async fn dom_probe(page: &chromiumoxide::Page) -> String {
    eval_js(
        page,
        r#"(function() {
        var result = {};
        // Find elements with data or metric-like content
        var els = document.querySelectorAll('[class*="fitness"], [class*="freshness"], [class*="fatigue"], [class*="form"], [class*="training"], [class*="effort"], [class*="heart"], [class*="stat"], [class*="score"], [class*="chart"], [data-testid]');
        result.health_elements = [];
        els.forEach(function(el) {
            var text = el.textContent.trim().substring(0, 150);
            if (text.length > 3) {
                result.health_elements.push({
                    tag: el.tagName,
                    class: (el.className || '').toString().substring(0, 120),
                    testid: el.getAttribute('data-testid') || '',
                    text: text
                });
            }
        });

        // Find numeric values that could be metrics
        var spans = document.querySelectorAll('span, div, h1, h2, h3, h4, p');
        result.metrics = [];
        var seen = {};
        spans.forEach(function(el) {
            var t = el.textContent.trim();
            if (t.length < 40 && /\d+/.test(t) && !seen[t]) {
                var cls = (el.className || '').toString();
                if (cls.length > 0) {
                    seen[t] = true;
                    result.metrics.push({
                        tag: el.tagName,
                        class: cls.substring(0, 80),
                        text: t
                    });
                }
            }
        });

        // Network requests
        result.api_urls = performance.getEntriesByType('resource')
            .filter(function(e) { return e.name.includes('/api/') || e.name.includes('fitness') || e.name.includes('athlete'); })
            .map(function(e) { return e.name; });

        return JSON.stringify(result, null, 2);
    })()"#,
    )
    .await
}

#[tokio::test]
async fn explore_strava_wellness_pages() {
    let has_creds = env::var("STRAVA_EMAIL").is_ok() && env::var("STRAVA_PASSWORD").is_ok();
    let has_session = matches!(load_session().await, Ok(Some(_)));

    if !has_creds && !has_session {
        eprintln!("Set STRAVA_EMAIL + STRAVA_PASSWORD or save a session first. Skipping.");
        return;
    }

    let provider = ProviderConfig::strava_default();
    let config = ScraperConfig::default();
    let scraper = ChromeScraper::new(config.clone(), provider);
    let session = get_session(&scraper).await;
    eprintln!("Session: {} cookies\n", session.cookies.len());

    let browser = launch_browser(&config, true)
        .await
        .expect("launch headless browser");

    // Validate session
    eprintln!("== SESSION VALIDATION ==");
    probe_page(
        &browser,
        &session,
        "dashboard",
        "https://www.strava.com/dashboard",
    )
    .await;

    // Probe wellness pages
    eprintln!("\n== STRAVA WELLNESS PAGES ==");

    let pages = [
        ("fitness", "https://www.strava.com/athlete/fitness"),
        (
            "training-log",
            "https://www.strava.com/athletes/32530060/training/log",
        ),
        (
            "training-calendar",
            "https://www.strava.com/athlete/calendar",
        ),
    ];

    for (name, url) in &pages {
        probe_page(&browser, &session, name, url).await;
    }

    // Deep probe on fitness page
    eprintln!("\n== FITNESS PAGE DOM PROBE ==");
    let fitness_page = open_page(
        &browser,
        &session,
        "https://www.strava.com/athlete/fitness",
        8,
    )
    .await;
    let dom = dom_probe(&fitness_page).await;
    eprintln!("{dom}");
    take_screenshot(&fitness_page, "fitness-dom").await;

    // Probe training log URL variants
    eprintln!("\n== TRAINING LOG URL VARIANTS ==");
    let log_urls = [
        ("log-no-id", "https://www.strava.com/athlete/training/log"),
        (
            "log-with-id",
            "https://www.strava.com/athletes/32530060/training/log",
        ),
        (
            "training-overview",
            "https://www.strava.com/athlete/training",
        ),
    ];
    for (name, url) in &log_urls {
        probe_page(&browser, &session, name, url).await;
    }

    eprintln!("\nDone. Screenshots at /tmp/sciotte-strava-*.png");
}

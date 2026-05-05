// ABOUTME: Probe test that dumps where Strava embeds activity GPS coordinates
// ABOUTME: on the training-list and feed pages so we can scrape them without N+1
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Manual probe — gated on `STRAVA_EMAIL`/`STRAVA_PASSWORD` or a saved session.
//!
//! Run with:
//! ```sh
//! STRAVA_EMAIL=... STRAVA_PASSWORD=... \
//!   cargo test --test strava_list_gps_probe -- --nocapture
//! ```
//!
//! Prints, for the training-list page and the dashboard feed page:
//! - Up to 5 row IDs with their `outerHTML` (truncated) and `data-*` attrs
//! - Up to 50 hits of any inline `<script>` body matching `start_latlng`,
//!   `polyline`, or activity-card JSON payloads
//! - The shape of `window.pageView` / `window.__INITIAL_STATE` /
//!   `window.__NEXT_DATA__` if any of those are populated
//! - Mapbox static-image URLs whose path encodes `<lng>,<lat>,<zoom>`
//!
//! Goal: identify the cheapest list-page extraction path so we can drop
//! the per-activity detail fetch that the platform's weather backfill
//! currently relies on.

use std::env;
use std::time::Duration;

use chromiumoxide::browser::Browser;
use dravr_sciotte::auth::{load_session, save_session};
use dravr_sciotte::browser_utils::{inject_cookies, launch_browser};
use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, ChromeScraper};
use tokio::time::sleep;

const STRAVA_LOGIN_URL: &str = "https://www.strava.com/login";

async fn eval_js(page: &chromiumoxide::Page, js: &str) -> String {
    match page.evaluate(js).await {
        Ok(r) => match r.value().cloned().unwrap_or(serde_json::Value::Null) {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        },
        Err(e) => format!("error: {e}"),
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

const PROBE_JS: &str = r#"(function() {
    const out = {
        url: window.location.href,
        rows: [],
        scripts: [],
        globals: {},
        mapbox_imgs: [],
    };

    // 1. Training-list rows: outerHTML + data-* attrs (first 5)
    const rows = document.querySelectorAll('tr.training-activity-row, tr[data-activity-id], a[data-cy="activity-card"], a[href*="/activities/"]');
    for (let i = 0; i < rows.length && out.rows.length < 5; i++) {
        const r = rows[i];
        const dataAttrs = {};
        for (let j = 0; j < r.attributes.length; j++) {
            const a = r.attributes[j];
            if (a.name.startsWith('data-')) dataAttrs[a.name] = a.value;
        }
        out.rows.push({
            tag: r.tagName,
            class: (r.className || '').toString().substring(0, 200),
            data: dataAttrs,
            href: r.getAttribute('href') || '',
            outerHTML: r.outerHTML.substring(0, 600),
        });
    }

    // 2. Inline <script> bodies: hits on start_latlng / polyline / activity payloads
    const scripts = document.querySelectorAll('script');
    for (let s = 0; s < scripts.length; s++) {
        const body = scripts[s].textContent || '';
        const latlngMatches = body.match(/"start_latlng"\s*:\s*\[[^\]]*\]/g) || [];
        const polyMatches = body.match(/"summary_polyline"\s*:\s*"[^"]{1,40}"/g) || [];
        if (latlngMatches.length > 0 || polyMatches.length > 0) {
            out.scripts.push({
                idx: s,
                size: body.length,
                start_latlng_hits: latlngMatches.slice(0, 5),
                summary_polyline_hits: polyMatches.slice(0, 5),
            });
            if (out.scripts.length >= 5) break;
        }
    }

    // 3. Globals: window.pageView, __INITIAL_STATE, __NEXT_DATA__, _currentAthlete
    try {
        if (typeof pageView !== 'undefined' && pageView) {
            out.globals.pageView = {
                has_activity_fn: typeof pageView.activity === 'function',
                keys: Object.keys(pageView).slice(0, 30),
            };
        }
    } catch(e) {}
    try {
        if (window.__INITIAL_STATE__) {
            out.globals.__INITIAL_STATE__ = {
                top_keys: Object.keys(window.__INITIAL_STATE__).slice(0, 30),
            };
        }
    } catch(e) {}
    try {
        if (window.__NEXT_DATA__) {
            out.globals.__NEXT_DATA__ = {
                top_keys: Object.keys(window.__NEXT_DATA__).slice(0, 30),
            };
        }
    } catch(e) {}

    // 4. Mapbox static map images: <lng>,<lat>,<zoom> in src — first 5
    const mapImgs = document.querySelectorAll('img[src*="api.mapbox.com/styles"]');
    for (let i = 0; i < mapImgs.length && out.mapbox_imgs.length < 5; i++) {
        const src = mapImgs[i].getAttribute('src') || '';
        const m = src.match(/\/static\/[^\/]+\/(-?\d+\.\d+),(-?\d+\.\d+),/);
        out.mapbox_imgs.push({
            src_truncated: src.substring(0, 200),
            parsed_lng: m ? m[1] : null,
            parsed_lat: m ? m[2] : null,
        });
    }

    return JSON.stringify(out, null, 2);
})()"#;

#[tokio::test]
async fn probe_strava_list_page_for_gps() {
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

    let browser = launch_browser(&config, true, None)
        .await
        .expect("launch headless browser");

    let pages = [
        ("training-table", "https://www.strava.com/athlete/training"),
        ("dashboard-feed", "https://www.strava.com/dashboard"),
    ];

    for (name, url) in &pages {
        eprintln!("\n========================================");
        eprintln!("== {name}: {url}");
        eprintln!("========================================");
        let page = open_page(&browser, &session, url, 8).await;
        let landed = eval_js(&page, "window.location.href").await;
        eprintln!("Landed: {landed}");

        let probe = eval_js(&page, PROBE_JS).await;
        eprintln!("{probe}");
    }

    eprintln!("\nDone.");
}

// ABOUTME: Regression test — select_two_factor resolves the echoed option id against the
// ABOUTME: STORED chooser analysis (LLM ids are per-analysis), driven by a scripted VisionModel
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Vision 2FA continuation flow tests (`--features vision`).
//!
//! The vision LLM assigns non-deterministic option ids on every page analysis,
//! so `select_two_factor` must resolve the caller's echoed id against the same
//! analysis that presented the chooser — the one parked with the flow. The
//! scripted model below serves exactly two analyses: the chooser (consumed by
//! `credential_login`) and a success page (consumed by the post-click outcome
//! poll). The pre-fix implementation re-analyzed the page to resolve the id,
//! which here consumes the success analysis, finds no options, and fails —
//! making this a content-asserting regression net for the stored-id path.

#![cfg(feature = "vision")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, VisionModel, VisionModelError, VisionScraper};
use tokio::fs;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Serve the test fixtures directory via a minimal HTTP server
async fn start_fixture_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");

        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let dir = fixtures_dir.clone();
            tokio::spawn(async move {
                handle_http(stream, dir).await;
            });
        }
    });

    (addr, handle)
}

/// Minimal HTTP/1.1 handler that serves static files from the fixtures directory
async fn handle_http(stream: TcpStream, fixtures_dir: PathBuf) {
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

    let file_path = path.split('?').next().unwrap_or(path);
    let file_path = file_path.trim_start_matches('/');

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

/// Fake provider config pointing the login page at the fixture server.
fn fake_provider(base_url: &str) -> ProviderConfig {
    let toml = format!(
        r#"
[provider]
name = "fake-strava"
login_url = "{base_url}/strava/login.html"
login_success_patterns = ["/dashboard"]
login_failure_patterns = ["/login"]
login_email_selector = '#email, input[name="email"]'
login_password_selector = '#password, input[name="password"]'
login_button_selector = 'button[type="submit"], #login-button'
login_error_selector = '.alert-error'

[list_page]
url = "{base_url}/strava/dashboard.html"
row_selector = "tr"
link_selector = "a"
id_regex = '/\/activities\/(\d+)/'

[list_page.fields]
name = "td"
sport_type = "td"
date = "td"
time = "td"
distance = "td"
elevation = "td"

[detail_page]
url_template = "{base_url}/strava/activity/{{id}}"
js_extract = '(function() {{ return "{{}}"; }})()'
"#
    );
    ProviderConfig::from_toml(&toml).unwrap()
}

/// A vision model serving a fixed script of page analyses. Every
/// `analyze_screenshot` pops the next response; running past the script is an
/// error, so the assertion on remaining responses pins the exact number of
/// LLM round-trips the flow performed.
struct ScriptedVisionModel {
    responses: Mutex<VecDeque<String>>,
}

impl ScriptedVisionModel {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait]
impl VisionModel for ScriptedVisionModel {
    async fn analyze_screenshot(
        &self,
        _prompt: &str,
        _screenshot_png_b64: &str,
    ) -> Result<String, VisionModelError> {
        self.responses
            .lock()
            .await
            .pop_front()
            .ok_or_else(|| -> VisionModelError { "scripted vision model exhausted".into() })
    }
}

/// A fast-failing scraper config so the test never waits real-world timeouts.
fn test_config() -> ScraperConfig {
    ScraperConfig {
        login_timeout_secs: 30,
        password_step_timeout_secs: 10,
        page_load_wait_secs: 1,
        login_poll_interval_ms: 100,
        form_interaction_delay_ms: 50,
        pending_login_ttl_secs: 60,
        ..ScraperConfig::default()
    }
}

/// The echoed 2FA option id must resolve against the chooser analysis that was
/// parked with the flow — NOT a fresh re-analysis (whose ids the LLM renames).
/// Script: analysis #1 presents the chooser (id `phone_tap`), analysis #2 is
/// the post-click success page. The stored-id path consumes exactly these two;
/// the pre-fix re-analysis path would burn #2 on id resolution, find no
/// options in it, and fail with "2FA option not found".
#[tokio::test]
async fn select_two_factor_resolves_stored_option_ids() {
    let (addr, server) = start_fixture_server().await;
    let base_url = format!("http://{addr}");

    let chooser_analysis = serde_json::json!({
        "page_type": "two_factor_selection",
        "two_factor_options": [
            {"id": "phone_tap", "label": "Tap Yes on your phone", "x": 100.0, "y": 100.0},
            {"id": "otc", "label": "Get a one-time security code", "x": 100.0, "y": 200.0}
        ]
    })
    .to_string();
    let success_analysis = serde_json::json!({ "page_type": "success" }).to_string();

    let model = Arc::new(ScriptedVisionModel::new(vec![
        chooser_analysis,
        success_analysis,
    ]));
    let model_dyn: Arc<dyn VisionModel> = Arc::clone(&model) as Arc<dyn VisionModel>;
    let scraper = VisionScraper::new(test_config(), fake_provider(&base_url), model_dyn);

    // Step 1: login lands on the scripted chooser and parks the flow with its options.
    let outcome = scraper
        .credential_login("user@example.com", "hunter2", "email")
        .await
        .expect("credential_login should reach the scripted chooser");
    let LoginResult::TwoFactorChoice(options) = outcome else {
        panic!("expected TwoFactorChoice, got {outcome:?}");
    };
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].id, "phone_tap");

    // Step 2: the echoed id resolves against the STORED options (no extra
    // LLM analysis) and the outcome poll consumes the success analysis.
    let result = scraper
        .select_two_factor("phone_tap")
        .await
        .expect("select_two_factor should resolve the stored option id");
    let LoginResult::Success(session) = result else {
        panic!("expected Success after stored-id click, got {result:?}");
    };
    assert!(!session.session_id.is_empty());

    // The script is fully consumed: exactly 2 analyses — one for the chooser,
    // one for the outcome. A resolution re-analysis would have exhausted the
    // script early and failed above.
    assert_eq!(model.responses.lock().await.len(), 0);

    server.abort();
}

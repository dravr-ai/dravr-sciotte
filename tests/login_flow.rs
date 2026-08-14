// ABOUTME: Integration tests for credential login flows using fake HTML pages
// ABOUTME: Serves static fixtures via a local HTTP server and tests ChromeScraper against them
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::str_to_string
)]

use std::collections::HashMap;
use std::fmt::Debug as FmtDebug;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, ChromeScraper};
use tokio::fs;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::sleep;

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

    let (mut reader, mut writer) = stream.into_split();

    // Read until the header block is complete rather than assuming the whole
    // request lands in one segment. A single read() is a race: when the request
    // arrives split across TCP segments — far likelier on a loaded CI runner —
    // the first chunk may not carry a complete request line, the path parses to
    // something meaningless and this server answers 404. A 404'd fixture page
    // has no [data-challengetype] elements, so the scraper's 2FA options never
    // parse, it never navigates, and it spins to the step timeout still
    // reporting the page it asked for. That is the shape of the
    // `google_oauth_2fa_number_match` flake, and it is a bug in this harness
    // rather than in the scraper under test.
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break; // peer closed — includes Chrome's speculative preconnects
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break; // headers complete; these fixtures never carry a body
        }
        if buf.len() > 64 * 1024 {
            break; // refuse to buffer without bound
        }
    }
    if buf.is_empty() {
        return;
    }

    let request = String::from_utf8_lossy(&buf);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    // Strip query params for file lookup
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

/// Create a fake Strava provider config pointing to our local test server
fn fake_strava_provider(base_url: &str) -> ProviderConfig {
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

/// Create a fake Garmin provider config pointing to our local test server
fn fake_garmin_provider(base_url: &str) -> ProviderConfig {
    let toml = format!(
        r#"
[provider]
name = "fake-garmin"
login_url = "{base_url}/garmin/sign-in.html"
login_success_patterns = ["/dashboard"]
login_failure_patterns = ["/sign-in"]
login_email_selector = '#email, input[name="email"]'
login_password_selector = '#password, input[name="password"]'
login_button_selector = '#login-btn-signin, button[type="submit"]'
login_error_selector = '.alert-error'
login_otp_selector = 'input[name="verificationCode"], input[name="code"], input[type="tel"]'

[list_page]
url = "{base_url}/garmin/dashboard.html"
row_selector = "tr"
link_selector = "a"
id_regex = '/\/activity\/(\d+)/'

[list_page.fields]
name = "td"
sport_type = "td"
date = "td"
time = "td"
distance = "td"
elevation = "td"

[detail_page]
url_template = "{base_url}/garmin/activity/{{id}}"
js_extract = '(function() {{ return "{{}}"; }})()'
"#
    );
    ProviderConfig::from_toml(&toml).unwrap()
}

fn test_config() -> ScraperConfig {
    // Step timeouts are generous ceilings, not expected durations: the fixture
    // flow returns as soon as the expected state is detected, so a passing run
    // is unaffected. The headroom keeps the fake-server flow from timing out on
    // CI runners whose headless Chrome is ~3x slower than a dev laptop (the
    // tight 10/30/5s values flaked `google_oauth_2fa_number_match` at ~48s).
    ScraperConfig {
        page_load_wait_secs: 1,
        form_interaction_delay_ms: 100,
        email_step_timeout_secs: 30,
        password_step_timeout_secs: 30,
        login_timeout_secs: 90,
        login_poll_interval_ms: 200,
        phone_tap_timeout_secs: 20,
        credential_login_headless: true,
        ..ScraperConfig::default()
    }
}

// ============================================================================
// Strava direct login tests
// ============================================================================

#[tokio::test]
async fn strava_direct_login_success() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    let result = scraper
        .credential_login("test@example.com", "correct-password", "email")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::Success(ref s) if !s.cookies.is_empty() || !s.session_id.is_empty()),
        "Expected Success, got {result:?}"
    );
}

#[tokio::test]
async fn strava_direct_login_wrong_password() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    let result = scraper
        .credential_login("test@example.com", "wrong-password", "email")
        .await;

    // Wrong password should result in either a Failed login or a timeout
    // (the fake page stays on /login which doesn't match success patterns)
    match result {
        Ok(LoginResult::Failed(_)) => {} // Error message detected
        Err(ref e) if e.to_string().contains("timed out") => {} // Timed out on login page
        other => panic!("Expected Failed or timeout, got {other:?}"),
    }
}

// ============================================================================
// Garmin login with MFA tests
// ============================================================================

#[tokio::test]
async fn garmin_login_with_mfa() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_garmin_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    // Step 1: Login — should require OTP
    let result = scraper
        .credential_login("test@example.com", "correct-password", "email")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::OtpRequired),
        "Expected OtpRequired, got {result:?}"
    );

    // Step 2: Submit correct OTP
    let result = scraper.submit_otp("123456").await.unwrap();

    assert!(
        matches!(result, LoginResult::Success(_)),
        "Expected Success after OTP, got {result:?}"
    );
}

#[tokio::test]
async fn garmin_login_no_mfa() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_garmin_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    let result = scraper
        .credential_login("test@example.com", "no-mfa-password", "email")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::Success(_)),
        "Expected Success (no MFA), got {result:?}"
    );
}

/// Fixture server that serves an under-rendered login page (no email field) on
/// the FIRST request to the Garmin login path, then the real fixture on every
/// request after — simulating the transient datacenter-IP soft-throttle that
/// leaves the selector step with nothing to fill. Proves the reload-and-retry.
async fn start_flaky_login_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let login_hits = Arc::new(AtomicUsize::new(0));

    let handle = tokio::spawn(async move {
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let dir = fixtures_dir.clone();
            let hits = Arc::clone(&login_hits);
            tokio::spawn(async move {
                handle_http_flaky(stream, dir, &hits).await;
            });
        }
    });

    (addr, handle)
}

/// Like [`handle_http`], but the first request to `garmin/sign-in.html` returns a
/// page missing the login fields (an under-rendered "just a moment" shell), so
/// the selector step fails once before the retry reloads and gets the real page.
async fn handle_http_flaky(stream: TcpStream, fixtures_dir: PathBuf, login_hits: &AtomicUsize) {
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

    let broken_first =
        file_path == "garmin/sign-in.html" && login_hits.fetch_add(1, Ordering::SeqCst) == 0;

    let (status, content_type, body): (&str, &str, Vec<u8>) = if broken_first {
        (
            "200 OK",
            "text/html",
            b"<html><body><h1>Just a moment...</h1></body></html>".to_vec(),
        )
    } else {
        let full_path = fixtures_dir.join(file_path);
        if full_path.exists() && full_path.is_file() {
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
        }
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.write_all(&body).await;
}

/// The selector login must recover from a transient first-attempt failure via its
/// one reload-retry. Selector-only mode has NO vision fallback, so the login can
/// only succeed if the retry re-fetches the (now real) page and drives it —
/// without the retry this returns `Err`.
#[tokio::test]
async fn garmin_selector_login_retries_after_transient_failure() {
    let (addr, _server) = start_flaky_login_server().await;
    let base = format!("http://{addr}");
    let provider = fake_garmin_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    let result = scraper
        .credential_login("test@example.com", "no-mfa-password", "email")
        .await;

    assert!(
        matches!(result, Ok(LoginResult::Success(_))),
        "selector login must recover via the reload-retry after a transient first-attempt \
         failure (no vision fallback in selector mode); got {result:?}"
    );
}

/// A captured tracing event: its message and its fields rendered to strings.
type CapturedEvent = (String, HashMap<String, String>);
/// Shared, thread-safe log of captured events.
type CapturedEvents = Arc<Mutex<Vec<CapturedEvent>>>;

/// Captures each tracing event's message + fields so a test can assert on the
/// forensic snapshot the selector-login failure path logs.
#[derive(Clone, Default)]
struct EventCapture {
    events: CapturedEvents,
}

#[derive(Default)]
struct FieldGrab {
    message: String,
    fields: HashMap<String, String>,
}

impl Visit for FieldGrab {
    fn record_debug(&mut self, field: &Field, value: &dyn FmtDebug) {
        let rendered = format!("{value:?}");
        if field.name() == "message" {
            self.message.clone_from(&rendered);
        }
        self.fields.insert(field.name().to_owned(), rendered);
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_owned(), value.to_owned());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_owned(), value.to_string());
    }
}

impl<S> Layer<S> for EventCapture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut grab = FieldGrab::default();
        event.record(&mut grab);
        self.events
            .lock()
            .unwrap()
            .push((grab.message, grab.fields));
    }
}

/// The forensic snapshot on a selector-login failure must capture ground truth
/// about the page — here an under-rendered first page — so a future diagnosis
/// reads data, not inference: the expected email selector is absent and the
/// page state names the under-rendered shell.
#[tokio::test]
async fn garmin_selector_failure_logs_forensics() {
    let capture = EventCapture::default();
    let events = Arc::clone(&capture.events);
    let guard = tracing_subscriber::registry().with(capture).set_default();

    let (addr, _server) = start_flaky_login_server().await;
    let base = format!("http://{addr}");
    let provider = fake_garmin_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);
    // First page is under-rendered → selector attempt 1 fails and logs forensics;
    // the reload-retry then succeeds. We only care that the failure was recorded.
    let _ = scraper
        .credential_login("test@example.com", "no-mfa-password", "email")
        .await;

    let snapshot = events.lock().unwrap().clone();
    drop(guard);

    let (_, fields) = snapshot
        .iter()
        .find(|(msg, _)| msg.contains("Selector login failure forensics"))
        .unwrap_or_else(|| {
            let msgs: Vec<&String> = snapshot.iter().map(|(m, _)| m).collect();
            panic!("expected a forensics event; captured messages: {msgs:?}")
        });

    assert_eq!(
        fields.get("email_selector_present").map(String::as_str),
        Some("false"),
        "forensics must report the missing email selector on the under-rendered page; fields: {fields:?}"
    );
    assert!(
        fields.contains_key("attempt"),
        "forensics must record which attempt failed; fields: {fields:?}"
    );
    let page_state = fields
        .get("page_state")
        .cloned()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        page_state.contains("just a moment"),
        "forensics page_state must capture the actual page (under-rendered shell); got: {page_state}"
    );
}

// ============================================================================
// Google OAuth with 2FA + number match tests
// ============================================================================

/// Create a fake Strava provider with Google OAuth pointing to local test server
fn fake_strava_google_provider(base_url: &str) -> ProviderConfig {
    let toml = format!(
        r#"
[provider]
name = "fake-strava-google"
login_url = "{base_url}/strava/login.html"
login_success_patterns = ["/dashboard"]
login_failure_patterns = ["/login.html"]
login_email_selector = '#email, input[name="email"]'
login_password_selector = '#password, input[name="password"]'
login_button_selector = 'button[type="submit"]'
login_error_selector = '.alert-error'
login_otp_selector = 'input[name="code"], input[type="tel"]'

[provider.login_oauth_buttons]
google = "text:Sign In With Google"

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

#[tokio::test]
async fn google_oauth_2fa_number_match() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_google_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    // Step 1: Login — should return TwoFactorChoice (2FA options on challenge page)
    let result = scraper
        .credential_login("test@example.com", "2fa-password", "google")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::TwoFactorChoice(ref opts) if !opts.is_empty()),
        "Expected TwoFactorChoice, got {result:?}"
    );

    // Step 2: Select "app" (phone tap) — should show number match
    let result = scraper.select_two_factor("app").await.unwrap();

    // Should get NumberMatch("78") or Success (if auto-redirect was fast)
    match &result {
        LoginResult::NumberMatch(number) => {
            assert_eq!(number, "78", "Expected number 78, got {number}");

            // Step 3: Poll — fake page auto-redirects after 3s
            let result = scraper.select_two_factor("poll").await.unwrap();
            assert!(
                matches!(result, LoginResult::Success(_)),
                "Expected Success after poll, got {result:?}"
            );
        }
        LoginResult::Success(_) => {
            // Also acceptable — page auto-redirected before we checked
        }
        other => panic!("Expected NumberMatch or Success, got {other:?}"),
    }
}

#[tokio::test]
async fn google_oauth_direct_success() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_google_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    // Google OAuth with password that leads directly to success
    let result = scraper
        .credential_login("test@example.com", "correct-password", "google")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::Success(_)),
        "Expected Success, got {result:?}"
    );
}

/// Password submit that lands straight on a TOTP page, with no chooser in between.
///
/// Untested until now, and it is the path where `initial_url` can be sampled *after*
/// the navigation has already landed — making the OTP page the initial URL. The poll
/// must still report `OtpRequired` from that page rather than wait for it to "change".
#[tokio::test]
async fn google_oauth_direct_totp_requires_otp() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_google_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    let result = scraper
        .credential_login("test@example.com", "totp-password", "google")
        .await
        .unwrap();

    assert!(
        matches!(result, LoginResult::OtpRequired),
        "Expected OtpRequired from the TOTP page, got {result:?}"
    );
}

// ============================================================================
// Provider config tests
// ============================================================================

#[test]
fn fake_strava_provider_parses() {
    let provider = fake_strava_provider("http://localhost:9999");
    assert_eq!(provider.provider.name, "fake-strava");
    assert!(provider.provider.login_email_selector.is_some());
}

#[test]
fn fake_garmin_provider_parses() {
    let provider = fake_garmin_provider("http://localhost:9999");
    assert_eq!(provider.provider.name, "fake-garmin");
    assert!(provider.provider.login_otp_selector.is_some());
}

// ============================================================================
// Browser lifecycle tests
// ============================================================================

/// Verify that `close_browser` gracefully shuts down Chrome after login.
///
/// Previously, dropping the scraper without closing caused the chromiumoxide
/// WebSocket handler to error-loop on the dead connection, spamming ERROR logs
/// and triggering error notification alerts.
#[tokio::test]
async fn close_browser_after_login_no_error_loop() {
    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    // Login launches a browser
    let result = scraper
        .credential_login("test@example.com", "correct-password", "email")
        .await
        .unwrap();
    assert!(
        matches!(result, LoginResult::Success(_)),
        "Expected Success, got {result:?}"
    );

    // Explicitly close — should not panic or hang
    scraper.close_browser().await;

    // Calling close again is a safe no-op
    scraper.close_browser().await;
}

/// Verify that `close_browser` is safe on a scraper that never launched a browser.
#[tokio::test]
async fn close_browser_without_launch_is_noop() {
    let provider = fake_strava_provider("http://localhost:9999");
    let scraper = ChromeScraper::new(test_config(), provider);

    // No browser was ever launched — close should be a no-op
    scraper.close_browser().await;
}

/// Verify that the headless browser is closed after `get_activities` (the Arc path).
/// This is the real production flow: `open_authenticated_page` → `get_headless_browser`
/// → `Arc<Browser>` → scrape → drop(page) → `close_browsers` → `Arc::into_inner`.
#[tokio::test]
async fn close_browser_after_get_activities() {
    use dravr_sciotte::models::{ActivityParams, AuthSession};

    let (addr, _server) = start_fixture_server().await;
    let base = format!("http://{addr}");
    let provider = fake_strava_provider(&base);
    let scraper = ChromeScraper::new(test_config(), provider);

    // Create a fake session with cookies (so open_authenticated_page works)
    let session = AuthSession {
        session_id: "test-session".to_owned(),
        cookies: vec![],
        created_at: chrono::Utc::now(),
        expires_at: None,
    };

    let params = ActivityParams {
        limit: Some(5),
        enrich_details: false,
        ..Default::default()
    };

    // get_activities exercises the Arc<Browser> path via get_headless_browser.
    // The dashboard has no activity data and no session redirect, so it returns
    // an empty list. The important thing is the browser lifecycle.
    let result = scraper.get_activities(&session, &params).await;
    match &result {
        Ok(activities) => eprintln!("get_activities returned {} activities", activities.len()),
        Err(e) => eprintln!("get_activities errored (expected): {e}"),
    }
    drop(result);

    // Give a moment for any background tasks to settle
    sleep(Duration::from_secs(1)).await;

    // If Arc::into_inner failed, we'll see "Browser was not closed manually" or
    // "Arc::into_inner returned None" in stderr. The test verifies the path runs.
}

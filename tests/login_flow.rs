// ABOUTME: Integration tests for credential login flows using fake HTML pages
// ABOUTME: Serves static fixtures via a local HTTP server and tests ChromeScraper against them

use std::net::SocketAddr;
use std::path::PathBuf;

use dravr_sciotte::config::ScraperConfig;
use dravr_sciotte::error::LoginResult;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::{ActivityScraper, ChromeScraper};
use tokio::net::TcpListener;

/// Serve the test fixtures directory via a minimal HTTP server
async fn start_fixture_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
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
async fn handle_http(stream: tokio::net::TcpStream, fixtures_dir: PathBuf) {
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

    // Strip query params for file lookup
    let file_path = path.split('?').next().unwrap_or(path);
    let file_path = file_path.trim_start_matches('/');

    let full_path = fixtures_dir.join(file_path);

    let (status, content_type, body) = if full_path.exists() && full_path.is_file() {
        let body = tokio::fs::read(&full_path).await.unwrap_or_default();
        let ct = if std::path::Path::new(file_path)
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
    ScraperConfig {
        page_load_wait_secs: 1,
        form_interaction_delay_ms: 100,
        email_step_timeout_secs: 5,
        password_step_timeout_secs: 5,
        login_timeout_secs: 15,
        login_poll_interval_ms: 200,
        phone_tap_timeout_secs: 5,
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

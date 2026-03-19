// ABOUTME: Browser automation for login — credential-based REST and WebSocket screenshot streaming
// ABOUTME: Handles email/password login, OAuth flows, and streams Chrome frames for interactive login
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::Json;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    InsertTextParams, MouseButton,
};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use dravr_sciotte::models::{AuthSession, CookieData};
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::state::SharedState;

const VIEWPORT_WIDTH: u32 = 1280;
const VIEWPORT_HEIGHT: u32 = 1024;
const SCREENSHOT_QUALITY: i64 = 60;
const SCREENSHOT_POLL_INTERVAL_MS: u64 = 80;
const LOGIN_TIMEOUT_SECS: u64 = 120;
const URL_POLL_INTERVAL_MS: u64 = 500;
const PAGE_LOAD_WAIT_SECS: u64 = 2;

/// Input messages sent from the client over WebSocket
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMessage {
    Click {
        x: f64,
        y: f64,
    },
    Mousedown {
        x: f64,
        y: f64,
    },
    Mouseup {
        x: f64,
        y: f64,
    },
    Mousemove {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        #[serde(rename = "deltaX")]
        delta_x: f64,
        #[serde(rename = "deltaY")]
        delta_y: f64,
    },
    Keydown {
        key: String,
        code: String,
    },
    Keyup {
        key: String,
        code: String,
    },
    Text {
        text: String,
    },
    Resize {
        width: f64,
        height: f64,
    },
}

/// Request body for credential-based login (no streaming)
#[derive(Debug, Deserialize)]
pub struct CredentialLoginRequest {
    /// User's email or username
    pub email: String,
    /// User's password
    pub password: String,
    /// Login method: "email" (default), "google", "apple"
    #[serde(default = "default_login_method")]
    pub method: String,
}

fn default_login_method() -> String {
    "email".to_owned()
}

/// Request body for OTP/2FA code submission
#[derive(Debug, Deserialize)]
pub struct OtpSubmitRequest {
    /// One-time password or 2FA code
    pub code: String,
}

/// Query parameters for the WebSocket login endpoint
#[derive(Debug, Deserialize)]
pub struct BrowserLoginParams {
    /// Optional bearer token for authentication (WebSocket can't send headers from JS)
    token: Option<String>,
    /// Login method: "direct" (default), "google", "apple"
    method: Option<String>,
}

/// WebSocket upgrade handler for browser login streaming.
///
/// Accepts optional `?token=` query param for authentication since
/// browser WebSocket API cannot send custom headers.
pub async fn browser_login_ws(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<BrowserLoginParams>,
) -> impl IntoResponse {
    // Validate bearer token if DRAVR_SCIOTTE_API_KEY is set
    if let Ok(expected) = std::env::var("DRAVR_SCIOTTE_API_KEY") {
        let provided = params.token.as_deref().unwrap_or("");
        let is_valid: bool =
            subtle::ConstantTimeEq::ct_eq(provided.as_bytes(), expected.as_bytes()).into();
        if !is_valid {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Invalid or missing token",
            )
                .into_response();
        }
    }

    let method = params.method.unwrap_or_default();
    ws.on_upgrade(move |socket| handle_browser_login(socket, state, method))
        .into_response()
}

/// Core WebSocket handler that manages the Chrome session
async fn handle_browser_login(socket: WebSocket, state: SharedState, method: String) {
    let provider = {
        let guard = state.read().await;
        guard.scraper().inner().provider().clone()
    };

    info!(
        provider = %provider.provider.name,
        method = %method,
        "Browser streaming session started"
    );

    if let Err(e) = run_streaming_session(socket, state, &provider, &method).await {
        error!(error = %e, "Browser streaming session failed");
    }
}

/// Run the full streaming session: launch Chrome, stream frames, handle input, detect login
async fn run_streaming_session(
    socket: WebSocket,
    state: SharedState,
    provider: &ProviderConfig,
    method: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    send_status(&mut ws_sender, "launching", "Starting browser...").await;

    let browser = launch_streaming_browser().await?;
    let page = browser
        .new_page(&provider.provider.login_url)
        .await
        .map_err(|e| format!("Failed to open login page: {e}"))?;

    send_status(&mut ws_sender, "navigating", &provider.provider.login_url).await;

    tokio::time::sleep(Duration::from_secs(PAGE_LOAD_WAIT_SECS)).await;
    dismiss_cookie_dialog(&page).await;

    // For OAuth methods, click the provider button before starting screenshot streaming
    if !method.is_empty() && method != "direct" {
        if let Some(selector) = provider.provider.login_oauth_buttons.get(method) {
            send_status(
                &mut ws_sender,
                "oauth",
                &format!("Clicking {method} login..."),
            )
            .await;
            click_oauth_button(&page, selector).await?;
            tokio::time::sleep(Duration::from_secs(PAGE_LOAD_WAIT_SECS)).await;
        } else {
            return Err(format!("Unknown OAuth method: {method}").into());
        }
    }

    let page_arc = Arc::new(page);

    // Login detection via URL polling
    let page_login = Arc::clone(&page_arc);
    let provider_clone = provider.clone();
    let (login_tx, mut login_rx) = tokio::sync::oneshot::channel::<AuthSession>();
    let login_tx = Arc::new(Mutex::new(Some(login_tx)));

    let login_task =
        tokio::spawn(async move { poll_for_login(&page_login, &provider_clone, login_tx).await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(LOGIN_TIMEOUT_SECS);
    let mut screenshot_interval =
        tokio::time::interval(Duration::from_millis(SCREENSHOT_POLL_INTERVAL_MS));

    loop {
        if tokio::time::Instant::now() > deadline {
            let msg = serde_json::json!({"type": "login_failed", "reason": "timeout"});
            let _ = ws_sender.send(Message::Text(msg.to_string().into())).await;
            break;
        }

        tokio::select! {
            biased; // Check branches in order: login > client input > screenshots

            // Login completed (highest priority)
            result = &mut login_rx => {
                if let Ok(session) = result {
                    if let Err(e) = dravr_sciotte::auth::save_session(&session).await {
                        warn!(error = %e, "Failed to persist session");
                    }
                    let session_id = session.session_id.clone();
                    let cookie_count = session.cookies.len();
                    state.write().await.add_session(session);

                    let msg = serde_json::json!({
                        "type": "login_success",
                        "session_id": session_id,
                        "cookie_count": cookie_count,
                    });
                    let _ = ws_sender.send(Message::Text(msg.to_string().into())).await;
                    info!(session_id = %session_id, "Login successful via browser stream");
                }
                break;
            }

            // Client input (second priority — must not be starved by screenshots)
            Some(msg) = ws_receiver.next() => {
                match msg {
                    Ok(Message::Text(text)) => {
                        info!(text = %text, "Received client input");
                        if let Err(e) = handle_client_input(&text, &page_arc).await {
                            warn!(error = %e, "Input dispatch failed");
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => {
                        info!("Client disconnected");
                        break;
                    }
                    _ => {}
                }
            }

            // Screenshot polling (lowest priority)
            _ = screenshot_interval.tick() => {
                let params = ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Jpeg)
                    .quality(SCREENSHOT_QUALITY)
                    .build();
                match page_arc.screenshot(params).await {
                    Ok(data) if !data.is_empty() => {
                        if ws_sender.send(Message::Binary(data.into())).await.is_err() {
                            info!("Client disconnected during frame send");
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        debug!(error = %e, "Screenshot capture failed");
                    }
                }
            }
        }
    }

    login_task.abort();
    Ok(())
}

/// Poll the page URL until login is detected, then capture cookies
async fn poll_for_login(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
    login_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<AuthSession>>>>,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(LOGIN_TIMEOUT_SECS);

    loop {
        if tokio::time::Instant::now() > deadline {
            return;
        }

        let url = page.url().await.ok().flatten().unwrap_or_default();

        let on_failure = provider
            .provider
            .login_failure_patterns
            .iter()
            .any(|p| url.contains(p));

        let on_success = provider
            .provider
            .login_success_patterns
            .iter()
            .any(|p| url.contains(p));

        if !url.is_empty() && !on_failure && on_success {
            info!(url = %url, "Login detected via URL");

            if let Ok(cookies) = page.get_cookies().await {
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

                let session = AuthSession {
                    session_id: generate_session_id(),
                    cookies: cookie_data,
                    created_at: chrono::Utc::now(),
                    expires_at: None,
                };

                let maybe_tx = login_tx.lock().await.take();
                if let Some(tx) = maybe_tx {
                    let _ = tx.send(session);
                }
            }
            return;
        }

        tokio::time::sleep(Duration::from_millis(URL_POLL_INTERVAL_MS)).await;
    }
}

// ============================================================================
// Credential Login (Flow 1 — no streaming, delegates to core library)
// ============================================================================

/// POST /auth/login-with-credentials handler.
///
/// Delegates to the core library's `credential_login()` which launches headless Chrome,
/// fills email/password via JS, submits the form, and polls for login success.
pub async fn credential_login(
    State(state): State<SharedState>,
    Json(request): Json<CredentialLoginRequest>,
) -> impl IntoResponse {
    let result: dravr_sciotte::error::ScraperResult<dravr_sciotte::error::LoginResult> = {
        let guard = state.read().await;
        guard
            .scraper()
            .credential_login(&request.email, &request.password, &request.method)
            .await
    };

    match result {
        Ok(dravr_sciotte::error::LoginResult::Success(session)) => {
            if let Err(e) = dravr_sciotte::auth::save_session(&session).await {
                warn!(error = %e, "Failed to persist session to disk");
            }
            let session_id = session.session_id.clone();
            let cookie_count = session.cookies.len();
            state.write().await.add_session(session);

            info!(session_id = %session_id, "Credential login successful");
            Json(json!({
                "status": "authenticated",
                "session_id": session_id,
                "cookie_count": cookie_count,
            }))
            .into_response()
        }
        Ok(dravr_sciotte::error::LoginResult::OtpRequired) => {
            info!("Credential login requires OTP/2FA");
            Json(json!({
                "status": "otp_required",
                "reason": "Provider requires a one-time password or 2FA verification",
            }))
            .into_response()
        }
        Ok(dravr_sciotte::error::LoginResult::Failed(reason)) => {
            warn!(reason = %reason, "Credential login rejected");
            (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "failed",
                    "reason": reason,
                })),
            )
                .into_response()
        }
        Err(e) => {
            error!(error = %e, "Credential login error");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "error",
                    "reason": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

/// POST /auth/submit-otp handler.
///
/// Submits a one-time password / 2FA code after `credential_login` returned `otp_required`.
pub async fn submit_otp(
    State(state): State<SharedState>,
    Json(request): Json<OtpSubmitRequest>,
) -> impl IntoResponse {
    let result: dravr_sciotte::error::ScraperResult<dravr_sciotte::error::LoginResult> = {
        let guard = state.read().await;
        guard.scraper().submit_otp(&request.code).await
    };

    match result {
        Ok(dravr_sciotte::error::LoginResult::Success(session)) => {
            if let Err(e) = dravr_sciotte::auth::save_session(&session).await {
                warn!(error = %e, "Failed to persist session to disk");
            }
            let session_id = session.session_id.clone();
            let cookie_count = session.cookies.len();
            state.write().await.add_session(session);

            info!(session_id = %session_id, "OTP verification successful");
            Json(json!({
                "status": "authenticated",
                "session_id": session_id,
                "cookie_count": cookie_count,
            }))
            .into_response()
        }
        Ok(dravr_sciotte::error::LoginResult::OtpRequired) => {
            info!("OTP submitted but another verification step required");
            Json(json!({
                "status": "otp_required",
                "reason": "Additional verification step required",
            }))
            .into_response()
        }
        Ok(dravr_sciotte::error::LoginResult::Failed(reason)) => {
            warn!(reason = %reason, "OTP verification rejected");
            (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({
                    "status": "failed",
                    "reason": reason,
                })),
            )
                .into_response()
        }
        Err(e) => {
            error!(error = %e, "OTP submission error");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "error",
                    "reason": e.to_string(),
                })),
            )
                .into_response()
        }
    }
}

// ============================================================================
// WebSocket streaming helpers (cookie dismiss + OAuth click for streaming flow)
// ============================================================================

/// Auto-dismiss cookie consent dialogs for the WebSocket streaming flow.
/// Delegates to the same JS logic used by the core library.
async fn dismiss_cookie_dialog(page: &chromiumoxide::Page) {
    let dismiss_js = r#"
        (function() {
            var btn = document.querySelector('#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll')
                || document.querySelector('[data-cookiefirst-action="accept"]')
                || document.querySelector('button[id*="accept"], button[class*="accept"]');
            if (btn) { btn.click(); return 'dismissed'; }
            var iframes = document.querySelectorAll('iframe');
            for (var i = 0; i < iframes.length; i++) {
                try {
                    var doc = iframes[i].contentDocument;
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
        info!(result = %val, "Cookie dialog auto-dismiss attempt");
    }
}

/// Click an OAuth provider button for the WebSocket streaming flow.
///
/// Supports CSS selectors and `text:` prefix for text-content matching.
async fn click_oauth_button(
    page: &chromiumoxide::Page,
    selector: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let escaped_selector = dravr_sciotte::js_utils::escape_js_selector(selector);
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

    let result = page
        .evaluate(js)
        .await
        .map_err(|e| format!("Failed to click OAuth button '{selector}': {e}"))?;

    let status = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    if status == "not_found" {
        return Err(format!("OAuth button not found for selector: {selector}").into());
    }

    debug!(selector, "OAuth button clicked");
    Ok(())
}

/// Handle a client input message by dispatching the appropriate CDP command
async fn handle_client_input(
    text: &str,
    page: &chromiumoxide::Page,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let msg: ClientMessage = serde_json::from_str(text)?;

    match msg {
        ClientMessage::Click { x, y } => {
            let (cx, cy) = scale_coords(x, y);
            dispatch_click(page, cx, cy).await?;
        }
        ClientMessage::Mousedown { x, y } => {
            let (cx, cy) = scale_coords(x, y);
            dispatch_mouse(page, DispatchMouseEventType::MousePressed, cx, cy).await?;
        }
        ClientMessage::Mouseup { x, y } => {
            let (cx, cy) = scale_coords(x, y);
            dispatch_mouse(page, DispatchMouseEventType::MouseReleased, cx, cy).await?;
        }
        ClientMessage::Mousemove { x, y } => {
            let (cx, cy) = scale_coords(x, y);
            let params = DispatchMouseEventParams {
                r#type: DispatchMouseEventType::MouseMoved,
                x: cx,
                y: cy,
                modifiers: None,
                timestamp: None,
                button: Some(MouseButton::None),
                buttons: None,
                click_count: None,
                force: None,
                tangential_pressure: None,
                tilt_x: None,
                tilt_y: None,
                twist: None,
                delta_x: None,
                delta_y: None,
                pointer_type: None,
            };
            page.execute(params).await?;
        }
        ClientMessage::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            let (cx, cy) = scale_coords(x, y);
            let params = DispatchMouseEventParams {
                r#type: DispatchMouseEventType::MouseWheel,
                x: cx,
                y: cy,
                modifiers: None,
                timestamp: None,
                button: Some(MouseButton::None),
                buttons: None,
                click_count: None,
                force: None,
                tangential_pressure: None,
                tilt_x: None,
                tilt_y: None,
                twist: None,
                delta_x: Some(delta_x),
                delta_y: Some(delta_y),
                pointer_type: None,
            };
            page.execute(params).await?;
        }
        ClientMessage::Keydown { key, code } => {
            dispatch_key(page, DispatchKeyEventType::KeyDown, &key, &code).await?;
        }
        ClientMessage::Keyup { key, code } => {
            dispatch_key(page, DispatchKeyEventType::KeyUp, &key, &code).await?;
        }
        ClientMessage::Text { text } => {
            page.execute(InsertTextParams::new(text)).await?;
        }
        ClientMessage::Resize { width, height } => {
            debug!(width, height, "Client viewport resized");
        }
    }

    Ok(())
}

/// Dispatch a mouse press/release event
async fn dispatch_mouse(
    page: &chromiumoxide::Page,
    event_type: DispatchMouseEventType,
    x: f64,
    y: f64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let params = DispatchMouseEventParams {
        r#type: event_type,
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
    page.execute(params).await?;
    Ok(())
}

/// Dispatch a click (mouse down + mouse up) at the given coordinates
async fn dispatch_click(
    page: &chromiumoxide::Page,
    x: f64,
    y: f64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    dispatch_mouse(page, DispatchMouseEventType::MousePressed, x, y).await?;
    dispatch_mouse(page, DispatchMouseEventType::MouseReleased, x, y).await?;
    Ok(())
}

/// Dispatch a keyboard event
async fn dispatch_key(
    page: &chromiumoxide::Page,
    event_type: DispatchKeyEventType,
    key: &str,
    code: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let params = DispatchKeyEventParams {
        r#type: event_type,
        modifiers: None,
        timestamp: None,
        text: None,
        unmodified_text: None,
        key_identifier: None,
        code: Some(code.to_owned()),
        key: Some(key.to_owned()),
        windows_virtual_key_code: None,
        native_virtual_key_code: None,
        auto_repeat: None,
        is_keypad: None,
        is_system_key: None,
        location: None,
        commands: None,
    };
    page.execute(params).await?;
    Ok(())
}

/// Scale client coordinates to Chrome viewport coordinates.
/// Client sends coordinates already scaled to Chrome viewport space.
/// Pass them through directly to CDP — no further scaling needed.
fn scale_coords(x: f64, y: f64) -> (f64, f64) {
    (x, y)
}

/// Send a JSON status message over the WebSocket
async fn send_status(
    ws: &mut futures::stream::SplitSink<WebSocket, Message>,
    state_name: &str,
    detail: &str,
) {
    let msg = serde_json::json!({
        "type": "status",
        "state": state_name,
        "message": detail
    });
    let _ = ws.send(Message::Text(msg.to_string().into())).await;
}

/// Launch a Chrome browser configured for streaming (headless with rendering)
async fn launch_streaming_browser() -> Result<Browser, Box<dyn std::error::Error + Send + Sync>> {
    let config = BrowserConfig::builder()
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-setuid-sandbox")
        .arg("--disable-blink-features=AutomationControlled")
        .arg("--disable-popup-blocking")
        .arg("--user-agent=Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .window_size(VIEWPORT_WIDTH, VIEWPORT_HEIGHT)
        .build()
        .map_err(|e| format!("Failed to configure browser: {e}"))?;

    let (browser, mut handler) = Browser::launch(config)
        .await
        .map_err(|e| format!("Failed to launch browser: {e}"))?;

    tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            debug!(?event, "Streaming browser event");
        }
    });

    Ok(browser)
}

/// Generate a unique session identifier
fn generate_session_id() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

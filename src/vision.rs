// ABOUTME: Vision-based activity scraper using LLM screenshot analysis via embacle
// ABOUTME: Resilient alternative to CSS selectors — survives UI redesigns by using visual understanding
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chrono::Utc;
use embacle::types::{ChatMessage, ChatRequest, ImagePart, LlmProvider};
use tokio::sync::Mutex;
use tokio::time::{self, Instant};
use tracing::{debug, info, warn};

use crate::browser_utils;
use crate::config::ScraperConfig;
use crate::error::{LoginResult, ScraperError, ScraperResult, TwoFactorOption};
use crate::models::{
    self, Activity, ActivityParams, AthleteProfile, AuthSession, CookieData, DailySummary,
    HealthParams,
};
use crate::provider::ProviderConfig;
use crate::types::ActivityScraper;

/// Vision-based scraper that uses LLM screenshot analysis instead of CSS selectors.
///
/// Implements the same `ActivityScraper` trait as `ChromeScraper` but extracts data
/// by sending page screenshots to a vision-capable LLM (via embacle) with structured
/// extraction prompts defined in markdown files.
///
/// # Feature Flag
///
/// Requires the `vision` feature: `dravr-sciotte = { features = ["vision"] }`
pub struct VisionScraper {
    config: ScraperConfig,
    provider: ProviderConfig,
    llm: Arc<dyn LlmProvider>,
    browser: Mutex<Option<Arc<Browser>>>,
    pending_login: Mutex<Option<(Browser, chromiumoxide::Page)>>,
}

impl VisionScraper {
    /// Create a vision scraper with a provider config and an embacle LLM provider
    pub fn new(config: ScraperConfig, provider: ProviderConfig, llm: Arc<dyn LlmProvider>) -> Self {
        Self {
            config,
            provider,
            llm,
            browser: Mutex::new(None),
            pending_login: Mutex::new(None),
        }
    }

    /// Get or create a headless browser instance for scraping
    async fn get_browser(&self) -> ScraperResult<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        if let Some(browser) = guard.as_ref() {
            return Ok(Arc::clone(browser));
        }

        let browser = browser_utils::launch_browser(&self.config, true).await?;
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));

        info!("Vision scraper browser launched");
        Ok(browser)
    }

    /// Take a full-page screenshot and encode as base64 PNG
    async fn screenshot_base64(&self, page: &chromiumoxide::Page) -> ScraperResult<String> {
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(true)
            .build();

        let data = page
            .screenshot(params)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to take screenshot: {e}"),
            })?;

        Ok(BASE64_STANDARD.encode(&data))
    }

    /// Send a screenshot + prompt to the LLM and get a text response
    async fn ask_llm_with_screenshot(
        &self,
        screenshot_b64: &str,
        prompt: &str,
    ) -> ScraperResult<String> {
        let image = ImagePart::new(screenshot_b64.to_owned(), "image/png").map_err(|e| {
            ScraperError::Internal {
                reason: format!("Failed to create image part: {e}"),
            }
        })?;

        let message = ChatMessage::user_with_images(prompt.to_owned(), vec![image]);

        let request = ChatRequest {
            messages: vec![message],
            model: None,
            temperature: Some(0.0),
            max_tokens: Some(4096),
            stream: false,
            tools: None,
            tool_choice: None,
            top_p: None,
            stop: None,
            response_format: None,
        };

        let response = self
            .llm
            .complete(&request)
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("LLM request failed: {e}"),
            })?;

        Ok(response.content)
    }

    /// Load a prompt from a markdown file path
    fn load_prompt(path: &str) -> ScraperResult<String> {
        fs::read_to_string(path).map_err(|e| ScraperError::Config {
            reason: format!("Failed to read vision prompt '{path}': {e}"),
        })
    }

    /// Handle the provider login page — click OAuth button or fill email
    async fn handle_provider_login(
        &self,
        page: &chromiumoxide::Page,
        analysis: &PageAnalysis,
        config: &ScraperConfig,
        method: &str,
        email: &str,
    ) -> ScraperResult<()> {
        if method == "google" || method == "apple" {
            info!(method, "Vision: clicking OAuth button");
            // Try provider config selector first (reliable text matching)
            let clicked =
                if let Some(selector) = self.provider.provider.login_oauth_buttons.get(method) {
                    browser_utils::click_element(page, selector).await.is_ok()
                } else {
                    false
                };
            // Fall back to LLM-detected coordinates
            if !clicked {
                let label = if method == "google" {
                    "Google"
                } else {
                    "Apple"
                };
                if let Some(action) = analysis.find_action_by_label(label) {
                    let _ = browser_utils::cdp_click_at(page, action.x, action.y).await;
                }
            }
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
        } else {
            info!("Vision: filling email field");
            self.vision_fill_and_submit(page, analysis, email).await?;
        }
        Ok(())
    }

    /// Handle 2FA pages — returns a `LoginResult` if a decision is needed, `None` to keep polling
    fn handle_2fa_page(analysis: &PageAnalysis) -> Option<LoginResult> {
        match analysis.page_type.as_str() {
            "two_factor_selection" => {
                info!("Vision: 2FA selection page detected");
                let options: Vec<TwoFactorOption> = analysis
                    .two_factor_options
                    .iter()
                    .map(|o| TwoFactorOption {
                        id: o.id.clone(),
                        label: o.label.clone(),
                    })
                    .collect();
                if options.is_empty() {
                    return None;
                }
                // Can't move browser+page here, so we clone what we need
                // The caller will store them after we return
                Some(LoginResult::TwoFactorChoice(options))
            }
            "otp_entry" => {
                info!("Vision: OTP entry page detected");
                Some(LoginResult::OtpRequired)
            }
            "number_match" | "phone_approval" => {
                if let Some(ref number) = analysis.match_number {
                    info!(number, "Vision: number matching challenge");
                    return Some(LoginResult::NumberMatch(number.clone()));
                }
                info!("Vision: phone approval — waiting");
                None
            }
            _ => None,
        }
    }

    /// Handle passkey challenge — click "Try another way", then "Enter your password"
    async fn handle_passkey_challenge(&self, page: &chromiumoxide::Page, config: &ScraperConfig) {
        let _ = browser_utils::click_element(page, "text:Try another way, text:Essayer autrement")
            .await;
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
        let pwd_selector = r#"input[type="password"], input[name="Passwd"]"#;
        if !browser_utils::element_exists(page, pwd_selector).await {
            let _ = browser_utils::click_element(
                page,
                "text:Enter your password, text:Saisir votre mot de passe",
            )
            .await;
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
        }
    }

    /// Dismiss a cookie consent dialog using JS fallback and LLM coordinates
    async fn dismiss_cookie(
        &self,
        page: &chromiumoxide::Page,
        analysis: &PageAnalysis,
        config: &ScraperConfig,
    ) {
        browser_utils::dismiss_cookie_dialog(page).await;
        if let Some(action) = analysis.find_click_action() {
            let _ = browser_utils::cdp_click_at(page, action.x, action.y).await;
        }
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
    }

    /// Extract a number matching challenge number from the current page via LLM
    pub async fn extract_match_number(&self, page: &chromiumoxide::Page) -> Option<String> {
        let analysis = self.analyze_page(page).await.ok()?;
        analysis.match_number
    }

    /// Fill a field and click a button based on page analysis.
    /// Uses common CSS selectors first (reliable), LLM coordinates as fallback.
    async fn vision_fill_and_submit(
        &self,
        page: &chromiumoxide::Page,
        analysis: &PageAnalysis,
        text: &str,
    ) -> ScraperResult<()> {
        // Try common selectors first, fall back to LLM coordinates
        let email_selectors = r#"input[type="email"], input[name="email"], #email"#;
        let password_selectors =
            r#"input[type="password"], input[name="password"], input[name="Passwd"]"#;
        let submit_selectors = r#"#identifierNext button, #identifierNext, #passwordNext button, #passwordNext, button[type="submit"], text:Next, text:Log In, text:Sign In"#;

        // Determine if this is a password or email field
        let filled = if browser_utils::element_exists(page, password_selectors).await {
            browser_utils::fill_input_field(page, password_selectors, text)
                .await
                .is_ok()
        } else if browser_utils::element_exists(page, email_selectors).await {
            browser_utils::fill_input_field(page, email_selectors, text)
                .await
                .is_ok()
        } else if let Some(field) = analysis.find_fill_action() {
            browser_utils::cdp_click_at(page, field.x, field.y).await?;
            page.execute(InsertTextParams::new(text)).await.is_ok()
        } else {
            false
        };

        if !filled {
            warn!("Vision: could not fill input field");
        }

        time::sleep(Duration::from_millis(self.config.form_interaction_delay_ms)).await;

        // Click submit — try selectors first, then LLM coordinates
        let clicked = browser_utils::click_element(page, submit_selectors)
            .await
            .is_ok();
        if !clicked {
            if let Some(button) = analysis.find_click_action() {
                let _ = browser_utils::cdp_click_at(page, button.x, button.y).await;
            }
        }

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;
        Ok(())
    }

    /// Analyze a page screenshot to determine its type and available actions
    async fn analyze_page(&self, page: &chromiumoxide::Page) -> ScraperResult<PageAnalysis> {
        let screenshot = self.screenshot_base64(page).await?;

        let prompt = self
            .provider
            .provider
            .vision_page_analysis_prompt
            .as_deref()
            .map(Self::load_prompt)
            .transpose()?
            .unwrap_or_else(|| DEFAULT_PAGE_ANALYSIS_PROMPT.to_owned());

        let response = self.ask_llm_with_screenshot(&screenshot, &prompt).await?;
        let json = extract_json(&response);

        serde_json::from_str(&json).map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to parse page analysis: {e}\nRaw response: {response}"),
        })
    }

    /// Extract activity list data from a screenshot using the list page vision prompt
    async fn extract_list_data(
        &self,
        page: &chromiumoxide::Page,
    ) -> ScraperResult<Vec<serde_json::Value>> {
        let screenshot = self.screenshot_base64(page).await?;

        let prompt = self
            .provider
            .list_page
            .vision_prompt
            .as_deref()
            .map(Self::load_prompt)
            .transpose()?
            .ok_or_else(|| ScraperError::Config {
                reason: "No list_page.vision_prompt configured for this provider".to_owned(),
            })?;

        let response = self.ask_llm_with_screenshot(&screenshot, &prompt).await?;
        let json = extract_json(&response);

        serde_json::from_str(&json).map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to parse activity list: {e}\nRaw response: {response}"),
        })
    }

    /// Extract activity detail data from a screenshot using the detail page vision prompt
    async fn extract_detail_data(
        &self,
        page: &chromiumoxide::Page,
    ) -> ScraperResult<serde_json::Value> {
        let screenshot = self.screenshot_base64(page).await?;

        let prompt = self
            .provider
            .detail_page
            .vision_prompt
            .as_deref()
            .map(Self::load_prompt)
            .transpose()?
            .ok_or_else(|| ScraperError::Config {
                reason: "No detail_page.vision_prompt configured for this provider".to_owned(),
            })?;

        let response = self.ask_llm_with_screenshot(&screenshot, &prompt).await?;
        let json = extract_json(&response);

        serde_json::from_str(&json).map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to parse activity detail: {e}\nRaw response: {response}"),
        })
    }

    /// Inject session cookies into a browser page
    async fn inject_cookies(
        &self,
        page: &chromiumoxide::Page,
        session: &AuthSession,
    ) -> ScraperResult<()> {
        use chromiumoxide::cdp::browser_protocol::network::CookieParam;

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
}

#[async_trait]
impl ActivityScraper for VisionScraper {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        // Vision scraper delegates to credential_login for programmatic login
        Err(ScraperError::Auth {
            reason: "VisionScraper requires credential_login() — browser_login() opens a visible window which is not needed with vision-based navigation".to_owned(),
        })
    }

    async fn credential_login(
        &self,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult> {
        let config = &self.config;
        info!(
            provider = %self.provider.provider.name,
            method,
            "Starting vision-based credential login"
        );

        let browser = browser_utils::launch_browser(config, false).await?;
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open login page: {e}"),
            })?;

        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Vision-driven login loop: analyze page, take action, repeat
        let deadline = Instant::now() + Duration::from_secs(config.login_timeout_secs);
        let mut cookie_dismiss_attempts = 0u32;

        loop {
            if Instant::now() > deadline {
                return Err(ScraperError::Auth {
                    reason: "Vision login timed out".to_owned(),
                });
            }

            let analysis = self.analyze_page(&page).await?;
            debug!(page_type = ?analysis.page_type, "Vision page analysis");

            match analysis.page_type.as_str() {
                "cookie_consent" => {
                    cookie_dismiss_attempts += 1;
                    info!(
                        attempt = cookie_dismiss_attempts,
                        "Vision: dismissing cookie consent"
                    );
                    self.dismiss_cookie(&page, &analysis, config).await;
                    if cookie_dismiss_attempts > 3 {
                        warn!("Cookie dismiss stuck, skipping");
                    }
                }
                "provider_login" => {
                    self.handle_provider_login(&page, &analysis, config, method, email)
                        .await?;
                }
                "oauth_email" => {
                    info!("Vision: filling OAuth email field");
                    self.vision_fill_and_submit(&page, &analysis, email).await?;
                }
                "oauth_password" => {
                    info!("Vision: filling password field");
                    self.vision_fill_and_submit(&page, &analysis, password)
                        .await?;
                }
                "passkey_challenge" => {
                    info!("Vision: bypassing passkey challenge");
                    self.handle_passkey_challenge(&page, config).await;
                }
                "two_factor_selection" | "otp_entry" | "phone_approval" | "number_match" => {
                    if let Some(result) = Self::handle_2fa_page(&analysis) {
                        *self.pending_login.lock().await = Some((browser, page));
                        return Ok(result);
                    }
                    time::sleep(Duration::from_secs(3)).await;
                }
                "success" => {
                    info!("Vision: login success detected");
                    let session = capture_cookies_as_session(&page).await?;
                    return Ok(LoginResult::Success(session));
                }
                "error" => {
                    let msg = analysis
                        .error_message
                        .unwrap_or_else(|| "Login failed".to_owned());
                    return Ok(LoginResult::Failed(msg));
                }
                _ => {
                    warn!(
                        page_type = analysis.page_type,
                        "Vision: unknown page type, waiting"
                    );
                    time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
                }
            }
        }
    }

    async fn submit_otp(&self, code: &str) -> ScraperResult<LoginResult> {
        let (browser, page) =
            self.pending_login
                .lock()
                .await
                .take()
                .ok_or_else(|| ScraperError::Auth {
                    reason: "No pending OTP session".to_owned(),
                })?;

        let analysis = self.analyze_page(&page).await?;
        self.vision_fill_and_submit(&page, &analysis, code).await?;

        // Check result
        let result_analysis = self.analyze_page(&page).await?;
        match result_analysis.page_type.as_str() {
            "success" => {
                let session = capture_cookies_as_session(&page).await?;
                Ok(LoginResult::Success(session))
            }
            "error" => Ok(LoginResult::Failed(
                result_analysis
                    .error_message
                    .unwrap_or_else(|| "OTP verification failed".to_owned()),
            )),
            _ => {
                *self.pending_login.lock().await = Some((browser, page));
                Ok(LoginResult::OtpRequired)
            }
        }
    }

    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult> {
        let (browser, page) =
            self.pending_login
                .lock()
                .await
                .take()
                .ok_or_else(|| ScraperError::Auth {
                    reason: "No pending 2FA session".to_owned(),
                })?;

        let analysis = self.analyze_page(&page).await?;
        let option = analysis
            .two_factor_options
            .iter()
            .find(|o| o.id == option_id);

        if let Some(opt) = option {
            browser_utils::cdp_click_at(&page, opt.x, opt.y).await?;
        } else {
            *self.pending_login.lock().await = Some((browser, page));
            return Err(ScraperError::Auth {
                reason: format!("2FA option '{option_id}' not found"),
            });
        }

        let timeout = if option_id == "app" {
            self.config.phone_tap_timeout_secs
        } else {
            self.config.password_step_timeout_secs
        };

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;

        let deadline = Instant::now() + Duration::from_secs(timeout);
        loop {
            if Instant::now() > deadline {
                return Err(ScraperError::Auth {
                    reason: "2FA verification timed out".to_owned(),
                });
            }

            let result_analysis = self.analyze_page(&page).await?;
            match result_analysis.page_type.as_str() {
                "otp_entry" => {
                    *self.pending_login.lock().await = Some((browser, page));
                    return Ok(LoginResult::OtpRequired);
                }
                "success" => {
                    let session = capture_cookies_as_session(&page).await?;
                    return Ok(LoginResult::Success(session));
                }
                "error" => {
                    return Ok(LoginResult::Failed(
                        result_analysis
                            .error_message
                            .unwrap_or_else(|| "2FA failed".to_owned()),
                    ));
                }
                _ => {
                    time::sleep(Duration::from_millis(self.config.login_poll_interval_ms)).await;
                }
            }
        }
    }

    async fn is_authenticated(&self, session: &AuthSession) -> bool {
        if let Some(expires) = session.expires_at {
            if Utc::now() > expires {
                return false;
            }
        }
        !session.cookies.is_empty()
    }

    async fn get_activities(
        &self,
        session: &AuthSession,
        params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>> {
        let browser = self.get_browser().await?;

        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        self.inject_cookies(&page, session).await?;

        page.goto(&self.provider.list_page.url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to navigate to list page: {e}"),
            })?;

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;

        let target = params.limit.unwrap_or(20) as usize;
        let items = self.extract_list_data(&page).await?;

        let activities: Vec<Activity> = items
            .into_iter()
            .take(target)
            .filter_map(|v| parse_vision_activity(&v))
            .collect();

        info!(count = activities.len(), "Vision: activities extracted");
        Ok(activities)
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let browser = self.get_browser().await?;
        let url = self.provider.detail_url(activity_id);

        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        self.inject_cookies(&page, session).await?;

        page.goto(&url).await.map_err(|e| ScraperError::Browser {
            reason: format!("Failed to navigate to activity page: {e}"),
        })?;

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;

        let data = self.extract_detail_data(&page).await?;

        Ok(parse_vision_activity_detail(activity_id, &data))
    }

    async fn get_athlete(&self, session: &AuthSession) -> ScraperResult<AthleteProfile> {
        let profile_url = self
            .provider
            .provider
            .profile_url
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no profile_url configured".to_owned(),
            })?;

        let browser = self.get_browser().await?;
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        self.inject_cookies(&page, session).await?;

        page.goto(profile_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to navigate to profile: {e}"),
            })?;

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;

        let screenshot = self.screenshot_base64(&page).await?;
        let prompt = "Extract the athlete's profile from this page. Return JSON with fields: display_name, firstname, lastname, profile_picture_url, city, country. Only include fields that are visible.";
        let response = self.ask_llm_with_screenshot(&screenshot, prompt).await?;
        let json = extract_json(&response);

        serde_json::from_str(&json).map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to parse athlete profile: {e}"),
        })
    }

    async fn get_daily_summary(
        &self,
        _session: &AuthSession,
        _params: &HealthParams,
    ) -> ScraperResult<DailySummary> {
        Err(ScraperError::Config {
            reason: "Vision-based health summary extraction is not yet supported".to_owned(),
        })
    }
}

// ============================================================================
// Page analysis types
// ============================================================================

/// Result of LLM page analysis
#[derive(Debug, serde::Deserialize)]
struct PageAnalysis {
    page_type: String,
    #[serde(default)]
    actions: Vec<PageAction>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    two_factor_options: Vec<TwoFactorOptionCoords>,
    /// Number shown on screen for number matching challenge (e.g., "78")
    #[serde(default)]
    match_number: Option<String>,
}

/// An action the LLM identified on the page
#[derive(Debug, serde::Deserialize)]
struct PageAction {
    #[serde(rename = "type")]
    action_type: String,
    label: String,
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
}

/// A 2FA option with click coordinates
#[derive(Debug, serde::Deserialize)]
struct TwoFactorOptionCoords {
    id: String,
    label: String,
    #[serde(default)]
    x: f64,
    #[serde(default)]
    y: f64,
}

impl PageAnalysis {
    fn find_click_action(&self) -> Option<&PageAction> {
        self.actions.iter().find(|a| a.action_type == "click")
    }

    fn find_fill_action(&self) -> Option<&PageAction> {
        self.actions.iter().find(|a| a.action_type == "fill")
    }

    fn find_action_by_label(&self, partial: &str) -> Option<&PageAction> {
        let lower = partial.to_lowercase();
        self.actions
            .iter()
            .find(|a| a.label.to_lowercase().contains(&lower))
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Extract JSON from an LLM response that may include markdown fences
fn extract_json(response: &str) -> String {
    let trimmed = response.trim();

    // Try extracting from ```json ... ``` fences
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_owned();
        }
    }

    // Try extracting from ``` ... ``` fences
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_owned();
        }
    }

    // Already JSON
    trimmed.to_owned()
}

/// Parse a vision-extracted activity from a JSON value into an Activity
fn parse_vision_activity(v: &serde_json::Value) -> Option<Activity> {
    let id = v["id"].as_str()?.to_owned();
    let name = v["name"].as_str().unwrap_or("").to_owned();

    Some(Activity {
        id,
        name,
        sport_type: models::SportType::from_strava(v["type"].as_str().unwrap_or("")),
        start_date: Utc::now(),
        duration_seconds: 0,
        distance_meters: None,
        elevation_gain: None,
        average_heart_rate: None,
        max_heart_rate: None,
        average_power: None,
        max_power: None,
        normalized_power: None,
        average_cadence: None,
        average_speed: None,
        max_speed: None,
        suffer_score: None,
        calories: None,
        elapsed_time_seconds: None,
        pace: v.get("pace").and_then(|p| p.as_str()).map(String::from),
        gap: None,
        device_name: None,
        gear_name: None,
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: None,
        city: None,
        region: None,
        country: None,
        perceived_exertion: None,
        sport_type_detail: v.get("type").and_then(|t| t.as_str()).map(String::from),
        workout_type: None,
        training_stress_score: None,
        intensity_factor: None,
        start_latitude: None,
        start_longitude: None,
        segment_efforts: None,
        provider: "vision-scraper".to_owned(),
    })
}

/// Parse a vision-extracted activity detail into an Activity
fn parse_vision_activity_detail(id: &str, v: &serde_json::Value) -> Activity {
    Activity {
        id: id.to_owned(),
        name: v["name"].as_str().unwrap_or("").to_owned(),
        sport_type: models::SportType::from_strava(v["type"].as_str().unwrap_or("")),
        start_date: Utc::now(),
        duration_seconds: 0,
        distance_meters: None,
        elevation_gain: None,
        average_heart_rate: None,
        max_heart_rate: None,
        average_power: v
            .get("avg_power")
            .and_then(|p| p.as_str())
            .and_then(|s| s.replace('W', "").replace("watts", "").trim().parse().ok()),
        max_power: None,
        normalized_power: None,
        average_cadence: None,
        average_speed: None,
        max_speed: None,
        suffer_score: None,
        calories: None,
        elapsed_time_seconds: None,
        pace: v.get("pace").and_then(|p| p.as_str()).map(String::from),
        gap: v.get("gap").and_then(|p| p.as_str()).map(String::from),
        device_name: v.get("device").and_then(|d| d.as_str()).map(String::from),
        gear_name: v.get("gear").and_then(|g| g.as_str()).map(String::from),
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: v.get("weather").and_then(|w| w.as_str()).map(String::from),
        city: None,
        region: None,
        country: None,
        perceived_exertion: v
            .get("perceived_exertion")
            .and_then(|p| p.as_str())
            .map(String::from),
        sport_type_detail: v.get("type").and_then(|t| t.as_str()).map(String::from),
        workout_type: None,
        training_stress_score: None,
        intensity_factor: None,
        start_latitude: None,
        start_longitude: None,
        segment_efforts: None,
        provider: "vision-scraper".to_owned(),
    }
}

/// Capture cookies from the page and build an `AuthSession`
async fn capture_cookies_as_session(page: &chromiumoxide::Page) -> ScraperResult<AuthSession> {
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
    Ok(AuthSession {
        session_id: generate_session_id(),
        cookies: cookie_data,
        created_at: Utc::now(),
        expires_at: None,
    })
}

/// Generate a unique session identifier
fn generate_session_id() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}-{:x}", d.as_secs(), d.subsec_nanos())
}

/// Launch a browser for vision scraping
/// Default page analysis prompt when no provider-specific one is configured
const DEFAULT_PAGE_ANALYSIS_PROMPT: &str = r#"Analyze this web page screenshot. Return a JSON object with:
- "page_type": one of "provider_login", "oauth_email", "oauth_password", "cookie_consent", "passkey_challenge", "two_factor_selection", "otp_entry", "phone_approval", "number_match", "success", "error", "unknown"
- "actions": array of {"type": "click"|"fill", "label": "description", "x": number, "y": number}
- "error_message": string or null
- "two_factor_options": array of {"id": "otp"|"app"|"sms", "label": "description", "x": number, "y": number}
- "match_number": if page shows a number matching challenge (e.g. "Tap 78 on your phone"), extract the number as a string (e.g. "78"), otherwise null
Return valid JSON only."#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_plain() {
        let input = r#"[{"id": "1", "name": "Run"}]"#;
        assert_eq!(extract_json(input), input);
    }

    #[test]
    fn extract_json_from_fenced_block() {
        let input = "Here is the data:\n```json\n[{\"id\": \"1\"}]\n```\nDone.";
        assert_eq!(extract_json(input), r#"[{"id": "1"}]"#);
    }

    #[test]
    fn extract_json_from_plain_fence() {
        let input = "```\n{\"x\": 1}\n```";
        assert_eq!(extract_json(input), r#"{"x": 1}"#);
    }

    #[test]
    fn parse_vision_activity_minimal() {
        let v = serde_json::json!({"id": "123", "name": "Run", "type": "Run"});
        let activity = parse_vision_activity(&v).unwrap(); // Safe: test with valid JSON fields
        assert_eq!(activity.id, "123");
        assert_eq!(activity.name, "Run");
    }

    #[test]
    fn parse_vision_activity_missing_id() {
        let v = serde_json::json!({"name": "Run"});
        assert!(parse_vision_activity(&v).is_none());
    }

    #[test]
    fn page_analysis_deserializes() {
        let json = r#"{
            "page_type": "oauth_email",
            "actions": [
                {"type": "fill", "label": "email field", "x": 400, "y": 300},
                {"type": "click", "label": "Next button", "x": 400, "y": 450}
            ],
            "error_message": null,
            "two_factor_options": []
        }"#;
        let analysis: PageAnalysis = serde_json::from_str(json).unwrap(); // Safe: test with valid JSON
        assert_eq!(analysis.page_type, "oauth_email");
        assert_eq!(analysis.actions.len(), 2);
        assert!(analysis.find_fill_action().is_some());
        assert!(analysis.find_click_action().is_some());
    }

    #[test]
    fn page_analysis_find_action_by_label() {
        let json = r#"{
            "page_type": "passkey_challenge",
            "actions": [
                {"type": "click", "label": "Try another way", "x": 300, "y": 500},
                {"type": "click", "label": "Use passkey", "x": 300, "y": 400}
            ]
        }"#;
        let analysis: PageAnalysis = serde_json::from_str(json).unwrap(); // Safe: test with valid JSON
        let action = analysis.find_action_by_label("another way");
        assert!(action.is_some());
        assert!((action.unwrap().y - 500.0).abs() < 0.1); // Safe: guarded by is_some assert above
    }

    #[test]
    fn two_factor_options_deserialize() {
        let json = r#"{
            "page_type": "two_factor_selection",
            "actions": [],
            "two_factor_options": [
                {"id": "otp", "label": "Google Authenticator", "x": 100, "y": 200},
                {"id": "app", "label": "Tap Yes on phone", "x": 100, "y": 300}
            ]
        }"#;
        let analysis: PageAnalysis = serde_json::from_str(json).unwrap(); // Safe: test with valid JSON
        assert_eq!(analysis.two_factor_options.len(), 2);
        assert_eq!(analysis.two_factor_options[0].id, "otp");
    }
}

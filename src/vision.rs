// ABOUTME: Vision-based activity scraper using LLM screenshot analysis via a VisionModel
// ABOUTME: Resilient alternative to CSS selectors — survives UI redesigns by using visual understanding
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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
use crate::pending_login::PendingLogin;
use crate::provider::ProviderConfig;
use crate::types::ActivityScraper;
use crate::vision_model::VisionModel;

/// JPEG quality (0-100) for vision-analysis screenshots. Low enough to keep a
/// full-page SSO login screenshot far under the model's request-size cap, high
/// enough for the LLM to read field labels and buttons.
const VISION_SCREENSHOT_QUALITY: i64 = 55;

/// Raw screenshot bytes above which we drop from full-page to viewport-only
/// before base64-encoding. The vision request has a ~5 MB cap and base64
/// inflates by 4/3, so keep raw under ~3 MB to leave headroom for the prompt.
const MAX_VISION_SCREENSHOT_BYTES: usize = 3_000_000;

/// An in-flight interactive login parked between steps: the live browser plus
/// the credentials later steps may still need — e.g. Google shows its password
/// page only after the user picks "Enter your password" on the verification
/// chooser, so `select_two_factor` must be able to fill it. Held in memory only
/// (never logged, never persisted), same lifetime and TTL as the browser itself.
/// `two_factor_options` snapshots the chooser analysis that produced a parked
/// `TwoFactorChoice`: the LLM assigns non-deterministic option ids per analysis,
/// so the echoed id must resolve against the SAME analysis the caller saw (its
/// coordinates stay valid — the parked page hasn't changed).
struct ParkedVisionLogin {
    browser: Browser,
    page: chromiumoxide::Page,
    email: String,
    password: String,
    two_factor_options: Vec<TwoFactorOptionCoords>,
}

/// Vision-based scraper that uses LLM screenshot analysis instead of CSS selectors.
///
/// Implements the same `ActivityScraper` trait as `ChromeScraper` but extracts data
/// by sending page screenshots to a vision-capable LLM (via a [`VisionModel`]) with structured
/// extraction prompts defined in markdown files.
///
/// # Feature Flag
///
/// Requires the `vision` feature: `dravr-sciotte = { features = ["vision"] }`
pub struct VisionScraper {
    config: ScraperConfig,
    provider: ProviderConfig,
    llm: Arc<dyn VisionModel>,
    browser: Mutex<Option<Arc<Browser>>>,
    /// Parked login flow (browser + page + credentials) between vision-driven
    /// `credential_login` and a follow-up 2FA call. Wrapped in `PendingLogin`
    /// so abandoned flows are evicted after `ScraperConfig::pending_login_ttl_secs`.
    pending_login: Mutex<Option<PendingLogin<ParkedVisionLogin>>>,
}

impl VisionScraper {
    /// Create a vision scraper with a provider config and a [`VisionModel`]
    pub fn new(config: ScraperConfig, provider: ProviderConfig, llm: Arc<dyn VisionModel>) -> Self {
        Self {
            config,
            provider,
            llm,
            browser: Mutex::new(None),
            pending_login: Mutex::new(None),
        }
    }

    /// Park an in-flight 2FA login so a follow-up `submit_otp` /
    /// `select_two_factor` can resume the same Chrome page. Drops any
    /// previously parked session so the field stays a single-slot queue.
    async fn store_pending_login(&self, parked: ParkedVisionLogin) {
        *self.pending_login.lock().await = Some(PendingLogin::new(parked));
    }

    /// Take the parked 2FA login if it was stored less than
    /// `config.pending_login_ttl_secs` ago. Expired entries are dropped on
    /// access — chromiumoxide's `kill_on_drop` reaps the held Chrome.
    async fn take_pending_login(&self) -> Option<ParkedVisionLogin> {
        let parked = self.pending_login.lock().await.take()?;
        let ttl = Duration::from_secs(self.config.pending_login_ttl_secs);
        let result = parked.into_inner_if_fresh(ttl);
        if result.is_none() {
            debug!(
                ttl_secs = self.config.pending_login_ttl_secs,
                "Evicted expired vision pending login (Chrome will be reaped on drop)"
            );
        }
        result
    }

    /// Get or create a headless browser instance for scraping
    async fn get_browser(&self) -> ScraperResult<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        if let Some(browser) = guard.as_ref() {
            return Ok(Arc::clone(browser));
        }

        // Vision scraper does not yet thread session_id through — pass None
        // (ephemeral profile). When VisionScraper gets per-session profile
        // support, swap to Some(session_id).
        let browser = browser_utils::launch_browser(&self.config, true, None).await?;
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));

        info!("Vision scraper browser launched");
        Ok(browser)
    }

    /// Capture the current page as a base64 JPEG for the vision LLM.
    ///
    /// JPEG — not PNG — because a full-page PNG of a tall SSO login page can
    /// exceed the model's request-size cap (a real Garmin login hit 7.4 MB vs a
    /// 5 MB limit and was rejected as "image too large", silently breaking
    /// vision login). JPEG at [`VISION_SCREENSHOT_QUALITY`] keeps the same
    /// full-page context an order of magnitude smaller. If a JPEG is *still*
    /// oversized (a pathologically long page), fall back to a viewport-only
    /// capture so page analysis always has an image to reason about rather than
    /// erroring — a partial view beats no view.
    async fn screenshot_base64(&self, page: &chromiumoxide::Page) -> ScraperResult<String> {
        let capture = |full_page: bool| {
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Jpeg)
                .quality(VISION_SCREENSHOT_QUALITY)
                .full_page(full_page)
                .build()
        };

        let shot = |params| async {
            page.screenshot(params)
                .await
                .map_err(|e| ScraperError::Browser {
                    reason: format!("Failed to take screenshot: {e}"),
                })
        };

        let mut data = shot(capture(true)).await?;
        if data.len() > MAX_VISION_SCREENSHOT_BYTES {
            warn!(
                bytes = data.len(),
                cap = MAX_VISION_SCREENSHOT_BYTES,
                "Full-page vision screenshot over the model size cap — retrying viewport-only"
            );
            data = shot(capture(false)).await?;
        }

        Ok(BASE64_STANDARD.encode(&data))
    }

    /// Send a screenshot + prompt to the LLM and get a text response
    async fn ask_llm_with_screenshot(
        &self,
        screenshot_b64: &str,
        prompt: &str,
    ) -> ScraperResult<String> {
        self.llm
            .analyze_screenshot(prompt, screenshot_b64)
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("LLM request failed: {e}"),
            })
    }

    /// Load a prompt from a markdown file path.
    ///
    /// Resolves via `DRAVR_SCIOTTE_SCRIPTS_DIR` overrides first, then the
    /// compiled-in prompts, then the raw path — see [`resolve_prompt`].
    fn load_prompt(path: &str) -> ScraperResult<String> {
        let override_dir = env::var("DRAVR_SCIOTTE_SCRIPTS_DIR")
            .ok()
            .map(PathBuf::from);
        resolve_prompt(path, override_dir.as_deref())
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

    /// Poll the parked page after a 2FA option click until a terminal outcome.
    ///
    /// Same page-type coverage as the `credential_login` driver: a 2FA choice
    /// can lead anywhere in the provider's flow — Google's "Enter your
    /// password" lands on the `oauth_password` page (fillable because the
    /// parked flow carries the credentials), a chained chooser re-parks as
    /// another `TwoFactorChoice`, and a phone tap surfaces as `NumberMatch`.
    async fn await_two_factor_outcome(
        &self,
        parked: ParkedVisionLogin,
        timeout_secs: u64,
    ) -> ScraperResult<LoginResult> {
        let ParkedVisionLogin {
            browser,
            page,
            email,
            password,
            ..
        } = parked;

        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            if Instant::now() > deadline {
                return Err(ScraperError::Auth {
                    reason: "2FA verification timed out".to_owned(),
                });
            }

            let result_analysis = self.analyze_page(&page).await?;
            match result_analysis.page_type.as_str() {
                "oauth_email" => {
                    info!("Vision: 2FA continuation — filling email field");
                    self.vision_fill_and_submit(&page, &result_analysis, &email)
                        .await?;
                }
                "oauth_password" => {
                    info!("Vision: 2FA continuation — filling password field");
                    self.vision_fill_and_submit(&page, &result_analysis, &password)
                        .await?;
                }
                "passkey_challenge" => {
                    info!("Vision: 2FA continuation — bypassing passkey challenge");
                    self.handle_passkey_challenge(&page, &self.config).await;
                }
                "two_factor_selection" | "phone_approval" | "number_match" => {
                    if let Some(result) = Self::handle_2fa_page(&result_analysis) {
                        // Chained chooser: re-park with ITS options so the next
                        // selection resolves against the analysis the caller saw.
                        let two_factor_options = result_analysis.two_factor_options.clone();
                        self.store_pending_login(ParkedVisionLogin {
                            browser,
                            page,
                            email,
                            password,
                            two_factor_options,
                        })
                        .await;
                        return Ok(result);
                    }
                    time::sleep(Duration::from_millis(self.config.login_poll_interval_ms)).await;
                }
                "otp_entry" => {
                    let two_factor_options = result_analysis.two_factor_options.clone();
                    self.store_pending_login(ParkedVisionLogin {
                        browser,
                        page,
                        email,
                        password,
                        two_factor_options,
                    })
                    .await;
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

        let browser = browser_utils::launch_browser(config, false, None).await?;
        let page =
            browser_utils::open_page_with_stealth(&browser, &self.provider.provider.login_url)
                .await?;

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
                        self.store_pending_login(ParkedVisionLogin {
                            browser,
                            page,
                            email: email.to_owned(),
                            password: password.to_owned(),
                            two_factor_options: analysis.two_factor_options.clone(),
                        })
                        .await;
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
        let ParkedVisionLogin {
            browser,
            page,
            email,
            password,
            ..
        } = self
            .take_pending_login()
            .await
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
                let two_factor_options = result_analysis.two_factor_options.clone();
                self.store_pending_login(ParkedVisionLogin {
                    browser,
                    page,
                    email,
                    password,
                    two_factor_options,
                })
                .await;
                Ok(LoginResult::OtpRequired)
            }
        }
    }

    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult> {
        let parked = self
            .take_pending_login()
            .await
            .ok_or_else(|| ScraperError::Auth {
                reason: "No pending 2FA session".to_owned(),
            })?;

        // Resolve the echoed id against the SAME analysis that presented the
        // chooser: the LLM assigns non-deterministic ids per analysis, so a
        // fresh re-analysis can rename every option ("phone_tap" → "tap_yes")
        // and strand the caller. The stored coordinates stay valid — the
        // parked page hasn't changed since the choice was offered. A fresh
        // analysis is the fallback for flows parked before options existed.
        let clicked =
            if let Some(opt) = parked.two_factor_options.iter().find(|o| o.id == option_id) {
                browser_utils::cdp_click_at(&parked.page, opt.x, opt.y).await?;
                true
            } else {
                let analysis = self.analyze_page(&parked.page).await?;
                match analysis
                    .two_factor_options
                    .iter()
                    .find(|o| o.id == option_id)
                {
                    Some(opt) => {
                        browser_utils::cdp_click_at(&parked.page, opt.x, opt.y).await?;
                        true
                    }
                    None => false,
                }
            };
        if !clicked {
            let known: Vec<&str> = parked
                .two_factor_options
                .iter()
                .map(|o| o.id.as_str())
                .collect();
            let reason = format!("2FA option '{option_id}' not found (offered: {known:?})");
            self.store_pending_login(parked).await;
            return Err(ScraperError::Auth { reason });
        }

        let timeout = if option_id == "app" {
            self.config.phone_tap_timeout_secs
        } else {
            self.config.password_step_timeout_secs
        };

        time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;
        self.await_two_factor_outcome(parked, timeout).await
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

        let page =
            browser_utils::open_page_with_stealth(&browser, &self.provider.provider.login_url)
                .await?;

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

        let page =
            browser_utils::open_page_with_stealth(&browser, &self.provider.provider.login_url)
                .await?;

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
        let page =
            browser_utils::open_page_with_stealth(&browser, &self.provider.provider.login_url)
                .await?;

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
#[derive(Debug, Clone, serde::Deserialize)]
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
        splits: None,
        laps: None,
        route: None,
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
        splits: None,
        laps: None,
        route: None,
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

/// Resolve a vision prompt path to its content.
///
/// Resolution order: the override directory (`DRAVR_SCIOTTE_SCRIPTS_DIR`, the
/// same hot-swappable mount as [`crate::script_loader`]), then the compiled-in
/// prompts, then the raw path (absolute paths and runtime-loaded provider
/// configs). The compiled-in prompts keep deployed binaries working when the
/// working directory does not contain the repository's `providers/` tree.
fn resolve_prompt(path: &str, override_dir: Option<&Path>) -> ScraperResult<String> {
    if let Some(dir) = override_dir {
        let full = dir.join(path);
        if let Ok(content) = fs::read_to_string(&full) {
            debug!(path, dir = %dir.display(), "Loaded vision prompt from override directory");
            return Ok(content);
        }
    }
    if let Some(content) = embedded_prompt(path) {
        return Ok(content.to_owned());
    }
    fs::read_to_string(path).map_err(|e| ScraperError::Config {
        reason: format!("Failed to read vision prompt '{path}': {e}"),
    })
}

/// Compiled-in vision prompts for the built-in providers, keyed by the path
/// strings the embedded provider TOMLs use.
fn embedded_prompt(path: &str) -> Option<&'static str> {
    match path {
        "providers/strava/page_analysis.md" => {
            Some(include_str!("../providers/strava/page_analysis.md"))
        }
        "providers/strava/list_page.md" => Some(include_str!("../providers/strava/list_page.md")),
        "providers/strava/detail_page.md" => {
            Some(include_str!("../providers/strava/detail_page.md"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::process;

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

    #[test]
    fn embedded_prompts_cover_builtin_provider_configs() {
        let configs = [
            ProviderConfig::strava_default().unwrap(), // Safe: test asserts fixture invariant
            ProviderConfig::garmin_default().unwrap(), // Safe: test asserts fixture invariant
        ];
        for config in &configs {
            let paths = [
                config.provider.vision_page_analysis_prompt.as_deref(),
                config.list_page.vision_prompt.as_deref(),
                config.detail_page.vision_prompt.as_deref(),
            ];
            for path in paths.into_iter().flatten() {
                assert!(
                    embedded_prompt(path).is_some_and(|p| !p.trim().is_empty()),
                    "configured vision prompt '{path}' has no embedded default — deployed binaries would fail to load it"
                );
            }
        }
    }

    #[test]
    fn embedded_prompts_match_repo_files() {
        for path in [
            "providers/strava/page_analysis.md",
            "providers/strava/list_page.md",
            "providers/strava/detail_page.md",
        ] {
            let embedded = embedded_prompt(path).unwrap(); // Safe: test asserts embedded key exists
            let on_disk = fs::read_to_string(path).unwrap(); // Safe: repo file exists at test cwd
            assert_eq!(
                embedded, on_disk,
                "embedded prompt for '{path}' maps to the wrong file"
            );
        }
    }

    #[test]
    fn resolve_prompt_uses_embedded_default_without_files() {
        // Simulates a deployed binary: override dir absent on disk, no
        // providers/ tree consulted — the compiled-in prompt must resolve.
        let prompt = resolve_prompt(
            "providers/strava/page_analysis.md",
            Some(Path::new("/nonexistent-sciotte-override")),
        )
        .unwrap(); // Safe: test asserts embedded fallback
        assert!(!prompt.trim().is_empty());
    }

    #[test]
    fn resolve_prompt_override_dir_wins() {
        let dir = env::temp_dir().join(format!("sciotte-prompt-override-{}", process::id()));
        let sub = dir.join("providers/strava");
        fs::create_dir_all(&sub).unwrap(); // Safe: test-owned temp dir
        fs::write(sub.join("page_analysis.md"), "OVERRIDDEN PROMPT").unwrap(); // Safe: test-owned temp dir
        let prompt = resolve_prompt("providers/strava/page_analysis.md", Some(&dir));
        fs::remove_dir_all(&dir).unwrap(); // Safe: test-owned temp dir
        assert_eq!(prompt.unwrap(), "OVERRIDDEN PROMPT"); // Safe: test asserts override read
    }

    #[test]
    fn resolve_prompt_unknown_missing_path_errors() {
        let err = resolve_prompt("providers/nope/missing.md", None).unwrap_err(); // Safe: test asserts error path
        assert!(matches!(err, ScraperError::Config { .. }));
    }
}

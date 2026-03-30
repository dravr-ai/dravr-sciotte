// ABOUTME: Chromiumoxide-based sport activity scraper driven by TOML provider configs
// ABOUTME: Implements ActivityScraper trait using headless Chrome via CDP with configurable selectors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chrono::{NaiveDateTime, Utc};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::browser_utils::{
    capture_session, cdp_click_at, click_element, dismiss_cookie_dialog, element_exists,
    fill_input_field, inject_cookies, launch_browser, read_visible_text,
};
#[cfg(feature = "vision")]
use crate::config::LoginMode;
use crate::config::ScraperConfig;
use crate::error::{LoginResult, ScraperError, ScraperResult};
use crate::models::{
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams, SportType,
};
use crate::provider::ProviderConfig;
use crate::types::ActivityScraper;

// Login timing constants are now in ScraperConfig (env-configurable).

/// URL patterns that indicate an OTP/2FA page
/// URL patterns that indicate an OTP/2FA code entry page.
/// Excludes /challenge/pk (passkey — user approves via Touch ID, no code needed).
const OTP_URL_PATTERNS: &[&str] = &[
    "challenge/totp",
    "challenge/sms",
    "challenge/ipp",
    "verify",
    "2fa",
    "mfa",
    "otp",
];

/// URL pattern for Google passkey challenge — not an OTP, handled by clicking "Try another way"
const PASSKEY_CHALLENGE_PATTERN: &str = "challenge/pk";

/// Selectors for the "Try another way" link on Google's passkey challenge page
const TRY_ANOTHER_WAY_SELECTOR: &str = "text:Try another way, text:Essayer autrement";

/// URL pattern for Google challenge pages
const CHALLENGE_URL_PATTERN: &str = "/challenge/";

/// Challenge URL suffixes that are NOT 2FA selection pages (skip these for option parsing)
const CHALLENGE_SKIP_PATTERNS: &[&str] = &["challenge/pk", "challenge/pwd", "challenge/dp"];

/// URL pattern for Google device prompt (phone tap without selection)
const DEVICE_PROMPT_PATTERN: &str = "challenge/dp";

// JS scripts moved to scripts/js/ — loaded at runtime via script_loader

/// Google's OTP submit button selectors (used as fallback in `submit_otp`)
const GOOGLE_OTP_SUBMIT_SELECTOR: &str = "#totpNext button, #totpNext, text:Next";

/// OAuth form selectors for third-party login pages (Google, Apple).
/// These are universal — the same regardless of which provider uses them.
struct OAuthFormSelectors {
    email: &'static str,
    email_next: &'static str,
    password: &'static str,
    password_next: &'static str,
}

const GOOGLE_OAUTH_SELECTORS: OAuthFormSelectors = OAuthFormSelectors {
    email: r#"input[type="email"]"#,
    email_next: r"#identifierNext button, #identifierNext",
    password: r#"input[type="password"], input[name="Passwd"]"#,
    password_next: r"#passwordNext button, #passwordNext, text:Next",
};

const APPLE_OAUTH_SELECTORS: OAuthFormSelectors = OAuthFormSelectors {
    email: r#"#account_name_text_field, input[type="text"]"#,
    email_next: r#"#sign-in, button[type="submit"]"#,
    password: r#"#password_text_field, input[type="password"]"#,
    password_next: r#"#sign-in, button[type="submit"]"#,
};

/// Chrome-based sport activity scraper driven by a TOML provider configuration.
///
/// The provider config defines login URLs, CSS selectors, and JS extraction scripts
/// so the same engine can scrape different sport platforms.
///
/// Set `DRAVR_SCIOTTE_LOGIN_MODE=vision` to use LLM-powered page analysis for login
/// (requires the `vision` feature and an embacle `LlmProvider`).
pub struct ChromeScraper {
    config: ScraperConfig,
    provider: ProviderConfig,
    /// Shared browser instance for headless scraping (lazily created)
    browser: Mutex<Option<Arc<Browser>>>,
    /// Browser + page kept alive during OTP/2FA flow for follow-up calls.
    /// Stores both so Chrome isn't killed when `credential_login` returns.
    pending_login: Mutex<Option<(Browser, chromiumoxide::Page)>>,
    /// Optional LLM provider for vision-based login (requires `vision` feature)
    #[cfg(feature = "vision")]
    llm: Option<Arc<dyn embacle::types::LlmProvider>>,
    /// Persistent vision scraper instance for multi-step login flows (OTP/2FA follow-up)
    #[cfg(feature = "vision")]
    vision_scraper: Mutex<Option<crate::vision::VisionScraper>>,
}

impl ChromeScraper {
    /// Create a scraper with explicit provider and browser config
    #[must_use]
    pub fn new(config: ScraperConfig, provider: ProviderConfig) -> Self {
        Self {
            config,
            provider,
            browser: Mutex::new(None),
            pending_login: Mutex::new(None),
            #[cfg(feature = "vision")]
            llm: None,
            #[cfg(feature = "vision")]
            vision_scraper: Mutex::new(None),
        }
    }

    /// Set the LLM provider for vision-based login (requires `vision` feature)
    #[cfg(feature = "vision")]
    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn embacle::types::LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Create with default browser config and the built-in Strava provider
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(ScraperConfig::default(), ProviderConfig::strava_default())
    }

    /// Get a reference to the provider configuration
    pub const fn provider(&self) -> &ProviderConfig {
        &self.provider
    }

    /// Get or create a headless browser instance for scraping
    async fn get_headless_browser(&self) -> ScraperResult<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        if let Some(browser) = guard.as_ref() {
            return Ok(Arc::clone(browser));
        }

        let browser = launch_browser(&self.config, true).await?;
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));

        info!("Headless browser launched for scraping");
        Ok(browser)
    }

    /// Open a new page with session cookies and navigate to the given URL
    async fn open_authenticated_page(
        &self,
        session: &AuthSession,
        url: &str,
    ) -> ScraperResult<chromiumoxide::Page> {
        let browser = self.get_headless_browser().await?;

        // Each call creates a new page (tab) with isolated cookies via CDP Network.setCookie.
        // Navigate to the provider's login page first so cookies are set on the right domain
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms)).await;

        inject_cookies(&page, session).await?;

        // Navigate to the actual target URL with cookies set
        page.goto(url).await.map_err(|e| ScraperError::Browser {
            reason: format!("Failed to navigate to {url}: {e}"),
        })?;

        tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms * 2)).await;
        Ok(page)
    }

    /// Vision-based credential login using LLM screenshot analysis.
    /// Delegates to the vision login loop which handles any page layout.
    #[cfg(feature = "vision")]
    async fn run_vision_credential_login(
        &self,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult> {
        let llm = self.llm.as_ref().ok_or_else(|| ScraperError::Config {
            reason: "Vision login mode requires an LLM provider — call ChromeScraper::with_llm()"
                .to_owned(),
        })?;

        let vision = crate::vision::VisionScraper::new(
            self.config.clone(),
            self.provider.clone(),
            Arc::clone(llm),
        );

        let result = vision.credential_login(email, password, method).await?;

        // Keep the vision scraper alive for OTP/2FA follow-up calls
        if matches!(
            result,
            LoginResult::OtpRequired
                | LoginResult::TwoFactorChoice(_)
                | LoginResult::NumberMatch(_)
                | LoginResult::Failed(_)
        ) {
            *self.vision_scraper.lock().await = Some(vision);
        }

        Ok(result)
    }

    /// Direct credential login — fill the provider's native email/password form
    async fn run_direct_credential_login(
        &self,
        page: &chromiumoxide::Page,
        email: &str,
        password: &str,
    ) -> ScraperResult<LoginResult> {
        let config = &self.config;
        let selectors = LoginSelectors::from_provider(&self.provider)?;

        debug!(selector = selectors.email, "Filling email field");
        fill_input_field(page, selectors.email, email).await?;
        tokio::time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;

        let password_visible = element_exists(page, selectors.password).await;
        debug!(password_visible, "Password field check after page load");

        if !password_visible {
            debug!("Submitting email, waiting for password field to appear");
            click_element(page, selectors.button).await?;
            let step = poll_for_next_step(
                page,
                &self.provider,
                config,
                selectors.password,
                config.email_step_timeout_secs,
            )
            .await?;
            if let StepOutcome::LoginResult(result) = step {
                debug!("Login resolved during email step");
                return Ok(result);
            }
            debug!("Password field appeared after email submit");
        }

        debug!(selector = selectors.password, "Filling password field");
        fill_input_field(page, selectors.password, password).await?;
        tokio::time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
        debug!("Clicking submit after password");
        click_element(page, selectors.button).await?;

        poll_credential_login_result(
            page,
            &self.provider,
            config,
            config.password_step_timeout_secs,
            Some(password),
        )
        .await
    }

    /// OAuth credential login — click provider OAuth button, then fill Google/Apple form
    async fn run_oauth_credential_login(
        &self,
        page: &chromiumoxide::Page,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult> {
        let config = &self.config;
        let oauth_button_selector = self
            .provider
            .provider
            .login_oauth_buttons
            .get(method)
            .ok_or_else(|| ScraperError::Config {
                reason: format!("No OAuth button selector configured for method: {method}"),
            })?;

        let oauth_form = match method {
            "google" => &GOOGLE_OAUTH_SELECTORS,
            "apple" => &APPLE_OAUTH_SELECTORS,
            other => {
                return Err(ScraperError::Config {
                    reason: format!("Unsupported OAuth method: {other}"),
                });
            }
        };

        // Click the OAuth button on the provider's login page
        debug!(method, "Clicking OAuth button on provider page");
        click_element(page, oauth_button_selector).await?;
        tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Fill email on the OAuth provider's page
        debug!(selector = oauth_form.email, "Filling OAuth email field");
        fill_input_field(page, oauth_form.email, email).await?;
        tokio::time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
        debug!("Clicking Next after OAuth email");
        click_element(page, oauth_form.email_next).await?;

        // Wait for the page transition — the password field may exist as a hidden element
        // on the email step, so we must wait for Google to actually transition pages
        debug!(
            wait_secs = config.page_load_wait_secs,
            "Waiting for OAuth page transition"
        );
        tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Now wait for a visible password field
        debug!("Waiting for OAuth password field to become visible");
        let step = poll_for_next_step(
            page,
            &self.provider,
            config,
            oauth_form.password,
            config.email_step_timeout_secs,
        )
        .await?;
        if let StepOutcome::LoginResult(result) = step {
            debug!("Login resolved during OAuth email step");
            return Ok(result);
        }

        // Fill password on the OAuth provider's page
        save_timeout_screenshot(page, "before-password-fill").await;
        debug!(
            selector = oauth_form.password,
            "Filling OAuth password field"
        );
        fill_input_field(page, oauth_form.password, password).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        debug!("Clicking Next after OAuth password");
        click_element(page, oauth_form.password_next).await?;
        save_timeout_screenshot(page, "after-password-submit").await;

        // Poll for final result — Google/Apple will redirect back to the provider
        poll_credential_login_result(
            page,
            &self.provider,
            config,
            config.password_step_timeout_secs,
            Some(password),
        )
        .await
    }
}

#[async_trait]
impl ActivityScraper for ChromeScraper {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        info!(
            provider = %self.provider.provider.name,
            "Launching visible browser for login"
        );

        let browser = launch_browser(&self.config, false).await?;
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open login page: {e}"),
            })?;

        info!("Waiting for user to log in...");
        wait_for_login(&page, &self.provider, &self.config).await?;

        let session = capture_session(&page).await?;

        info!(
            cookie_count = session.cookies.len(),
            "Login successful, session captured"
        );
        Ok(session)
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
            login_mode = ?config.login_mode,
            "Starting credential login"
        );

        // Vision mode: delegate to the vision login loop
        #[cfg(feature = "vision")]
        if matches!(config.login_mode, LoginMode::Vision) {
            return self
                .run_vision_credential_login(email, password, method)
                .await;
        }

        // Fake login mode: serve embedded HTML fixtures instead of real provider
        let login_url = if config.fake_login {
            let base = crate::fake_login::start_fake_server().await.map_err(|e| {
                ScraperError::Browser {
                    reason: format!("Failed to start fake login server: {e}"),
                }
            })?;
            info!(base_url = %base, "Using fake login server");
            format!("{base}/strava/login.html")
        } else {
            self.provider.provider.login_url.clone()
        };

        let browser = launch_browser(
            config,
            config.fake_login || config.credential_login_headless,
        )
        .await?;
        let page = browser
            .new_page(&login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open login page: {e}"),
            })?;

        debug!(
            wait_secs = config.page_load_wait_secs,
            "Waiting for page JS to render"
        );
        tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
        dismiss_cookie_dialog(&page).await;

        let result = match method {
            "google" | "apple" => {
                self.run_oauth_credential_login(&page, email, password, method)
                    .await
            }
            _ => {
                self.run_direct_credential_login(&page, email, password)
                    .await
            }
        };

        // Hybrid mode: on failure, retry with vision
        #[cfg(feature = "vision")]
        if matches!(config.login_mode, LoginMode::Hybrid) {
            if let Err(ref e) = result {
                warn!(error = %e, "Selector login failed, retrying with vision mode");
                return self
                    .run_vision_credential_login(email, password, method)
                    .await;
            }
        }

        let result = result?;

        if matches!(
            result,
            LoginResult::OtpRequired
                | LoginResult::TwoFactorChoice(_)
                | LoginResult::NumberMatch(_)
        ) {
            *self.pending_login.lock().await = Some((browser, page));
        }

        Ok(result)
    }

    async fn submit_otp(&self, code: &str) -> ScraperResult<LoginResult> {
        // Delegate to vision scraper if it's active
        #[cfg(feature = "vision")]
        if let Some(ref vision) = *self.vision_scraper.lock().await {
            return vision.submit_otp(code).await;
        }

        let (browser, page) =
            self.pending_login
                .lock()
                .await
                .take()
                .ok_or_else(|| ScraperError::Auth {
                    reason: "No pending OTP session — call credential_login first".to_owned(),
                })?;

        let otp_selector = self
            .provider
            .provider
            .login_otp_selector
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no login_otp_selector configured".to_owned(),
            })?;
        let button_selector = self
            .provider
            .provider
            .login_button_selector
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no login_button_selector configured".to_owned(),
            })?;

        let config = &self.config;
        // Combine provider's button selector with Google's OTP button as fallback
        let combined_button = format!("{button_selector}, {GOOGLE_OTP_SUBMIT_SELECTOR}");
        info!("Submitting OTP code");
        // Try provider OTP selector first, fall back to any visible text input
        let fill_result = fill_input_field(&page, otp_selector, code).await;
        if fill_result.is_err() {
            warn!("OTP selector failed, trying fallback input detection");
            // Dump visible inputs for debugging
            let debug_js = r"(function() {
                var inputs = document.querySelectorAll('input');
                return JSON.stringify(Array.from(inputs).map(function(i) {
                    var r = i.getBoundingClientRect();
                    return {type: i.type, name: i.name, id: i.id, visible: r.width > 0 && r.height > 0, w: r.width, h: r.height};
                }));
            })()";
            if let Ok(result) = page.evaluate(debug_js).await {
                let val = result
                    .value()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default();
                warn!(inputs = %val, "Available input fields on OTP page");
            }
            // Try any visible text/number/tel input as fallback
            let fallback = r#"input[type="text"], input[type="number"], input[type="tel"], input:not([type="hidden"]):not([type="password"])"#;
            fill_input_field(&page, fallback, code).await?;
        }
        tokio::time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
        click_element(&page, &combined_button).await?;

        // Wait for Google to process the code and redirect away from the TOTP page
        tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        let result = poll_credential_login_result(
            &page,
            &self.provider,
            config,
            config.password_step_timeout_secs,
            None,
        )
        .await?;

        // Keep the browser + page alive for retry on failure or further interaction
        if matches!(
            result,
            LoginResult::OtpRequired
                | LoginResult::TwoFactorChoice(_)
                | LoginResult::NumberMatch(_)
                | LoginResult::Failed(_)
        ) {
            *self.pending_login.lock().await = Some((browser, page));
        }

        Ok(result)
    }

    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult> {
        // Delegate to vision scraper if it's active
        #[cfg(feature = "vision")]
        if let Some(ref vision) = *self.vision_scraper.lock().await {
            return vision.select_two_factor(option_id).await;
        }

        let (browser, page) =
            self.pending_login
                .lock()
                .await
                .take()
                .ok_or_else(|| ScraperError::Auth {
                    reason: "No pending 2FA session — call credential_login first".to_owned(),
                })?;

        let config = &self.config;

        // "poll" = just wait for success (used after NumberMatch — user taps on phone)
        if option_id == "poll" {
            info!("Polling for 2FA approval (phone tap)");
            let result = poll_credential_login_result(
                &page,
                &self.provider,
                config,
                config.phone_tap_timeout_secs,
                None,
            )
            .await?;
            if !matches!(result, LoginResult::Success(_)) {
                *self.pending_login.lock().await = Some((browser, page));
            }
            return Ok(result);
        }

        info!(option_id, "Selecting 2FA method");
        if !cdp_click_two_fa_option(&page, option_id).await {
            *self.pending_login.lock().await = Some((browser, page));
            return Err(ScraperError::Auth {
                reason: format!("2FA option '{option_id}' not found on page"),
            });
        }

        tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Check if we're already on an OTP code entry page
        let current_url = page.url().await.ok().flatten().unwrap_or_default();
        if path_contains_any(&current_url, OTP_URL_PATTERNS) {
            info!(url = %current_url, "Already on OTP page after selecting 2FA method");
            *self.pending_login.lock().await = Some((browser, page));
            return Ok(LoginResult::OtpRequired);
        }

        // For "app" (phone tap): check if a number matching challenge appeared
        if option_id == "app" {
            if let Some(number) = extract_number_from_page(&page).await {
                info!(number = %number, "Number matching challenge detected");
                *self.pending_login.lock().await = Some((browser, page));
                return Ok(LoginResult::NumberMatch(number));
            }
            // Debug: dump what's visible on the page so we can improve extraction
            let current_url = page.url().await.ok().flatten().unwrap_or_default();
            warn!(url = %current_url, "Number extraction returned None — dumping page debug info");
            if let Ok(screenshot) = page
                .save_screenshot(
                    chromiumoxide::page::ScreenshotParams::builder()
                        .full_page(true)
                        .build(),
                    "/tmp/sciotte-number-match-debug.png",
                )
                .await
            {
                warn!(
                    "Debug screenshot saved to /tmp/sciotte-number-match-debug.png ({} bytes)",
                    screenshot.len()
                );
            }
            // Dump all visible 2-3 digit numbers and their font sizes
            let debug_js = r"(function() {
                var results = [];
                var all = document.querySelectorAll('div, span, p');
                for (var i = 0; i < all.length; i++) {
                    var el = all[i];
                    var text = el.textContent.trim();
                    if (!/^\d{2,3}$/.test(text)) continue;
                    var style = window.getComputedStyle(el);
                    var fontSize = parseFloat(style.fontSize) || 0;
                    var rect = el.getBoundingClientRect();
                    results.push({text: text, fontSize: fontSize, tag: el.tagName, w: rect.width, h: rect.height, visible: rect.width > 0 && rect.height > 0});
                }
                return JSON.stringify(results);
            })()";
            if let Ok(result) = page.evaluate(debug_js).await {
                if let Some(v) = result.value().and_then(|v| v.as_str()) {
                    warn!(candidates = %v, "All 2-3 digit numbers found on page");
                }
            }
        }

        // Phone tap needs longer — user must pick up their phone
        let timeout = if option_id == "app" {
            config.phone_tap_timeout_secs
        } else {
            config.password_step_timeout_secs
        };
        let result =
            poll_credential_login_result(&page, &self.provider, config, timeout, None).await?;

        if !matches!(result, LoginResult::Success(_)) {
            *self.pending_login.lock().await = Some((browser, page));
        }

        Ok(result)
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
        let page = self
            .open_authenticated_page(session, &self.provider.list_page.url)
            .await?;

        check_session_redirect(&page, &self.provider).await?;

        let target_count = params.limit.unwrap_or(20) as usize;
        let js = self.provider.list_extraction_js();

        // Paginate the training page: it shows ~20 activities per page with a "next" button.
        // We extract each page and click next until we have enough activities.
        let mut all_items: Vec<serde_json::Value> = Vec::new();

        loop {
            match extract_via_js(&page, &js).await {
                Ok(items) => {
                    debug!(count = items.len(), "Activities found on current page");
                    all_items.extend(items);
                }
                Err(e) => {
                    warn!(error = %e, "List page JS extraction failed");
                    break;
                }
            }

            if all_items.len() >= target_count {
                break;
            }

            // Click the "next page" button if it exists
            let has_next = page
                .evaluate(
                    r#"(function() {
                        var btn = document.querySelector("button.next_page");
                        if (btn && !btn.disabled) { btn.click(); return true; }
                        return false;
                    })()"#,
                )
                .await
                .ok()
                .and_then(|r| r.value().and_then(serde_json::Value::as_bool))
                .unwrap_or(false);

            if !has_next {
                debug!("No more pages available");
                break;
            }

            info!(
                collected = all_items.len(),
                target = target_count,
                "Loading next page of activities"
            );
            tokio::time::sleep(Duration::from_millis(self.config.interaction_delay_ms * 3)).await;
        }

        // Truncate to target and deduplicate by ID
        deduplicate_by_id(&mut all_items);
        let mut activities = parse_js_activity_items(&all_items);
        apply_activity_filters(&mut activities, params);

        info!(
            count = activities.len(),
            "Activities extracted from list page"
        );

        // Optionally enrich each activity by navigating to its detail page
        if params.enrich_details {
            info!(
                count = activities.len(),
                "Enriching activities from detail pages (this may take a while)"
            );
            let total = activities.len();
            for (i, activity) in activities.iter_mut().enumerate() {
                info!(
                    progress = format!("{}/{}", i + 1, total),
                    id = %activity.id,
                    "Fetching detail page"
                );
                let detail_url = self.provider.detail_url(&activity.id);
                match navigate_and_extract_detail(&page, &detail_url, &self.provider, &self.config)
                    .await
                {
                    Ok(detail) => merge_detail_into_activity(activity, &detail),
                    Err(e) => {
                        warn!(id = %activity.id, error = %e, "Failed to enrich activity");
                    }
                }
            }
        }

        info!(count = activities.len(), "Activities scraped");
        Ok(activities)
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let url = self.provider.detail_url(activity_id);
        info!(url = %url, "Navigating to activity detail page");

        let page = self.open_authenticated_page(session, &url).await?;
        let data = extract_detail_via_js(&page, &self.provider).await?;
        let activity = build_activity_from_detail(activity_id, &data);

        info!(id = activity_id, name = %activity.name, "Activity detail scraped");
        Ok(activity)
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
        let js = self
            .provider
            .provider
            .profile_js_extract
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no profile_js_extract configured".to_owned(),
            })?;

        let page = self.open_authenticated_page(session, profile_url).await?;

        let result = page
            .evaluate(js)
            .await
            .map_err(|e| ScraperError::Scraping {
                reason: format!("Profile JS extraction failed: {e}"),
            })?;

        let json_str = result
            .value()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();

        let profile: AthleteProfile =
            serde_json::from_str(&json_str).map_err(|e| ScraperError::Scraping {
                reason: format!("Failed to parse profile data: {e}"),
            })?;

        info!(
            name = profile.display_name.as_deref().unwrap_or("unknown"),
            "Athlete profile scraped"
        );
        Ok(profile)
    }

    async fn get_daily_summary(
        &self,
        session: &AuthSession,
        params: &HealthParams,
    ) -> ScraperResult<DailySummary> {
        if self.provider.health_pages.is_empty() {
            return Err(ScraperError::Config {
                reason: format!(
                    "Provider '{}' has no health_pages configured",
                    self.provider.provider.name
                ),
            });
        }

        let pages = self.provider.health_urls(&params.date);
        let mut summary = empty_daily_summary(params.date, &self.provider.provider.name);

        for (page_name, url) in &pages {
            info!(url = %url, page = %page_name, date = %params.date, "Navigating to health page");
            let page = self.open_authenticated_page(session, url).await?;
            check_session_redirect(&page, &self.provider).await?;

            // Health dashboards are React SPAs — wait for async rendering
            tokio::time::sleep(Duration::from_secs(self.config.page_load_wait_secs * 2)).await;

            let js = &self.provider.health_pages[*page_name].js_extract;
            match extract_health_json(&page, js).await {
                Ok(raw) => {
                    debug!(page = %page_name, raw_json = %raw, "Health page extraction result");
                    let parsed: serde_json::Value = serde_json::from_str(&raw)
                        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
                    merge_health_data(&mut summary, &parsed);
                }
                Err(e) => {
                    warn!(page = %page_name, error = %e, "Health page extraction failed, skipping");
                }
            }
        }

        info!(date = %params.date, pages = pages.len(), "Daily summary scraped");
        Ok(summary)
    }
}

// ============================================================================
// Login flow types and helpers
// ============================================================================

/// Check URL patterns against the path only (strip query params)
/// Try to extract a 2-3 digit number from the page (Google number matching challenge).
/// Looks for a prominent number displayed on screen.
async fn extract_number_from_page(page: &chromiumoxide::Page) -> Option<String> {
    let js = crate::script_loader::loader()
        .load("extract_number.js")
        .await;
    let result = page.evaluate(js).await.ok()?;
    let json_str = result.value().and_then(|v| v.as_str().map(String::from))?;
    let parsed: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    let dbg_info = parsed["debug"].as_str().unwrap_or("");
    info!(candidates = %dbg_info, "Number extraction candidates");

    parsed["number"].as_str().map(String::from)
}

fn url_path_matches(url: &str, patterns: &[String]) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    patterns.iter().any(|p| path.contains(p.as_str()))
}

/// Check if the URL path (excluding query params) contains any of the given patterns.
/// Prevents false positives from base64 tokens in query strings matching short patterns like "2fa".
fn path_contains_any(url: &str, patterns: &[&str]) -> bool {
    let path = url.split('?').next().unwrap_or(url);
    patterns.iter().any(|p| path.contains(p))
}

/// Extracted login selectors from provider config, validated upfront
#[derive(Debug)]
struct LoginSelectors<'a> {
    email: &'a str,
    password: &'a str,
    button: &'a str,
}

impl<'a> LoginSelectors<'a> {
    fn from_provider(provider: &'a ProviderConfig) -> ScraperResult<Self> {
        let email = provider
            .provider
            .login_email_selector
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no login_email_selector configured".to_owned(),
            })?;
        let password = provider
            .provider
            .login_password_selector
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no login_password_selector configured".to_owned(),
            })?;
        let button = provider
            .provider
            .login_button_selector
            .as_deref()
            .ok_or_else(|| ScraperError::Config {
                reason: "Provider has no login_button_selector configured".to_owned(),
            })?;
        Ok(Self {
            email,
            password,
            button,
        })
    }
}

/// Outcome of waiting for the next login step
enum StepOutcome {
    /// The expected field appeared in the DOM
    FieldAppeared,
    /// Login resolved early (success, OTP, or failure)
    LoginResult(LoginResult),
}

/// Poll until a target field appears OR a login result is detected (success/OTP/error)
async fn poll_for_next_step(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
    config: &ScraperConfig,
    field_selector: &str,
    timeout_secs: u64,
) -> ScraperResult<StepOutcome> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if tokio::time::Instant::now() > deadline {
            save_timeout_screenshot(page, "step-timeout").await;
            return Err(ScraperError::Auth {
                reason: format!(
                    "Login step timed out after {timeout_secs}s waiting for next field"
                ),
            });
        }

        // Check if the target field appeared
        if element_exists(page, field_selector).await {
            return Ok(StepOutcome::FieldAppeared);
        }

        let url = page
            .url()
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to get page URL: {e}"),
            })?
            .unwrap_or_default();

        // Passkey challenge — click "Try another way" then "Enter your password"
        if url.contains(PASSKEY_CHALLENGE_PATTERN) {
            info!("Passkey challenge detected, clicking 'Try another way'");
            let _ = click_element(page, TRY_ANOTHER_WAY_SELECTOR).await;
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            info!("Clicking 'Enter your password' via CDP");
            cdp_click_enter_password(page).await;
            // Double-click — Google sometimes needs a second click on the challenge option
            tokio::time::sleep(Duration::from_secs(1)).await;
            cdp_click_enter_password(page).await;
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            continue;
        }

        // Device prompt (/challenge/dp) — notification already sent to phone.
        // Don't click "Try another way" — it kills the existing notification.
        // Return to caller so user can approve on their phone.
        if url.contains(DEVICE_PROMPT_PATTERN) {
            info!("Device prompt detected — user has phone notification");
            return Ok(StepOutcome::LoginResult(LoginResult::NumberMatch(
                "Check your phone".to_owned(),
            )));
        }

        // Check for OTP/2FA code entry pages (challenge/totp, challenge/sms, etc.)
        if path_contains_any(&url, OTP_URL_PATTERNS) {
            info!(url = %url, "OTP/2FA page detected during step transition");
            return Ok(StepOutcome::LoginResult(LoginResult::OtpRequired));
        }

        // Challenge selection page — could be sign-in method chooser (pre-password)
        // or 2FA options (post-password). If "Enter your password" is an option,
        // auto-click it instead of returning it as a 2FA choice.
        if url.contains(CHALLENGE_URL_PATTERN)
            && !CHALLENGE_SKIP_PATTERNS.iter().any(|p| url.contains(p))
        {
            save_timeout_screenshot(page, "challenge-step").await;
            info!(url = %url, "Challenge selection page detected during step transition");

            // Check if "Enter your password" is available — if so, click it automatically
            cdp_click_enter_password(page).await;
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            continue;
        }

        // Check for success
        if !url.is_empty() && url_path_matches(&url, &provider.provider.login_success_patterns) {
            info!(url = %url, "Login succeeded during step transition");
            let session = capture_session(page).await?;
            return Ok(StepOutcome::LoginResult(LoginResult::Success(session)));
        }

        // Check for error messages
        if let Some(ref error_selector) = provider.provider.login_error_selector {
            if let Some(error_text) = read_visible_text(page, error_selector).await {
                return Ok(StepOutcome::LoginResult(LoginResult::Failed(error_text)));
            }
        }

        tokio::time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    }
}

// ============================================================================
// 2FA helpers
// ============================================================================

/// Parsed 2FA option with coordinates for CDP click
#[derive(Debug, serde::Deserialize)]
struct TwoFactorOptionWithCoords {
    id: String,
    label: String,
    x: f64,
    y: f64,
}

/// Parse 2FA options from the current page
async fn parse_two_fa_options(page: &chromiumoxide::Page) -> Vec<TwoFactorOptionWithCoords> {
    let js = crate::script_loader::loader()
        .load("parse_2fa_options.js")
        .await;
    let Ok(result) = page.evaluate(js).await else {
        return Vec::new();
    };
    let json_str = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    if json_str.starts_with("debug:") {
        warn!(raw = %json_str, "2FA options parser returned debug info — no options matched");
        return Vec::new();
    }
    serde_json::from_str(&json_str).unwrap_or_default()
}

/// CDP-click a 2FA option by its id, using stored coordinates
async fn cdp_click_two_fa_option(page: &chromiumoxide::Page, option_id: &str) -> bool {
    let options = parse_two_fa_options(page).await;
    for opt in &options {
        if opt.id == option_id {
            debug!(id = opt.id, x = opt.x, y = opt.y, "CDP clicking 2FA option");
            let _ = cdp_click_at(page, opt.x, opt.y).await;
            return true;
        }
    }
    warn!(option_id, "2FA option not found on page");
    false
}

/// Find and CDP-click the "Enter your password" option on Google's challenge page
async fn cdp_click_enter_password(page: &chromiumoxide::Page) {
    let js = crate::script_loader::loader()
        .load("enter_password_coords.js")
        .await;
    if let Ok(result) = page.evaluate(js).await {
        let val = result
            .value()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        if val.starts_with("not_found") {
            warn!(debug = %val, "Could not find 'Enter your password' option");
        } else if let Ok(coords) = serde_json::from_str::<serde_json::Value>(&val) {
            let x = coords["x"].as_f64().unwrap_or(0.0);
            let y = coords["y"].as_f64().unwrap_or(0.0);
            debug!(x, y, "CDP clicking 'Enter your password'");
            let _ = cdp_click_at(page, x, y).await;
        }
    }
}

/// Save a debug screenshot to the temp directory, logging the path on success
async fn save_timeout_screenshot(page: &chromiumoxide::Page, label: &str) {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .build();
    if let Ok(data) = page.screenshot(params).await {
        let path = std::env::temp_dir().join(format!("sciotte-{label}.png"));
        if tokio::fs::write(&path, &data).await.is_ok() {
            warn!("Timeout screenshot saved to {}", path.display());
        }
    }
}

/// Poll for credential login result: success, OTP required, or failure with error message
async fn poll_credential_login_result(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
    config: &ScraperConfig,
    timeout_secs: u64,
    password: Option<&str>,
) -> ScraperResult<LoginResult> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    // Capture the initial URL so we can detect when the page actually changes
    let initial_url = page.url().await.ok().flatten().unwrap_or_default();
    debug!(initial_url = %initial_url, "Polling for login result");

    loop {
        if tokio::time::Instant::now() > deadline {
            save_timeout_screenshot(page, "login-timeout").await;
            return Err(ScraperError::Auth {
                reason: "Credential login timed out".to_owned(),
            });
        }

        let url = page
            .url()
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to get page URL: {e}"),
            })?
            .unwrap_or_default();

        // Check success patterns early — works even if URL hasn't changed
        if !url.is_empty() && url_path_matches(&url, &provider.provider.login_success_patterns) {
            info!(url = %url, "Credential login detected via URL");
            let session = capture_session(page).await?;
            info!(
                cookie_count = session.cookies.len(),
                "Credential login successful"
            );
            return Ok(LoginResult::Success(session));
        }

        // Check OTP patterns — only if the URL has changed to a DIFFERENT OTP page.
        // If we're still on the same OTP page we started on, keep waiting for redirect.
        if url != initial_url && path_contains_any(&url, OTP_URL_PATTERNS) {
            info!(url = %url, "OTP/2FA page detected");
            return Ok(LoginResult::OtpRequired);
        }

        // Wait for the URL to change from the initial page before checking challenge types
        if url == initial_url {
            tokio::time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
            continue;
        }

        // Passkey challenge (after password) — click "Try another way" to reach 2FA selection.
        // Don't try "Enter your password" here — password was already submitted.
        if url.contains(PASSKEY_CHALLENGE_PATTERN) {
            info!("Passkey challenge detected post-password, clicking 'Try another way'");
            let _ = click_element(page, TRY_ANOTHER_WAY_SELECTOR).await;
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            continue;
        }

        // Device prompt (/challenge/dp) — notification already sent to phone.
        // Don't click "Try another way" — return so user can approve on phone.
        if url.contains(DEVICE_PROMPT_PATTERN) {
            info!("Device prompt detected post-password — user has phone notification");
            return Ok(LoginResult::NumberMatch("Check your phone".to_owned()));
        }

        // Challenge selection page — could be 2FA options or sign-in method chooser.
        if url.contains(CHALLENGE_URL_PATTERN)
            && !CHALLENGE_SKIP_PATTERNS.iter().any(|p| url.contains(p))
        {
            info!(url = %url, "Challenge selection page detected");
            // Wait for the page DOM to render before parsing options —
            // without this, slow runners (Windows CI) may see an empty DOM
            // and misclassify a 2FA page as a sign-in method chooser.
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            let options = parse_two_fa_options(page).await;
            if !options.is_empty() {
                // Real 2FA options found — return to caller for orchestration
                let choices: Vec<crate::error::TwoFactorOption> = options
                    .into_iter()
                    .map(|o| crate::error::TwoFactorOption {
                        id: o.id,
                        label: o.label,
                    })
                    .collect();
                return Ok(LoginResult::TwoFactorChoice(choices));
            }
            // No 2FA options — this is the sign-in method chooser page.
            // Click "Enter your password", re-fill password, and submit.
            info!("No 2FA options found, clicking 'Enter your password'");
            cdp_click_enter_password(page).await;
            tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            if let Some(pwd) = password {
                let pwd_selector = r#"input[type="password"], input[name="Passwd"]"#;
                if element_exists(page, pwd_selector).await {
                    info!("Re-filling password after sign-in method selection");
                    let _ = fill_input_field(page, pwd_selector, pwd).await;
                    tokio::time::sleep(Duration::from_millis(config.form_interaction_delay_ms))
                        .await;
                    let _ =
                        click_element(page, "#passwordNext button, #passwordNext, text:Next").await;
                    tokio::time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
                }
            }
            continue;
        }

        // Check for error messages on the login page
        if let Some(ref error_selector) = provider.provider.login_error_selector {
            if let Some(error_text) = read_visible_text(page, error_selector).await {
                return Ok(LoginResult::Failed(error_text));
            }
        }

        tokio::time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    }
}

/// Poll the browser page until the user has completed login.
/// Uses the provider's configured URL patterns to detect success/failure.
async fn wait_for_login(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
    config: &ScraperConfig,
) -> ScraperResult<()> {
    let timeout = config.login_timeout_secs;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);

    loop {
        if tokio::time::Instant::now() > deadline {
            return Err(ScraperError::Auth {
                reason: format!(
                    "Login timed out after {timeout} seconds — close the browser and retry"
                ),
            });
        }

        let url = page
            .url()
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to get page URL: {e}"),
            })?
            .unwrap_or_default();

        let on_failure_page = url_path_matches(&url, &provider.provider.login_failure_patterns);
        let on_success_page = url_path_matches(&url, &provider.provider.login_success_patterns);

        if !url.is_empty() && !on_failure_page && on_success_page {
            info!(url = %url, "Login detected");
            return Ok(());
        }

        tokio::time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    }
}

/// Check if the browser was redirected to a login page (session expired)
async fn check_session_redirect(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
) -> ScraperResult<()> {
    let url = page
        .url()
        .await
        .map_err(|e| ScraperError::Browser {
            reason: format!("Failed to get URL: {e}"),
        })?
        .unwrap_or_default();

    let on_failure = url_path_matches(&url, &provider.provider.login_failure_patterns);

    if on_failure {
        return Err(ScraperError::SessionExpired {
            reason: "Redirected to login page — session cookies expired, re-login required"
                .to_owned(),
        });
    }
    Ok(())
}

// ============================================================================
// Daily summary parsing
// ============================================================================

/// Extract the first integer from a string like "49 bpm", "5,156", "75/100"
fn parse_numeric_u32(s: &str) -> Option<u32> {
    let cleaned: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == ' ')
        .collect();
    cleaned.replace([',', ' '], "").parse().ok()
}

/// Extract the first float from a string like "50", "50.5"
fn parse_numeric_f32(s: &str) -> Option<f32> {
    let cleaned: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    cleaned.replace(',', ".").parse().ok()
}

/// Create an empty `DailySummary` with only date and provider set
fn empty_daily_summary(date: chrono::NaiveDate, provider: &str) -> DailySummary {
    DailySummary {
        date,
        provider: provider.to_owned(),
        resting_heart_rate: None,
        average_resting_heart_rate_7day: None,
        max_heart_rate: None,
        body_battery: None,
        stress_level: None,
        steps: None,
        step_goal: None,
        intensity_minutes: None,
        intensity_minutes_goal: None,
        vo2_max: None,
        training_load: None,
        sleep_score: None,
        sleep_duration_seconds: None,
        sleep_deep_seconds: None,
        sleep_light_seconds: None,
        sleep_rem_seconds: None,
        sleep_awake_seconds: None,
        hrv_status: None,
        hrv_value: None,
        weight_kg: None,
        body_fat_percent: None,
        fitness_score: None,
        fatigue_score: None,
        form_score: None,
        active_calories: None,
        total_calories: None,
    }
}

/// Merge raw JSON health data into an existing `DailySummary`.
/// Only fills fields that are still `None` — earlier pages take priority.
fn merge_health_data(summary: &mut DailySummary, raw: &serde_json::Value) {
    let s = |key| raw[key].as_str().unwrap_or_default();
    let set_u32 = |field: &mut Option<u32>, key| {
        if field.is_none() {
            *field = parse_numeric_u32(s(key));
        }
    };
    let set_f32 = |field: &mut Option<f32>, key| {
        if field.is_none() {
            *field = parse_numeric_f32(s(key));
        }
    };

    // Heart rate
    set_u32(&mut summary.resting_heart_rate, "resting_hr");
    set_u32(&mut summary.average_resting_heart_rate_7day, "avg_hr_7day");
    set_u32(&mut summary.max_heart_rate, "max_hr");

    // Body metrics
    set_u32(&mut summary.body_battery, "body_battery");
    set_u32(&mut summary.stress_level, "stress");

    // Steps
    set_u32(&mut summary.steps, "steps");
    set_u32(&mut summary.step_goal, "step_goal");

    // Intensity
    set_u32(&mut summary.intensity_minutes, "intensity_minutes");
    set_u32(&mut summary.intensity_minutes_goal, "intensity_goal");

    // Training
    set_f32(&mut summary.vo2_max, "vo2_max");
    set_u32(&mut summary.training_load, "training_load");

    // Sleep
    set_u32(&mut summary.sleep_score, "sleep_score");
    if summary.sleep_duration_seconds.is_none() {
        summary.sleep_duration_seconds = s("sleep_duration").parse().ok();
    }
    if summary.sleep_deep_seconds.is_none() {
        summary.sleep_deep_seconds = parse_duration_to_seconds(s("sleep_deep"));
    }
    if summary.sleep_light_seconds.is_none() {
        summary.sleep_light_seconds = parse_duration_to_seconds(s("sleep_light"));
    }
    if summary.sleep_rem_seconds.is_none() {
        summary.sleep_rem_seconds = parse_duration_to_seconds(s("sleep_rem"));
    }
    if summary.sleep_awake_seconds.is_none() {
        summary.sleep_awake_seconds = parse_duration_to_seconds(s("sleep_awake"));
    }

    // HRV
    if summary.hrv_status.is_none() {
        let v = s("hrv_status");
        if !v.is_empty() {
            summary.hrv_status = Some(v.to_owned());
        }
    }
    set_u32(&mut summary.hrv_value, "hrv_value");

    // Body composition
    set_f32(&mut summary.weight_kg, "weight_kg");
    set_f32(&mut summary.body_fat_percent, "body_fat_percent");

    // Training load (Strava Fitness & Freshness)
    set_u32(&mut summary.fitness_score, "fitness");
    set_u32(&mut summary.fatigue_score, "fatigue");
    if summary.form_score.is_none() {
        let v = s("form");
        if !v.is_empty() {
            summary.form_score = v.parse().ok();
        }
    }

    // Calories
    set_u32(&mut summary.active_calories, "active_calories");
    set_u32(&mut summary.total_calories, "total_calories");
}

/// Parse a duration string like "1h 23min", "45 min", "2h" into seconds
fn parse_duration_to_seconds(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    if let Some(caps) = s.find('h') {
        total += s[..caps].trim().parse::<u64>().unwrap_or(0) * 3600;
    }
    // Look for minutes after 'h' or standalone
    let min_part = s.split('h').next_back().unwrap_or(s);
    if let Some(m) = min_part.find('m') {
        total += min_part[..m].trim().parse::<u64>().unwrap_or(0) * 60;
    }
    if total > 0 {
        Some(total)
    } else {
        None
    }
}

/// Execute a JS snippet on a health page and return the raw JSON string
async fn extract_health_json(page: &chromiumoxide::Page, js: &str) -> ScraperResult<String> {
    let result = page
        .evaluate(js)
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("Health JS evaluation failed: {e}"),
        })?;
    Ok(result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default())
}

// ============================================================================
// JS extraction (generic, driven by provider config)
// ============================================================================

/// Execute a JS snippet on a page and parse the returned JSON array
async fn extract_via_js(
    page: &chromiumoxide::Page,
    js: &str,
) -> ScraperResult<Vec<serde_json::Value>> {
    let result = page
        .evaluate(js)
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("JS evaluation failed: {e}"),
        })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("[]");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse JS result: {e}"),
    })
}

/// Extract detailed activity data using the provider's configured JS snippet
async fn extract_detail_via_js(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
) -> ScraperResult<serde_json::Value> {
    let result = page
        .evaluate(provider.detail_page.js_extract.as_str())
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("Failed to extract activity data: {e}"),
        })?;

    let json_str = result.value().and_then(|v| v.as_str()).unwrap_or("{}");

    serde_json::from_str(json_str).map_err(|e| ScraperError::Scraping {
        reason: format!("Failed to parse activity detail: {e}"),
    })
}

/// Navigate an existing page to an activity detail URL and extract data
async fn navigate_and_extract_detail(
    page: &chromiumoxide::Page,
    url: &str,
    provider: &ProviderConfig,
    config: &ScraperConfig,
) -> ScraperResult<serde_json::Value> {
    page.goto(url).await.map_err(|e| ScraperError::Browser {
        reason: format!("Failed to navigate to {url}: {e}"),
    })?;

    tokio::time::sleep(Duration::from_millis(config.interaction_delay_ms * 2)).await;
    extract_detail_via_js(page, provider).await
}

// ============================================================================
// Activity construction from scraped data
// ============================================================================

/// Build Activity structs from JS-extracted item list
fn parse_js_activity_items(items: &[serde_json::Value]) -> Vec<Activity> {
    items
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            Some(build_activity_from_js_item(id, item))
        })
        .collect()
}

/// Build a single Activity from a JS-extracted list page row.
/// The list page provides: type, date, name, time, distance, elevation, suffer score.
fn build_activity_from_js_item(id: &str, item: &serde_json::Value) -> Activity {
    let sport_type_str = item["type"].as_str().unwrap_or("");
    Activity {
        id: id.to_owned(),
        name: item["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: if sport_type_str.is_empty() {
            SportType::Other("Unknown".to_owned())
        } else {
            SportType::from_strava(sport_type_str)
        },
        start_date: item["date"]
            .as_str()
            .and_then(parse_strava_date)
            .unwrap_or_else(Utc::now),
        duration_seconds: item["time"]
            .as_str()
            .and_then(parse_duration_string)
            .unwrap_or(0),
        distance_meters: item["distance"].as_str().and_then(parse_distance_string),
        elevation_gain: item["elevation"]
            .as_str()
            .and_then(|e| e.replace([',', 'm'], "").trim().parse().ok()),
        average_heart_rate: item["avg_hr"]
            .as_str()
            .and_then(|h| h.replace("bpm", "").trim().parse().ok()),
        max_heart_rate: None,
        average_speed: None,
        max_speed: None,
        calories: item["calories"]
            .as_str()
            .and_then(|c| c.replace(',', "").trim().parse().ok()),
        average_power: None,
        max_power: None,
        normalized_power: None,
        average_cadence: None,
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: item["suffer_score"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: None,
        pace: item["pace"].as_str().map(String::from),
        gap: None,
        elapsed_time_seconds: None,
        device_name: None,
        gear_name: None,
        perceived_exertion: None,
        workout_type: None,
        sport_type_detail: if sport_type_str.is_empty() {
            None
        } else {
            Some(sport_type_str.to_owned())
        },
        segment_efforts: None,
        provider: "scraper".to_owned(),
    }
}

/// Build an Activity from detailed activity page JS extraction.
/// The detail page JS extracts name, type, distance, moving time, pace, relative effort,
/// elevation, calories, elapsed time, heart rates, power, cadence, temperature,
/// humidity, and wind speed.
fn build_activity_from_detail(activity_id: &str, data: &serde_json::Value) -> Activity {
    Activity {
        id: activity_id.to_owned(),
        name: data["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: data["type"].as_str().map_or_else(
            || SportType::Other("Unknown".to_owned()),
            SportType::from_strava,
        ),
        start_date: data["date"]
            .as_str()
            .and_then(parse_strava_date)
            .unwrap_or_else(Utc::now),
        duration_seconds: data["moving_time"]
            .as_str()
            .or_else(|| data["elapsed_time"].as_str())
            .and_then(parse_duration_string)
            .unwrap_or(0),
        distance_meters: data["distance"].as_str().and_then(parse_distance_string),
        elevation_gain: data["elevation"]
            .as_str()
            .and_then(|e| e.replace([',', ' '], "").trim().parse().ok()),
        average_heart_rate: data["avg_hr"]
            .as_str()
            .and_then(|h| h.replace("bpm", "").trim().parse().ok()),
        max_heart_rate: data["max_hr"]
            .as_str()
            .and_then(|h| h.replace("bpm", "").trim().parse().ok()),
        average_speed: data["avg_speed"].as_str().and_then(parse_speed_string),
        max_speed: None,
        calories: data["calories"]
            .as_str()
            .and_then(|c| c.replace(',', "").trim().parse().ok()),
        average_power: data["avg_power"]
            .as_str()
            .and_then(|p| p.replace(['W', 'w'], "").trim().parse().ok()),
        max_power: None,
        normalized_power: None,
        average_cadence: data["cadence"]
            .as_str()
            .and_then(|c| c.replace("rpm", "").replace("spm", "").trim().parse().ok()),
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: data["relative_effort"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_latitude: None,
        start_longitude: None,
        city: None,
        region: None,
        country: None,
        temperature: None,
        feels_like: None,
        humidity: None,
        wind_speed: None,
        wind_direction: None,
        weather: None,
        pace: data["pace"].as_str().map(String::from),
        gap: data["gap"].as_str().map(String::from),
        elapsed_time_seconds: data["elapsed_time"]
            .as_str()
            .and_then(parse_duration_string),
        device_name: data["device"].as_str().map(String::from),
        gear_name: data["gear"].as_str().map(String::from),
        perceived_exertion: data["perceived_exertion"].as_str().map(String::from),
        workout_type: None,
        sport_type_detail: data["type"].as_str().map(String::from),
        segment_efforts: None,
        provider: "scraper".to_owned(),
    }
}

/// Merge detail page data into an activity already populated from the list page
fn merge_detail_into_activity(activity: &mut Activity, detail: &serde_json::Value) {
    // Sport type from the detail page heading (more accurate than list page table)
    if let Some(sport) = detail["type"].as_str() {
        let parsed = SportType::from_strava(sport);
        if !matches!(parsed, SportType::Other(_)) {
            activity.sport_type = parsed;
            activity.sport_type_detail = Some(sport.to_owned());
        }
    }

    // Location from the detail page date line
    if let Some(location) = detail["location"].as_str() {
        let parts: Vec<&str> = location.split(',').map(str::trim).collect();
        if let Some(city) = parts.first() {
            activity.city = Some((*city).to_owned());
        }
        if let Some(region) = parts.get(1) {
            activity.region = Some((*region).to_owned());
        }
    }

    merge_optional_u32(&mut activity.average_heart_rate, detail, "avg_hr", &["bpm"]);
    merge_optional_u32(&mut activity.max_heart_rate, detail, "max_hr", &["bpm"]);
    merge_optional_u32(
        &mut activity.average_cadence,
        detail,
        "cadence",
        &["rpm", "spm", "ppm"],
    );
    merge_optional_u32(&mut activity.calories, detail, "calories", &[","]);
    merge_optional_u32(&mut activity.suffer_score, detail, "relative_effort", &[]);
    merge_optional_u32(
        &mut activity.average_power,
        detail,
        "avg_power",
        &["W", "w"],
    );

    // Max speed from embedded JSON (m/s as string)
    if activity.max_speed.is_none() {
        activity.max_speed = detail["max_speed"]
            .as_str()
            .and_then(|s| s.trim().parse().ok());
    }

    merge_optional_string(&mut activity.pace, detail, "pace");
    merge_optional_string(&mut activity.gap, detail, "gap");
    merge_optional_string(&mut activity.weather, detail, "weather");
    merge_optional_string(&mut activity.wind_direction, detail, "wind_direction");
    merge_optional_string(&mut activity.device_name, detail, "device");
    merge_optional_string(&mut activity.gear_name, detail, "gear");
    merge_optional_string(
        &mut activity.perceived_exertion,
        detail,
        "perceived_exertion",
    );

    merge_optional_f32(
        &mut activity.temperature,
        detail,
        "temperature",
        &["°", "℃", "C"],
    );
    merge_optional_f32(
        &mut activity.feels_like,
        detail,
        "feels_like",
        &["°", "℃", "C"],
    );
    merge_optional_f32(&mut activity.humidity, detail, "humidity", &["%"]);
    merge_optional_f32(&mut activity.wind_speed, detail, "wind_speed", &["km/h"]);

    if activity.elapsed_time_seconds.is_none() {
        activity.elapsed_time_seconds = detail["elapsed_time"]
            .as_str()
            .and_then(parse_duration_string);
    }
}

/// Merge an optional u32 field from detail JSON, stripping given suffixes
fn merge_optional_u32(
    field: &mut Option<u32>,
    data: &serde_json::Value,
    key: &str,
    strip: &[&str],
) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().and_then(|v| {
        let mut s = v.to_owned();
        for suffix in strip {
            s = s.replace(suffix, "");
        }
        s.trim().parse().ok()
    });
}

/// Merge an optional f32 field from detail JSON, stripping given suffixes
fn merge_optional_f32(
    field: &mut Option<f32>,
    data: &serde_json::Value,
    key: &str,
    strip: &[&str],
) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().and_then(|v| {
        let mut s = v.to_owned();
        for suffix in strip {
            s = s.replace(suffix, "");
        }
        s.trim().parse().ok()
    });
}

/// Merge an optional String field from detail JSON
fn merge_optional_string(field: &mut Option<String>, data: &serde_json::Value, key: &str) {
    if field.is_some() {
        return;
    }
    *field = data[key].as_str().map(String::from);
}

/// Remove duplicate activity items by ID, preserving first occurrence
fn deduplicate_by_id(items: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| {
        item["id"]
            .as_str()
            .is_some_and(|id| seen.insert(id.to_owned()))
    });
}

/// Apply sport type, date range, and limit filters to an activity list
fn apply_activity_filters(activities: &mut Vec<Activity>, params: &ActivityParams) {
    if let Some(ref sport) = params.sport_type {
        let sport_lower = sport.to_lowercase();
        activities.retain(|a| {
            a.sport_type
                .display_name()
                .to_lowercase()
                .contains(&sport_lower)
                || a.sport_type_detail
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase().contains(&sport_lower))
        });
    }

    if let Some(before) = params.before {
        activities.retain(|a| a.start_date < before);
    }

    if let Some(after) = params.after {
        activities.retain(|a| a.start_date > after);
    }

    if let Some(limit) = params.limit {
        activities.truncate(limit as usize);
    }
}

// ============================================================================
// String parsing helpers
// ============================================================================

/// Parse date strings from various formats (handles day-of-week prefix like "Wed, 3/18/2026")
fn parse_strava_date(s: &str) -> Option<chrono::DateTime<Utc>> {
    let s = s.trim();

    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Strip day-of-week prefix like "Wed, " or "Mon, "
    let s = if s.len() > 5 && s.chars().nth(3) == Some(',') {
        s[5..].trim()
    } else {
        s
    };

    let formats = [
        "%m/%d/%Y",
        "%Y-%m-%d",
        "%B %d, %Y",
        "%b %d, %Y",
        "%Y-%m-%dT%H:%M:%S",
    ];

    for fmt in &formats {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(ndt.and_utc());
        }
        if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, fmt) {
            return nd.and_hms_opt(0, 0, 0).map(|ndt| ndt.and_utc());
        }
    }

    None
}

/// Parse duration strings like "1:23:45" or "45:30" into seconds
fn parse_duration_string(s: &str) -> Option<u64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();

    match parts.len() {
        3 => {
            let hours: u64 = parts[0].parse().ok()?;
            let mins: u64 = parts[1].parse().ok()?;
            let secs: u64 = parts[2].parse().ok()?;
            Some(hours * 3600 + mins * 60 + secs)
        }
        2 => {
            let mins: u64 = parts[0].parse().ok()?;
            let secs: u64 = parts[1].parse().ok()?;
            Some(mins * 60 + secs)
        }
        1 => parts[0].parse().ok(),
        _ => None,
    }
}

/// Parse distance strings like "5.2 km" or "3.1 mi" into meters
fn parse_distance_string(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();

    if s.contains("km") {
        let num_str = s.replace("km", "").replace(',', "").trim().to_owned();
        let km: f64 = num_str.parse().ok()?;
        Some(km * 1000.0)
    } else if s.contains("mi") {
        let num_str = s.replace("mi", "").replace(',', "").trim().to_owned();
        let mi: f64 = num_str.parse().ok()?;
        Some(mi * 1609.344)
    } else if s.contains('m') {
        let num_str = s.replace(['m', ','], "").trim().to_owned();
        num_str.parse().ok()
    } else {
        s.replace(',', "").parse().ok()
    }
}

/// Parse speed strings like "10 km/h" or "6.2 mph" into m/s
fn parse_speed_string(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();
    if s.contains("km/h") || s.contains("kph") {
        let num: f64 = s
            .replace("km/h", "")
            .replace("kph", "")
            .trim()
            .parse()
            .ok()?;
        Some(num / 3.6)
    } else if s.contains("mph") {
        let num: f64 = s.replace("mph", "").trim().parse().ok()?;
        Some(num * 0.447_04)
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration() {
        assert_eq!(parse_duration_string("1:23:45"), Some(5025));
        assert_eq!(parse_duration_string("45:30"), Some(2730));
        assert_eq!(parse_duration_string("3600"), Some(3600));
        assert_eq!(parse_duration_string(""), None);
    }

    #[test]
    fn parse_distance() {
        let d = parse_distance_string("5.2 km").unwrap();
        assert!((d - 5200.0).abs() < 1.0);

        let d = parse_distance_string("3.1 mi").unwrap();
        assert!((d - 4988.967).abs() < 1.0);

        let d = parse_distance_string("800m").unwrap();
        assert!((d - 800.0).abs() < 1.0);
    }

    #[test]
    fn parse_speed() {
        let s = parse_speed_string("10 km/h").unwrap();
        assert!((s - 2.7778).abs() < 0.01);

        let s = parse_speed_string("6.2 mph").unwrap();
        assert!((s - 2.7716).abs() < 0.01);
    }

    #[test]
    fn parse_date() {
        assert!(parse_strava_date("2024-03-15").is_some());
        assert!(parse_strava_date("March 15, 2024").is_some());
        assert!(parse_strava_date("Wed, 3/18/2026").is_some());
        assert!(parse_strava_date("garbage").is_none());
    }

    #[test]
    fn parse_date_with_weekday_prefix() {
        assert!(parse_strava_date("Wed, 3/18/2026").is_some());
        assert!(parse_strava_date("Mon, 1/5/2025").is_some());
    }

    // ========================================================================
    // Credential login unit tests
    // ========================================================================

    #[test]
    fn otp_url_patterns_match_specific_challenges() {
        let patterns = OTP_URL_PATTERNS;

        // Should match
        assert!(patterns
            .iter()
            .any(|p| "https://accounts.google.com/v3/signin/challenge/totp?x=1".contains(p)));
        assert!(patterns
            .iter()
            .any(|p| "https://accounts.google.com/challenge/sms/verify".contains(p)));
        assert!(patterns
            .iter()
            .any(|p| "https://example.com/2fa".contains(p)));
        assert!(patterns
            .iter()
            .any(|p| "https://example.com/verify".contains(p)));
        assert!(patterns
            .iter()
            .any(|p| "https://example.com/mfa".contains(p)));

        // Should NOT match (passkey, password, generic)
        assert!(!patterns
            .iter()
            .any(|p| "https://accounts.google.com/challenge/pk".contains(p)));
        assert!(!patterns
            .iter()
            .any(|p| "https://accounts.google.com/challenge/pwd".contains(p)));
        assert!(!patterns
            .iter()
            .any(|p| "https://accounts.google.com/v3/signin/identifier".contains(p)));
    }

    #[test]
    fn passkey_pattern_matches_challenge_pk() {
        assert!("https://accounts.google.com/v3/signin/challenge/pk?x=1"
            .contains(PASSKEY_CHALLENGE_PATTERN));
        assert!(!"https://accounts.google.com/challenge/totp".contains(PASSKEY_CHALLENGE_PATTERN));
        assert!(!"https://accounts.google.com/challenge/pwd".contains(PASSKEY_CHALLENGE_PATTERN));
    }

    #[test]
    fn challenge_skip_patterns_exclude_password_and_passkey() {
        let url_pwd = "https://accounts.google.com/v3/signin/challenge/pwd?x=1";
        let url_pk = "https://accounts.google.com/v3/signin/challenge/pk?x=1";
        let url_totp = "https://accounts.google.com/v3/signin/challenge/totp?x=1";
        let url_selection = "https://accounts.google.com/v3/signin/challenge/selection";

        // pwd and pk should be skipped
        assert!(CHALLENGE_SKIP_PATTERNS.iter().any(|p| url_pwd.contains(p)));
        assert!(CHALLENGE_SKIP_PATTERNS.iter().any(|p| url_pk.contains(p)));

        // totp and selection should NOT be skipped
        assert!(!CHALLENGE_SKIP_PATTERNS.iter().any(|p| url_totp.contains(p)));
        assert!(!CHALLENGE_SKIP_PATTERNS
            .iter()
            .any(|p| url_selection.contains(p)));
    }

    #[test]
    fn challenge_url_pattern_matches_all_challenges() {
        assert!(
            "https://accounts.google.com/v3/signin/challenge/totp".contains(CHALLENGE_URL_PATTERN)
        );
        assert!(
            "https://accounts.google.com/v3/signin/challenge/pk".contains(CHALLENGE_URL_PATTERN)
        );
        assert!(
            "https://accounts.google.com/v3/signin/challenge/pwd".contains(CHALLENGE_URL_PATTERN)
        );
        assert!(!"https://accounts.google.com/v3/signin/identifier".contains(CHALLENGE_URL_PATTERN));
    }

    #[test]
    fn login_selectors_from_valid_provider() {
        let provider = ProviderConfig::strava_default();
        let selectors = LoginSelectors::from_provider(&provider).unwrap();
        assert!(!selectors.email.is_empty());
        assert!(!selectors.password.is_empty());
        assert!(!selectors.button.is_empty());
    }

    #[test]
    fn login_selectors_from_provider_missing_email() {
        let mut provider = ProviderConfig::strava_default();
        provider.provider.login_email_selector = None;
        let result = LoginSelectors::from_provider(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("login_email_selector"));
    }

    #[test]
    fn login_selectors_from_provider_missing_password() {
        let mut provider = ProviderConfig::strava_default();
        provider.provider.login_password_selector = None;
        let result = LoginSelectors::from_provider(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("login_password_selector"));
    }

    #[test]
    fn login_selectors_from_provider_missing_button() {
        let mut provider = ProviderConfig::strava_default();
        provider.provider.login_button_selector = None;
        let result = LoginSelectors::from_provider(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("login_button_selector"));
    }

    #[test]
    fn two_fa_option_with_coords_deserializes_from_json() {
        let json = r#"[
            {"id": "otp", "label": "Get a verification code", "x": 150.5, "y": 250.0},
            {"id": "sms", "label": "Text message to (•••) ••••-53", "x": 150.5, "y": 350.0}
        ]"#;
        let options: Vec<TwoFactorOptionWithCoords> = serde_json::from_str(json).unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].id, "otp");
        assert!((options[0].x - 150.5).abs() < 0.01);
        assert_eq!(options[1].id, "sms");
    }

    #[test]
    fn two_fa_option_with_coords_empty_json() {
        let options: Vec<TwoFactorOptionWithCoords> = serde_json::from_str("[]").unwrap();
        assert!(options.is_empty());
    }

    #[test]
    fn two_fa_option_serializes_to_json() {
        let option = crate::error::TwoFactorOption {
            id: "otp".to_owned(),
            label: "Google Authenticator".to_owned(),
        };
        let json = serde_json::to_string(&option).unwrap();
        assert!(json.contains(r#""id":"otp""#));
        assert!(json.contains(r#""label":"Google Authenticator""#));
    }

    #[test]
    fn google_oauth_selectors_defined() {
        assert!(!GOOGLE_OAUTH_SELECTORS.email.is_empty());
        assert!(!GOOGLE_OAUTH_SELECTORS.email_next.is_empty());
        assert!(!GOOGLE_OAUTH_SELECTORS.password.is_empty());
        assert!(!GOOGLE_OAUTH_SELECTORS.password_next.is_empty());
        assert!(GOOGLE_OAUTH_SELECTORS.password_next.contains("text:Next"));
    }

    #[test]
    fn apple_oauth_selectors_defined() {
        assert!(!APPLE_OAUTH_SELECTORS.email.is_empty());
        assert!(!APPLE_OAUTH_SELECTORS.password.is_empty());
    }

    #[test]
    fn google_otp_submit_selector_includes_totp_next() {
        assert!(GOOGLE_OTP_SUBMIT_SELECTOR.contains("totpNext"));
        assert!(GOOGLE_OTP_SUBMIT_SELECTOR.contains("text:Next"));
    }

    #[test]
    fn strava_provider_has_oauth_buttons() {
        let provider = ProviderConfig::strava_default();
        assert!(provider.provider.login_oauth_buttons.contains_key("google"));
        assert!(provider.provider.login_oauth_buttons.contains_key("apple"));
    }

    #[test]
    fn strava_provider_has_otp_selector() {
        let provider = ProviderConfig::strava_default();
        assert!(provider.provider.login_otp_selector.is_some());
    }

    #[test]
    fn url_path_matches_ignores_query_params() {
        let patterns = vec!["/modern".to_owned(), "/dashboard".to_owned()];

        // Should NOT match — /modern is in query string, not path
        assert!(!url_path_matches(
            "https://sso.garmin.com/portal/sso/en-US/mfa?service=https://connect.garmin.com/modern",
            &patterns
        ));

        // Should match — /dashboard is in the path
        assert!(url_path_matches(
            "https://connect.garmin.com/modern/dashboard?foo=bar",
            &patterns
        ));

        // Should match — /modern is in the path
        assert!(url_path_matches(
            "https://connect.garmin.com/modern/activities",
            &patterns
        ));
    }

    #[test]
    fn url_path_matches_no_query_string() {
        let patterns = vec!["/dashboard".to_owned()];
        assert!(url_path_matches(
            "https://www.strava.com/dashboard",
            &patterns
        ));
        assert!(!url_path_matches("https://www.strava.com/login", &patterns));
    }

    #[test]
    fn url_path_matches_garmin_mfa_not_success() {
        let success = vec![
            "/app/home".to_owned(),
            "/app/activities".to_owned(),
            "/modern".to_owned(),
            "/dashboard".to_owned(),
        ];
        let mfa_url = "https://sso.garmin.com/portal/sso/en-US/mfa?clientId=GarminConnect&service=https://connect.garmin.com/modern";
        assert!(
            !url_path_matches(mfa_url, &success),
            "Garmin MFA URL should NOT match success patterns"
        );
    }

    #[test]
    fn url_path_matches_strava_login_redirect() {
        let success = vec![
            "/dashboard".to_owned(),
            "/athlete".to_owned(),
            "/onboarding".to_owned(),
        ];
        // Transient redirect through /login should not match success
        assert!(!url_path_matches("https://www.strava.com/login", &success));
        // Final destination should match
        assert!(url_path_matches(
            "https://www.strava.com/dashboard",
            &success
        ));
        assert!(url_path_matches(
            "https://www.strava.com/athlete/training",
            &success
        ));
    }

    #[test]
    fn garmin_provider_has_profile_url() {
        let provider = ProviderConfig::garmin_default();
        assert!(provider.provider.profile_url.is_some());
        assert!(provider.provider.profile_js_extract.is_some());
    }
}

// ABOUTME: Chromiumoxide-based sport activity scraper driven by TOML provider configs
// ABOUTME: Implements ActivityScraper trait using headless Chrome via CDP with configurable selectors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashSet;
use std::env;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "vision")]
use crate::vision_model::VisionModel;
use async_trait::async_trait;
use chromiumoxide::browser::Browser;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use chrono::{DateTime, NaiveDateTime, Utc};
use tokio::fs;
use tokio::sync::Mutex;
use tokio::time::{self, Instant};
use tracing::{debug, info, warn};

use crate::browser_utils::apply_minimal_stealth;
use crate::browser_utils::{
    capture_session, cdp_click_at, click_element, dismiss_cookie_dialog, element_exists,
    fill_input_field, inject_cookies, launch_browser, open_page_with_stealth, read_visible_text,
};
#[cfg(feature = "vision")]
use crate::config::LoginMode;
use crate::config::ScraperConfig;
use crate::error::{LoginResult, ScraperError, ScraperResult, TwoFactorOption};
use crate::fake_login;
use crate::models::{
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams, Lap, Split,
    SportType,
};
use crate::pending_login::PendingLogin;
use crate::provider::{ListPagination, ProviderConfig};
use crate::script_loader;
use crate::teardown_signal::TeardownGuard;
use crate::types::ActivityScraper;
#[cfg(feature = "vision")]
use crate::vision::VisionScraper;

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
/// (requires the `vision` feature and a [`VisionModel`]).
pub struct ChromeScraper {
    config: ScraperConfig,
    provider: ProviderConfig,
    /// Shared browser instance for headless scraping (lazily created)
    browser: Mutex<Option<Arc<Browser>>>,
    /// Browser + page kept alive during OTP/2FA flow for follow-up calls.
    /// Stores both so Chrome isn't killed when `credential_login` returns.
    /// Wrapped in `PendingLogin` so abandoned 2FA flows are evicted after
    /// `ScraperConfig::pending_login_ttl_secs` instead of pinning Chrome
    /// for the lifetime of the scraper.
    pending_login: Mutex<Option<PendingLogin<(Browser, chromiumoxide::Page)>>>,
    /// Optional vision model for vision-based login (requires `vision` feature)
    #[cfg(feature = "vision")]
    llm: Option<Arc<dyn VisionModel>>,
    /// Persistent vision scraper instance for multi-step login flows (OTP/2FA follow-up)
    #[cfg(feature = "vision")]
    vision_scraper: Mutex<Option<VisionScraper>>,
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

    /// Set the vision model for vision-based login (requires `vision` feature)
    #[cfg(feature = "vision")]
    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn VisionModel>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Create with default browser config and the built-in Strava provider
    ///
    /// # Errors
    ///
    /// Returns a config error if the embedded Strava provider TOML is malformed
    /// (compile-time constant, tested).
    pub fn default_config() -> ScraperResult<Self> {
        Ok(Self::new(
            ScraperConfig::default(),
            ProviderConfig::strava_default()?,
        ))
    }

    /// Get a reference to the provider configuration
    pub const fn provider(&self) -> &ProviderConfig {
        &self.provider
    }

    /// Get or create a headless browser instance for scraping.
    ///
    /// `profile_id` (typically `AuthSession.session_id`) selects the on-disk
    /// Chrome profile so cookies + localStorage persist across launches —
    /// crucial for keeping `cf_clearance` valid between scrape calls and
    /// avoiding Turnstile re-solves.
    async fn get_headless_browser(&self, profile_id: &str) -> ScraperResult<Arc<Browser>> {
        let mut guard = self.browser.lock().await;

        if let Some(browser) = guard.as_ref() {
            return Ok(Arc::clone(browser));
        }

        let browser = launch_browser(&self.config, true, Some(profile_id)).await?;
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));

        info!(profile_id, "Headless browser launched for scraping");
        Ok(browser)
    }

    /// Gracefully close the headless browser and pending login browser if open.
    ///
    /// Sends `Browser.close` CDP command so Chrome shuts down cleanly and the
    /// WebSocket handler task exits without error-looping. Safe to call multiple
    /// times — subsequent calls are no-ops.
    async fn close_browsers(&self) {
        // Open the teardown grace window before any close() call.
        // chromiumoxide's handler task emits its WS-reset `error!` log
        // *after* close().await resolves; the guard's Drop schedules a
        // delayed TEARDOWN_DEPTH decrement so the platform's tracing
        // layer keeps suppressing those expected post-close events for
        // a few hundred milliseconds, then resumes normal error
        // visibility. The binding lives to end-of-scope (load-bearing
        // — Drop is the side effect we need).
        let teardown_guard = TeardownGuard::new();

        let headless = self.browser.lock().await.take();
        if let Some(browser) = headless {
            let strong_count = Arc::strong_count(&browser);
            if strong_count > 1 {
                warn!(
                    strong_count,
                    "Browser Arc has multiple references, close will be skipped by Arc::into_inner"
                );
            }
            if let Some(mut browser) = Arc::into_inner(browser) {
                if let Err(e) = browser.close().await {
                    debug!(error = %e, "Browser close returned error (Chrome may already be gone)");
                }
                info!("Browser closed gracefully");
            } else {
                warn!("Arc::into_inner returned None — browser will be killed on drop");
            }
        }
        let pending = self.pending_login.lock().await.take();
        if let Some(pending) = pending {
            // TTL doesn't matter at shutdown — close whatever's parked.
            if let Some((mut browser, _page)) =
                pending.into_inner_if_fresh(Duration::from_secs(u64::MAX))
            {
                if let Err(e) = browser.close().await {
                    debug!(error = %e, "Pending login browser close returned error");
                }
            }
        }
        // Explicit drop here gives the guard a syntactic mention so the
        // unused_variables lint stays happy without resorting to the
        // `_` prefix the architectural validator forbids. The 500ms
        // grace window starts ticking on this drop, which is precisely
        // when we want the suppression window to close: after both
        // browser.close().await calls above have already resolved.
        drop(teardown_guard);
    }

    /// Park an in-flight 2FA/OTP login so a follow-up `submit_otp` /
    /// `select_two_factor` call can resume the same Chrome page. Drops any
    /// previously parked session first — both as a sanity sweep (abandoned
    /// flows from the same scraper) and to keep the field a single-slot
    /// queue.
    async fn store_pending_login(&self, browser: Browser, page: chromiumoxide::Page) {
        *self.pending_login.lock().await = Some(PendingLogin::new((browser, page)));
    }

    /// Take the parked 2FA/OTP login if it was stored less than
    /// `config.pending_login_ttl_secs` ago. Expired entries are dropped on
    /// access — chromiumoxide's `kill_on_drop` reaps the held Chrome.
    async fn take_pending_login(&self) -> Option<(Browser, chromiumoxide::Page)> {
        let parked = self.pending_login.lock().await.take()?;
        let ttl = Duration::from_secs(self.config.pending_login_ttl_secs);
        let result = parked.into_inner_if_fresh(ttl);
        if result.is_none() {
            debug!(
                ttl_secs = self.config.pending_login_ttl_secs,
                "Evicted expired pending login (Chrome will be reaped on drop)"
            );
        }
        result
    }

    /// Open a new page with session cookies and navigate to the given URL
    async fn open_authenticated_page(
        &self,
        session: &AuthSession,
        url: &str,
    ) -> ScraperResult<chromiumoxide::Page> {
        let browser = self.get_headless_browser(&session.session_id).await?;

        // Authenticated paths inject session cookies — CDP rejects cookies on
        // about:blank ("Blank page can not have cookie"), so navigate to the
        // login domain first (no challenge for cookie-authenticated sessions),
        // then register stealth so it's active for the final goto.
        let page = browser
            .new_page(&self.provider.provider.login_url)
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to open page: {e}"),
            })?;

        apply_minimal_stealth(&page).await?;

        time::sleep(Duration::from_millis(self.config.interaction_delay_ms)).await;

        inject_cookies(&page, session).await?;

        // Navigate to the actual target URL with cookies set.
        //
        // Heavy SPAs (Garmin /app/) hold open WebSockets / lazy-load resources
        // long after the initial XHRs resolve, so the underlying CDP
        // `Page.navigate` response can be delayed past chromiumoxide's
        // default request timeout. We wrap with `tokio::time::timeout` and
        // tolerate timeouts: by 6s the page has dispatched its hydration
        // XHRs into the stealth capture map, which is all we need.
        let goto_outcome = time::timeout(Duration::from_secs(15), page.goto(url)).await;
        match goto_outcome {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                return Err(ScraperError::Browser {
                    reason: format!("Failed to navigate to {url}: {e}"),
                });
            }
            Err(_elapsed) => {
                debug!(
                    url,
                    "page.goto timed out at 15s — continuing; capture interceptor \
                     does not require the load event"
                );
            }
        }

        // Modern provider apps (Garmin /app/) bounce through SSO on first
        // navigation to obtain a JetLagToken before allowing /gc-api/ calls.
        // This dance takes 5-8 seconds. Subsequent in-tab navigations within
        // /app/* reuse the localStorage'd token and are fast. 5s covers the
        // first-load case; older pre-rendered pages (Strava /training) waste
        // the time but are otherwise unaffected.
        time::sleep(Duration::from_secs(5)).await;

        if let Ok(Some(landed)) = page.url().await {
            info!(target = url, landed = %landed, "Page navigation settled");
        }

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

        let vision =
            VisionScraper::new(self.config.clone(), self.provider.clone(), Arc::clone(llm));

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
        time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;

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
        time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
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
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Fill email on the OAuth provider's page
        debug!(selector = oauth_form.email, "Filling OAuth email field");
        fill_input_field(page, oauth_form.email, email).await?;
        time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
        debug!("Clicking Next after OAuth email");
        click_element(page, oauth_form.email_next).await?;

        // Wait for the page transition — the password field may exist as a hidden element
        // on the email step, so we must wait for Google to actually transition pages
        debug!(
            wait_secs = config.page_load_wait_secs,
            "Waiting for OAuth page transition"
        );
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

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
        time::sleep(Duration::from_secs(1)).await;
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

        let browser = launch_browser(&self.config, false, None).await?;
        let page = open_page_with_stealth(&browser, &self.provider.provider.login_url).await?;

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
            let base =
                fake_login::start_fake_server()
                    .await
                    .map_err(|e| ScraperError::Browser {
                        reason: format!("Failed to start fake login server: {e}"),
                    })?;
            info!(base_url = %base, "Using fake login server");
            format!("{base}/strava/login.html")
        } else {
            self.provider.provider.login_url.clone()
        };

        let browser = launch_browser(
            config,
            config.fake_login || config.credential_login_headless,
            None,
        )
        .await?;
        let page = open_page_with_stealth(&browser, &login_url).await?;

        debug!(
            wait_secs = config.page_load_wait_secs,
            "Waiting for page JS to render"
        );
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
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
            self.store_pending_login(browser, page).await;
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
            self.take_pending_login()
                .await
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
        time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
        click_element(&page, &combined_button).await?;

        // Wait for Google to process the code and redirect away from the TOTP page
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

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
            self.store_pending_login(browser, page).await;
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
            self.take_pending_login()
                .await
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
                self.store_pending_login(browser, page).await;
            }
            return Ok(result);
        }

        info!(option_id, "Selecting 2FA method");
        if !cdp_click_two_fa_option(&page, option_id).await {
            self.store_pending_login(browser, page).await;
            return Err(ScraperError::Auth {
                reason: format!("2FA option '{option_id}' not found on page"),
            });
        }

        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;

        // Check if we're already on an OTP code entry page
        let current_url = page.url().await.ok().flatten().unwrap_or_default();
        if path_contains_any(&current_url, OTP_URL_PATTERNS) {
            info!(url = %current_url, "Already on OTP page after selecting 2FA method");
            self.store_pending_login(browser, page).await;
            return Ok(LoginResult::OtpRequired);
        }

        // For "app" (phone tap): check if a number matching challenge appeared
        if option_id == "app" {
            if let Some(number) = extract_number_from_page(&page).await {
                info!(number = %number, "Number matching challenge detected");
                self.store_pending_login(browser, page).await;
                return Ok(LoginResult::NumberMatch(number));
            }
            // Debug: dump what's visible on the page so we can improve extraction
            let current_url = page.url().await.ok().flatten().unwrap_or_default();
            warn!(url = %current_url, "Number extraction returned None — dumping page debug info");
            if let Ok(screenshot) = page
                .save_screenshot(
                    ScreenshotParams::builder().full_page(true).build(),
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
            self.store_pending_login(browser, page).await;
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
        let mut all_items = paginate_activity_list(
            &page,
            &js,
            params,
            self.provider.list_page.pagination.as_ref(),
            self.config.max_scrape_pages,
            self.config.interaction_delay_ms,
        )
        .await;

        // Truncate to target and deduplicate by ID
        deduplicate_by_id(&mut all_items);
        let mut activities = parse_js_activity_items(&all_items);
        apply_activity_filters(&mut activities, params);

        info!(
            count = activities.len(),
            "Activities extracted from list page"
        );

        // Augment activities with `city` / `region` from Strava's dashboard
        // feed JSON. The feed exposes `timeAndLocation.location` ("City,
        // Region") per activity — Strava strips precise GPS from the list
        // endpoints, so this is the cheapest path to a coarse start
        // location, and the platform's weather backfill can geocode the
        // string into a lat/lng for ambient temperature lookup.
        // Fetched in-page via same-origin XHR so cookies travel for free.
        // Best-effort: if the feed is unreachable or the activity isn't in
        // its window, the activity stays without city — extractor stays
        // intact, no failure.
        if !activities.is_empty() && self.provider.list_page.url.contains("strava.com") {
            if let Err(e) =
                enrich_activities_with_dashboard_feed(&page, &mut activities, target_count).await
            {
                debug!(error = %e, "Dashboard feed enrichment skipped");
            }

            fill_missing_locations_from_detail_pages(
                &page,
                &mut activities,
                &self.provider,
                &self.config,
            )
            .await;
        }

        // Optionally enrich each activity by navigating to its detail page.
        // enrich_limit caps the N+1 to the most recent N (the list is
        // reverse-chronological); None enriches all. The un-enriched tail keeps
        // its list-page fields (type, date, distance, elevation).
        if params.enrich_details {
            let enrich_cap = params.enrich_limit.unwrap_or(activities.len());
            let total = activities.len().min(enrich_cap);
            info!(
                enriching = total,
                scraped = activities.len(),
                "Enriching activities from detail pages (this may take a while)"
            );
            for (i, activity) in activities.iter_mut().enumerate().take(enrich_cap) {
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
        drop(page);
        self.close_browsers().await;
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
        drop(page);
        self.close_browsers().await;
        Ok(activity)
    }

    async fn get_activity_raw(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<serde_json::Value> {
        let url = self.provider.detail_url(activity_id);
        info!(url = %url, "Navigating to activity detail page (raw mode)");

        let page = self.open_authenticated_page(session, &url).await?;

        // The /app/ Garmin Connect is a fully client-rendered React app — its
        // initial XHRs to /gc-api/activity-service/* fire after hydration,
        // typically 2-4 seconds after page load. Wait long enough for the
        // stealth script's API capture map to be populated before extracting.
        time::sleep(Duration::from_secs(5)).await;

        let data = extract_detail_via_js(&page, &self.provider).await?;

        info!(id = activity_id, "Raw activity detail JSON extracted");
        drop(page);
        self.close_browsers().await;
        Ok(data)
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
        drop(page);
        self.close_browsers().await;
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
            time::sleep(Duration::from_secs(self.config.page_load_wait_secs * 2)).await;

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
        self.close_browsers().await;
        Ok(summary)
    }

    async fn close_browser(&self) {
        self.close_browsers().await;
    }

    async fn probe_list_page_for_gps(
        &self,
        session: &AuthSession,
    ) -> ScraperResult<serde_json::Value> {
        let pages = [
            ("training-table", "https://www.strava.com/athlete/training"),
            ("dashboard-feed", "https://www.strava.com/dashboard"),
        ];

        let mut report = serde_json::Map::new();
        for (name, url) in pages {
            let page = self.open_authenticated_page(session, url).await?;

            let landed = page
                .url()
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "<unknown>".to_owned());

            let dump_str = match page.evaluate(LIST_GPS_PROBE_JS).await {
                Ok(r) => r
                    .value()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                Err(e) => format!("__probe_eval_error: {e}"),
            };

            let dump: serde_json::Value =
                serde_json::from_str(&dump_str).unwrap_or(serde_json::Value::String(dump_str));

            report.insert(
                name.to_owned(),
                serde_json::json!({
                    "requested_url": url,
                    "landed_url": landed,
                    "dump": dump,
                }),
            );
        }

        self.close_browsers().await;
        Ok(serde_json::Value::Object(report))
    }
}

/// Page-context JavaScript that walks the rendered list/feed pages and
/// reports every place an activity start-coordinate could be hiding so
/// we can pick the cheapest extraction path. Returns a JSON string
/// (Strava CSP forbids returning structured objects directly).
const LIST_GPS_PROBE_JS: &str = r#"(function() {
    const out = {
        url: window.location.href,
        rows: [],
        scripts: [],
        globals: {},
        mapbox_imgs: [],
    };

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
            const nd = window.__NEXT_DATA__;
            out.globals.__NEXT_DATA__ = {
                top_keys: Object.keys(nd).slice(0, 30),
                props_top_keys: nd.props ? Object.keys(nd.props).slice(0, 30) : [],
                page: nd.page || null,
            };
            try {
                const ndStr = JSON.stringify(nd);
                const ndLatlng = ndStr.match(/"start_latlng"\s*:\s*\[[^\]]*\]/g) || [];
                const ndPoly = ndStr.match(/"summary_polyline"\s*:\s*"[^"]{1,40}"/g) || [];
                out.globals.__NEXT_DATA__.start_latlng_hits = ndLatlng.slice(0, 5);
                out.globals.__NEXT_DATA__.summary_polyline_hits = ndPoly.slice(0, 5);
                out.globals.__NEXT_DATA__.size_bytes = ndStr.length;
            } catch(e) {
                out.globals.__NEXT_DATA__.stringify_error = String(e);
            }
        }
    } catch(e) {}

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

// ============================================================================
// Login flow types and helpers
// ============================================================================

/// Check URL patterns against the path only (strip query params)
/// Try to extract a 2-3 digit number from the page (Google number matching challenge).
/// Looks for a prominent number displayed on screen.
async fn extract_number_from_page(page: &chromiumoxide::Page) -> Option<String> {
    let js = script_loader::loader().load("extract_number.js").await;
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

/// Return `true` when the rendered document body contains the given
/// substring. Used to fingerprint Google's automation-rejection page,
/// which has no stable URL pattern but ships consistent error copy.
async fn page_contains_text(page: &chromiumoxide::Page, needle: &str) -> bool {
    let js = format!(
        "((document.body && document.body.textContent) || '').indexOf({}) >= 0",
        serde_json::to_string(needle).unwrap_or_else(|_| "''".to_owned())
    );
    page.evaluate(js)
        .await
        .ok()
        .and_then(|r| r.value().and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// Outcome of waiting for the next login step
enum StepOutcome {
    /// The expected field appeared in the DOM
    FieldAppeared,
    /// Login resolved early (success, OTP, or failure)
    LoginResult(LoginResult),
}

/// Control-flow result of handling the Google device prompt (`/challenge/dp`)
/// during a poll iteration.
enum DevicePromptOutcome {
    /// A number-match digit was scraped — return this to the caller.
    Resolved(LoginResult),
    /// The prompt is still pending (clicked "Try another way" or waiting on
    /// the bounded stuck counter) — the poll loop should `continue`.
    KeepPolling,
}

/// Handle a single poll iteration on Google's device prompt (`/challenge/dp`).
///
/// Tries to scrape a number-match digit first (the push-notification happy
/// path). If none is present, clicks "Try another way" exactly once to surface
/// the 2FA chooser. When that click fails to navigate away from `/challenge/dp`
/// and no digit appears, the consecutive stuck polls are bounded by
/// `config.stuck_challenge_max_polls`; once exceeded, a structured `Auth` error
/// is returned so Hybrid login mode can escalate to the vision fallback instead
/// of polling to the full timeout.
async fn handle_device_prompt(
    page: &chromiumoxide::Page,
    config: &ScraperConfig,
    tried_another_way: &mut bool,
    stuck_polls: &mut u32,
) -> ScraperResult<DevicePromptOutcome> {
    if let Some(number) = extract_number_from_page(page).await {
        info!(number = %number, "Number-match digit scraped from /challenge/dp");
        return Ok(DevicePromptOutcome::Resolved(LoginResult::NumberMatch(
            number,
        )));
    }
    if *tried_another_way {
        // The single "Try another way" click did not navigate away from
        // /challenge/dp and no number-match digit surfaced. This is the stuck
        // state (Google DOM drift / push-only prompt). Bound the wait so
        // Hybrid login escalates to vision instead of polling to the timeout.
        *stuck_polls += 1;
        if *stuck_polls >= config.stuck_challenge_max_polls {
            save_timeout_screenshot(page, "challenge-dp-stuck").await;
            return Err(ScraperError::Auth {
                reason: format!(
                    "Stuck on Google device prompt ({DEVICE_PROMPT_PATTERN}) after \
                     'Try another way' failed to navigate and no number-match digit \
                     was found across {stuck_polls} polls"
                ),
            });
        }
        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    } else {
        info!("Device prompt detected — clicking 'Try another way' once to surface 2FA chooser");
        let _ = click_element(page, TRY_ANOTHER_WAY_SELECTOR).await;
        *tried_another_way = true;
        time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
    }
    Ok(DevicePromptOutcome::KeepPolling)
}

/// Poll until a target field appears OR a login result is detected (success/OTP/error)
async fn poll_for_next_step(
    page: &chromiumoxide::Page,
    provider: &ProviderConfig,
    config: &ScraperConfig,
    field_selector: &str,
    timeout_secs: u64,
) -> ScraperResult<StepOutcome> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut tried_another_way = false;
    // Counts consecutive polls spent stuck on /challenge/dp after the single
    // "Try another way" click failed to navigate. Bounded so Hybrid login can
    // escalate to vision quickly instead of polling to the full step timeout.
    let mut stuck_polls: u32 = 0;

    loop {
        if Instant::now() > deadline {
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

        // Google "browser may not be secure" interstitial — automation
        // detection fired before we could fill the password field. The page
        // has no recoverable button; return a clear error so callers know to
        // fix stealth instead of bumping the timeout.
        if url.contains("/v3/signin/rejected")
            || url.contains("BrowserNotSupported")
            || page_contains_text(page, "browser or app may not be secure").await
        {
            save_timeout_screenshot(page, "google-browser-rejected").await;
            return Err(ScraperError::Auth {
                reason: "Google rejected the browser as insecure (automation detected). \
                         Run with DRAVR_SCIOTTE_CREDENTIAL_LOGIN_HEADLESS=false, or \
                         use the email/password login flow on Strava directly."
                    .to_owned(),
            });
        }

        // Passkey challenge — click "Try another way" then "Enter your password"
        if url.contains(PASSKEY_CHALLENGE_PATTERN) {
            info!("Passkey challenge detected, clicking 'Try another way'");
            let _ = click_element(page, TRY_ANOTHER_WAY_SELECTOR).await;
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            info!("Clicking 'Enter your password' via CDP");
            cdp_click_enter_password(page).await;
            // Double-click — Google sometimes needs a second click on the challenge option
            time::sleep(Duration::from_secs(1)).await;
            cdp_click_enter_password(page).await;
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            continue;
        }

        // Device prompt (/challenge/dp) — pre-538b7a5 behavior: click
        // "Try another way" to reach `/challenge/selection`, which exposes
        // the real 2FA chooser (`app` / `otp` / `sms`) so the user can
        // pick. If a number-match digit is shown on the desktop, scrape
        // it instead — the user matches the digit on their phone. The
        // `tried_another_way` guard prevents firing the click on every
        // polling iteration (duplicate clicks generated duplicate phone
        // notifications in earlier flows).
        if url.contains(DEVICE_PROMPT_PATTERN) {
            match handle_device_prompt(page, config, &mut tried_another_way, &mut stuck_polls)
                .await?
            {
                DevicePromptOutcome::Resolved(result) => {
                    return Ok(StepOutcome::LoginResult(result));
                }
                DevicePromptOutcome::KeepPolling => continue,
            }
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
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
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

        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
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
    let js = script_loader::loader().load("parse_2fa_options.js").await;
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
    let js = script_loader::loader()
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
        let path = env::temp_dir().join(format!("sciotte-{label}.png"));
        if fs::write(&path, &data).await.is_ok() {
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
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut tried_another_way = false;
    // Counts consecutive polls spent stuck on /challenge/dp after the single
    // "Try another way" click failed to navigate. Bounded so Hybrid login can
    // escalate to vision quickly instead of polling to the full timeout.
    let mut stuck_polls: u32 = 0;

    // Capture the initial URL so we can detect when the page actually changes
    let initial_url = page.url().await.ok().flatten().unwrap_or_default();
    debug!(initial_url = %initial_url, "Polling for login result");

    loop {
        if Instant::now() > deadline {
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
            time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
            continue;
        }

        // Passkey challenge (after password) — click "Try another way" to reach 2FA selection.
        // Don't try "Enter your password" here — password was already submitted.
        if url.contains(PASSKEY_CHALLENGE_PATTERN) {
            info!("Passkey challenge detected post-password, clicking 'Try another way'");
            let _ = click_element(page, TRY_ANOTHER_WAY_SELECTOR).await;
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            continue;
        }

        // Device prompt (/challenge/dp) — same approach as pre-538b7a5:
        // click "Try another way" to reach `/challenge/selection`, which
        // exposes the 2FA chooser (`app`/`otp`/`sms`) for the user to
        // pick. If a number-match digit is on the desktop, scrape it
        // instead. The `tried_another_way` guard prevents firing the
        // click on every polling iteration — duplicate clicks generated
        // duplicate phone notifications in earlier flows.
        if url.contains(DEVICE_PROMPT_PATTERN) {
            match handle_device_prompt(page, config, &mut tried_another_way, &mut stuck_polls)
                .await?
            {
                DevicePromptOutcome::Resolved(result) => return Ok(result),
                DevicePromptOutcome::KeepPolling => continue,
            }
        }

        // Challenge selection page — could be 2FA options or sign-in method chooser.
        if url.contains(CHALLENGE_URL_PATTERN)
            && !CHALLENGE_SKIP_PATTERNS.iter().any(|p| url.contains(p))
        {
            info!(url = %url, "Challenge selection page detected");
            // Wait for the page DOM to render before parsing options —
            // without this, slow runners (Windows CI) may see an empty DOM
            // and misclassify a 2FA page as a sign-in method chooser.
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            let options = parse_two_fa_options(page).await;
            if !options.is_empty() {
                // Real 2FA options found — return to caller for orchestration
                let choices: Vec<TwoFactorOption> = options
                    .into_iter()
                    .map(|o| TwoFactorOption {
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
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
            if let Some(pwd) = password {
                let pwd_selector = r#"input[type="password"], input[name="Passwd"]"#;
                if element_exists(page, pwd_selector).await {
                    info!("Re-filling password after sign-in method selection");
                    let _ = fill_input_field(page, pwd_selector, pwd).await;
                    time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
                    let _ =
                        click_element(page, "#passwordNext button, #passwordNext, text:Next").await;
                    time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
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

        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
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
    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        if Instant::now() > deadline {
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

        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
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
        ftp: None,
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
    set_u32(&mut summary.ftp, "ftp");
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

    time::sleep(Duration::from_millis(config.interaction_delay_ms * 2)).await;
    extract_detail_via_js(page, provider).await
}

// ============================================================================
// Activity construction from scraped data
// ============================================================================

/// Detail-page fallback for activities the dashboard feed didn't cover.
///
/// The dashboard feed only returns the most recent ~N entries Strava
/// decides to expose; older activities (or some that simply don't carry
/// `timeAndLocation.location`) come back without city/region after the
/// feed pass. Each missing activity's detail page DOES expose location,
/// so we walk the still-empty subset and merge in city/region.
///
/// Bounded by `SCIOTTE_DETAIL_FALLBACK_MAX` (default 30) to cap worst-case
/// latency: a 200-activity scrape hitting 100 missing entries would
/// otherwise add 100 extra navigations. The cap covers what an
/// interactive coach query actually needs (recent ~30 activities) while
/// leaving older items for slower offline jobs to fill.
async fn fill_missing_locations_from_detail_pages(
    page: &chromiumoxide::Page,
    activities: &mut [Activity],
    provider: &ProviderConfig,
    config: &ScraperConfig,
) {
    let cap: usize = env::var("SCIOTTE_DETAIL_FALLBACK_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let missing: Vec<usize> = activities
        .iter()
        .enumerate()
        .filter(|(_, a)| a.city.is_none())
        .map(|(i, _)| i)
        .take(cap)
        .collect();
    if missing.is_empty() {
        return;
    }
    info!(
        missing = missing.len(),
        cap, "Detail-page location fallback for activities missing city after feed"
    );
    for idx in missing {
        let detail_url = provider.detail_url(&activities[idx].id);
        match navigate_and_extract_detail(page, &detail_url, provider, config).await {
            Ok(detail) => merge_detail_into_activity(&mut activities[idx], &detail),
            Err(e) => {
                debug!(id = %activities[idx].id, error = %e, "detail-page fallback skipped for activity");
            }
        }
    }
}

/// Augment activities with `city` / `region` from Strava's dashboard feed
/// JSON. Same-origin XHR carries cookies for free; we issue a single
/// request and merge results by activity id.
///
/// Strava strips precise GPS from list endpoints, but the dashboard feed
/// exposes `timeAndLocation.location` (e.g. "Prévost, Quebec") per recent
/// activity — enough for downstream weather geocoding. Best-effort:
/// activities outside the feed's window stay without city.
async fn enrich_activities_with_dashboard_feed(
    page: &chromiumoxide::Page,
    activities: &mut [Activity],
    target_count: usize,
) -> ScraperResult<()> {
    // Ask for at least as many feed entries as the caller wanted from the
    // training table; Strava clamps internally if we overshoot.
    let n = target_count.max(activities.len()).max(20);
    let js = format!(
        r"(async function() {{
            try {{
                const resp = await fetch('/dashboard/feed?feed_type=my_activity&athlete_id=current&num_entries={n}', {{
                    headers: {{ 'Accept': 'application/json,text/javascript,*/*; q=0.01', 'X-Requested-With': 'XMLHttpRequest' }},
                    credentials: 'same-origin',
                }});
                if (!resp.ok) return JSON.stringify({{ __error: 'http ' + resp.status }});
                const data = await resp.json();
                const out = {{}};
                (data.entries || []).forEach(function(e) {{
                    if (e.entity === 'Activity' && e.activity && e.activity.id) {{
                        const tl = e.activity.timeAndLocation || {{}};
                        if (tl.location) out[String(e.activity.id)] = tl.location;
                    }}
                }});
                return JSON.stringify(out);
            }} catch (err) {{
                return JSON.stringify({{ __error: String(err) }});
            }}
        }})()"
    );

    let result = page
        .evaluate(js)
        .await
        .map_err(|e| ScraperError::Scraping {
            reason: format!("dashboard feed evaluate failed: {e}"),
        })?;

    let body = result
        .value()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();

    let parsed: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| ScraperError::Scraping {
            reason: format!("dashboard feed JSON parse failed: {e}"),
        })?;

    if let Some(err) = parsed.get("__error").and_then(|v| v.as_str()) {
        return Err(ScraperError::Scraping {
            reason: format!("dashboard feed XHR error: {err}"),
        });
    }

    let Some(map) = parsed.as_object() else {
        return Ok(());
    };

    let mut filled = 0usize;
    for activity in activities.iter_mut() {
        if let Some(loc) = map.get(&activity.id).and_then(|v| v.as_str()) {
            // "City, Region" — split on the first comma. Strava's location
            // string is Mapbox-formatted; everything before the first comma
            // is the city, everything after is the higher-level admin area.
            if let Some((city, region)) = loc.split_once(',') {
                activity.city = Some(city.trim().to_owned());
                activity.region = Some(region.trim().to_owned());
            } else {
                activity.city = Some(loc.trim().to_owned());
            }
            filled += 1;
        }
    }

    debug!(
        feed_entries = map.len(),
        activities_total = activities.len(),
        activities_filled = filled,
        "Dashboard feed location enrichment complete"
    );

    Ok(())
}

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

/// Read a numeric-or-string JSON field as `f64`. Display-style strings
/// (e.g. "4.92 km", "150 bpm", "1,234 kcal") are parsed by stripping
/// thousand-separators / unit suffixes; raw `Number` values pass through.
fn json_field_f64(v: &serde_json::Value) -> Option<f64> {
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        let cleaned: String = s
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        if !cleaned.is_empty() {
            if let Ok(parsed) = cleaned.parse::<f64>() {
                return Some(parsed);
            }
        }
    }
    None
}

/// Build a single Activity from a JS-extracted list page row.
/// The list page provides: type, date, name, time, distance, elevation, suffer score.
fn build_activity_from_js_item(id: &str, item: &serde_json::Value) -> Activity {
    let sport_type_str = item["type"].as_str().unwrap_or("");
    let date_field = &item["date"];
    let start_date = date_field
        .as_str()
        .and_then(parse_strava_date)
        .or_else(|| {
            date_field
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
        })
        .unwrap_or_else(|| {
            // Never fabricate `now()` — a scrape-time stamp masquerades as a
            // real start and silently poisons day-attribution downstream.
            // Use the UNIX epoch as a loud, obviously-wrong sentinel and warn.
            warn!(
                id,
                date = %date_field,
                "list row exposed no parseable start_date — using UNIX_EPOCH sentinel, not now()"
            );
            chrono::DateTime::<Utc>::UNIX_EPOCH
        });

    let duration_seconds = item["time"]
        .as_str()
        .and_then(parse_duration_string)
        .or_else(|| json_field_f64(&item["time"]).map(|v| v.round() as u64))
        .unwrap_or(0);

    Activity {
        id: id.to_owned(),
        name: item["name"].as_str().unwrap_or("Untitled").to_owned(),
        sport_type: if sport_type_str.is_empty() {
            SportType::Other("Unknown".to_owned())
        } else {
            SportType::from_strava(sport_type_str)
        },
        start_date,
        duration_seconds,
        distance_meters: item["distance"]
            .as_str()
            .and_then(parse_distance_string)
            .or_else(|| json_field_f64(&item["distance"])),
        elevation_gain: json_field_f64(&item["elevation"]),
        average_heart_rate: json_field_f64(&item["avg_hr"]).map(|v| v.round() as u32),
        max_heart_rate: json_field_f64(&item["max_hr"]).map(|v| v.round() as u32),
        average_speed: None,
        max_speed: None,
        calories: json_field_f64(&item["calories"]).map(|v| v.round() as u32),
        average_power: None,
        max_power: None,
        normalized_power: None,
        average_cadence: None,
        training_stress_score: None,
        intensity_factor: None,
        suffer_score: json_field_f64(&item["suffer_score"]).map(|v| v.round() as u32),
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
        splits: None,
        laps: None,
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
            .or_else(|| {
                // The detail `js_extract` emits the activity's real UTC start as
                // an ISO-8601 zoned string (e.g. "2026-05-29T12:36:07Z"),
                // derived from the embedded activity JSON epoch — which
                // parse_strava_date's naive formats reject. Parse it as RFC3339
                // so we keep the true start time, not date-only midnight.
                data["date"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc))
            })
            .unwrap_or_else(|| {
                // Never fabricate `now()` — that scrape-time stamp is what made
                // every activity look like "today". Loud, obviously-wrong
                // sentinel instead, so a future extraction miss is visible.
                warn!(
                    id = activity_id,
                    date = %data["date"],
                    "detail page exposed no parseable start_date — using UNIX_EPOCH sentinel, not now()"
                );
                chrono::DateTime::<Utc>::UNIX_EPOCH
            }),
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
        start_latitude: data["start_latitude"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
        start_longitude: data["start_longitude"]
            .as_str()
            .and_then(|s| s.trim().parse().ok()),
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
        splits: parse_splits_from_detail(data),
        laps: parse_laps_from_detail(data),
        provider: "scraper".to_owned(),
    }
}

/// Parse a splits array returned by the detail-page JS extraction.
///
/// Expected shape (from the extended `js_extract` that looks for the
/// `splits_metric` array inside Strava's embedded activity JSON):
/// `[{"distance": f64, "elapsed_time": u64, "moving_time"?: u64,
/// "elevation_difference"?: f64, "average_speed"?: f64,
/// "average_heartrate"?: u32, "pace_zone"?: u32}, ...]`.
///
/// Missing `distance` or `elapsed_time` on an entry causes that entry
/// to be skipped — a split without either is unusable for coach reasoning.
/// Returns `None` when the array is absent or yields zero usable entries.
fn parse_splits_from_detail(data: &serde_json::Value) -> Option<Vec<Split>> {
    let arr = data.get("splits")?.as_array()?;
    let parsed: Vec<Split> = arr
        .iter()
        .enumerate()
        .filter_map(|(pos, v)| {
            let distance_meters = v.get("distance")?.as_f64()?;
            let elapsed_time_seconds = v.get("elapsed_time")?.as_u64()?;
            #[allow(clippy::cast_possible_truncation)]
            let index = v
                .get("split")
                .and_then(serde_json::Value::as_u64)
                .map_or((pos + 1) as u32, |n| n as u32);
            Some(Split {
                index,
                distance_meters,
                elapsed_time_seconds,
                moving_time_seconds: v.get("moving_time").and_then(serde_json::Value::as_u64),
                elevation_difference_meters: v
                    .get("elevation_difference")
                    .and_then(serde_json::Value::as_f64),
                average_speed_mps: v.get("average_speed").and_then(serde_json::Value::as_f64),
                #[allow(clippy::cast_possible_truncation)]
                average_heart_rate: v
                    .get("average_heartrate")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32),
                #[allow(clippy::cast_possible_truncation)]
                pace_zone: v
                    .get("pace_zone")
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32),
            })
        })
        .collect();
    (!parsed.is_empty()).then_some(parsed)
}

/// Parse a laps array returned by the detail-page JS extraction.
///
/// Expected shape mirrors Strava's embedded laps JSON (same keys as the
/// REST API's lap endpoint): `id`, `distance`, `elapsed_time`,
/// `moving_time`, `total_elevation_gain`, `average_speed`, `max_speed`,
/// `average_heartrate`, `max_heartrate`, `average_cadence`,
/// `average_watts`. Entries missing `distance` or `elapsed_time` are
/// skipped.
fn parse_laps_from_detail(data: &serde_json::Value) -> Option<Vec<Lap>> {
    let arr = data.get("laps")?.as_array()?;
    let parsed: Vec<Lap> = arr
        .iter()
        .enumerate()
        .filter_map(|(pos, v)| {
            let distance_meters = v.get("distance")?.as_f64()?;
            let elapsed_time_seconds = v.get("elapsed_time")?.as_u64()?;
            #[allow(clippy::cast_possible_truncation)]
            let index = (pos + 1) as u32;
            let id = v.get("id").and_then(|x| {
                x.as_u64()
                    .map(|n| n.to_string())
                    .or_else(|| x.as_str().map(String::from))
            });
            #[allow(clippy::cast_possible_truncation)]
            let cast_u32 = |key: &str| -> Option<u32> {
                v.get(key)
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32)
            };
            Some(Lap {
                id,
                index,
                distance_meters,
                elapsed_time_seconds,
                moving_time_seconds: v.get("moving_time").and_then(serde_json::Value::as_u64),
                elevation_gain_meters: v
                    .get("total_elevation_gain")
                    .and_then(serde_json::Value::as_f64),
                average_speed_mps: v.get("average_speed").and_then(serde_json::Value::as_f64),
                max_speed_mps: v.get("max_speed").and_then(serde_json::Value::as_f64),
                average_heart_rate: cast_u32("average_heartrate"),
                max_heart_rate: cast_u32("max_heartrate"),
                average_cadence: cast_u32("average_cadence"),
                average_power: cast_u32("average_watts"),
            })
        })
        .collect();
    (!parsed.is_empty()).then_some(parsed)
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

    if activity.start_latitude.is_none() {
        activity.start_latitude = detail["start_latitude"]
            .as_str()
            .and_then(|s| s.trim().parse().ok());
    }
    if activity.start_longitude.is_none() {
        activity.start_longitude = detail["start_longitude"]
            .as_str()
            .and_then(|s| s.trim().parse().ok());
    }

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
    let mut seen = HashSet::new();
    items.retain(|item| {
        item["id"]
            .as_str()
            .is_some_and(|id| seen.insert(id.to_owned()))
    });
}

/// Apply sport type, date range, and limit filters to an activity list
/// Page through the reverse-chronological list feed, collecting raw JS rows
/// until [`scrape_window_satisfied`] reports the request is covered (or the
/// feed ends, or the `max_scrape_pages` backstop trips). Returns the raw,
/// still-undeduplicated rows; the caller dedupes, parses, and filters.
///
/// Two stop modes:
///   - count-bounded (no `after`): page until `limit` rows are collected.
///   - date-bounded (`after` set): page until `limit` rows fall inside
///     `(after, before)` or the oldest row collected crosses at/below `after`
///     — paging further can only surface rows older than the window. This is
///     what lets a historical query reach back years instead of returning only
///     the recent feed. `max_scrape_pages` bounds a runaway "next" button.
async fn paginate_activity_list(
    page: &chromiumoxide::Page,
    js: &str,
    params: &ActivityParams,
    pagination: Option<&ListPagination>,
    max_scrape_pages: u32,
    interaction_delay_ms: u64,
) -> Vec<serde_json::Value> {
    // Fetch-based pagination (e.g. Strava): drive the list XHR with `&page=N`
    // directly so a deep `after` pages back years. Falls through to the legacy
    // "next page" button click for providers without a pagination config.
    if let Some(pagination) = pagination {
        return paginate_via_fetch(
            page,
            js,
            params,
            pagination,
            max_scrape_pages,
            interaction_delay_ms,
        )
        .await;
    }

    let target_count = params.limit.unwrap_or(20) as usize;
    let max_pages = max_scrape_pages.max(1);
    let mut all_items: Vec<serde_json::Value> = Vec::new();
    let mut in_window_count: usize = 0;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut page_count: u32 = 0;

    loop {
        page_count += 1;
        let items = match extract_via_js(page, js).await {
            Ok(items) => {
                debug!(count = items.len(), "Activities found on current page");
                items
            }
            Err(e) => {
                warn!(error = %e, "List page JS extraction failed");
                break;
            }
        };

        // Date bookkeeping only matters in date-bounded mode; skip the
        // per-page parse entirely on the common count-bounded path.
        if params.after.is_some() {
            for activity in parse_js_activity_items(&items) {
                if in_window(&activity, params) {
                    in_window_count += 1;
                }
                oldest = Some(oldest.map_or(activity.start_date, |o| o.min(activity.start_date)));
            }
        }
        all_items.extend(items);

        if scrape_window_satisfied(all_items.len(), in_window_count, oldest, params) {
            break;
        }
        if page_count >= max_pages {
            warn!(
                page_count,
                max_pages,
                collected = all_items.len(),
                "Reached max scrape pages — stopping pagination (window may be incomplete)"
            );
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
            in_window = in_window_count,
            target = target_count,
            "Loading next page of activities"
        );
        time::sleep(Duration::from_millis(interaction_delay_ms * 3)).await;
    }

    all_items
}

/// One fetched list page summarized in-page: the row count and each row's start
/// epoch (seconds). Summarizing in JS keeps the Rust loop's stop decision cheap
/// without shipping the whole JSON body across the CDP bridge every iteration —
/// the body itself is stashed in `window.__dravrCaptures` for the final extract.
struct FetchedListPage {
    count: usize,
    start_times: Vec<i64>,
}

/// Fetch one list page same-origin, stash its body into `window.__dravrCaptures`
/// (so the provider `js_extract` later maps it like a passively captured XHR),
/// and return a summary for the date-stop decision. Returns `None` on a
/// transport/HTTP error so the caller ends pagination with whatever it has.
async fn fetch_list_page(page: &chromiumoxide::Page, url: &str) -> Option<FetchedListPage> {
    let url_lit = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_owned());
    let js = format!(
        r#"(async function() {{
            const url = {url_lit};
            const meta = document.querySelector('meta[name="csrf-token"]');
            const csrf = meta ? meta.content : '';
            const ctrl = new AbortController();
            const timer = setTimeout(function() {{ ctrl.abort(); }}, 20000);
            try {{
                const resp = await fetch(url, {{
                    credentials: 'same-origin',
                    signal: ctrl.signal,
                    headers: {{
                        'x-requested-with': 'XMLHttpRequest',
                        'x-csrf-token': csrf,
                        'accept': 'text/javascript, application/javascript, application/ecmascript, application/x-ecmascript'
                    }}
                }});
                clearTimeout(timer);
                if (!resp.ok) return JSON.stringify({{ error: 'http ' + resp.status }});
                const body = await resp.text();
                window.__dravrCaptures = window.__dravrCaptures || {{}};
                window.__dravrCaptures[url] = {{ status: resp.status, body: body }};
                let models = [];
                try {{ const p = JSON.parse(body); models = (p && p.models) || []; }} catch (e) {{}}
                const starts = [];
                for (let i = 0; i < models.length; i++) {{
                    const st = models[i].start_time;
                    if (st) {{ const s = Math.floor(Date.parse(st) / 1000); if (!isNaN(s)) starts.push(s); }}
                }}
                return JSON.stringify({{ count: models.length, start_times: starts }});
            }} catch (err) {{
                clearTimeout(timer);
                return JSON.stringify({{ error: String(err) }});
            }}
        }})()"#
    );

    // Hard ceiling on the whole evaluate (CDP round-trip + in-page fetch) on top
    // of the in-page AbortController, so a stalled request can never hang the
    // backfill — and with it the single sciotte scrape permit — indefinitely.
    let result = match time::timeout(Duration::from_secs(30), page.evaluate(js)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            warn!(error = %e, url, "List page fetch evaluate failed");
            return None;
        }
        Err(_) => {
            warn!(url, "List page fetch evaluate timed out (30s)");
            return None;
        }
    };
    let body = result.value().and_then(|v| v.as_str().map(String::from))?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        warn!(url, error = err, "List page fetch returned an error");
        return None;
    }
    let count = parsed
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|c| usize::try_from(c).ok())
        .unwrap_or(0);
    let start_times = parsed
        .get("start_times")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(serde_json::Value::as_i64)
                .collect::<Vec<i64>>()
        })
        .unwrap_or_default();
    Some(FetchedListPage { count, start_times })
}

/// Page the activity list by fetching its XHR with `&page=N` directly instead of
/// clicking a "next page" button.
///
/// Each page is fetched same-origin (the browser attaches the session cookies +
/// CSRF) and stashed into `window.__dravrCaptures`, so the provider's existing
/// `js_extract` maps the accumulated rows with no duplicated field logic. The
/// Rust loop owns the stop decision — it reads each page's `start_time`s to know
/// when the feed crosses the requested `after` (date-bounded) or fills `limit`
/// (count-bounded), mirroring [`scrape_window_satisfied`]. This lets a deep
/// historical query reach back years where the legacy button-click path stalls
/// on a "next" control the page no longer exposes.
async fn paginate_via_fetch(
    page: &chromiumoxide::Page,
    js: &str,
    params: &ActivityParams,
    pagination: &ListPagination,
    max_scrape_pages: u32,
    interaction_delay_ms: u64,
) -> Vec<serde_json::Value> {
    const PAGE_PLACEHOLDER: &str = "{page}";

    let max_pages = max_scrape_pages.max(1);
    let mut in_window_count: usize = 0;
    let mut collected: usize = 0;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut pages_done: u32 = 0;
    let mut page_num = pagination.first_page;

    info!(
        first_page = page_num,
        max_pages,
        date_bounded = params.after.is_some(),
        "Starting fetch-based list pagination"
    );

    loop {
        let url = pagination
            .url_template
            .replace(PAGE_PLACEHOLDER, &page_num.to_string());
        // One retry for a transient stall/timeout (the prod datacenter IP can be
        // soft-throttled); a second failure stops with whatever we have rather
        // than spinning.
        // One retry for a transient stall/timeout (the prod datacenter IP can be
        // soft-throttled); a second failure stops with whatever we have rather
        // than spinning. A `break` in the failure path keeps this an explicit
        // `if let` (not collapsible to `map_or_else`).
        let mut summary = fetch_list_page(page, &url).await;
        if summary.is_none() {
            warn!(page = page_num, "List page fetch failed; retrying once");
            summary = fetch_list_page(page, &url).await;
        }
        let Some(summary) = summary else {
            warn!(
                page = page_num,
                collected, "List page fetch failed twice — stopping with partial results"
            );
            break;
        };
        pages_done += 1;
        if summary.count == 0 {
            // Empty page — the feed has ended.
            break;
        }
        collected += summary.count;
        for ts in summary.start_times {
            if let Some(dt) = DateTime::from_timestamp(ts, 0) {
                oldest = Some(oldest.map_or(dt, |o| o.min(dt)));
                let in_win =
                    params.after.is_none_or(|a| dt > a) && params.before.is_none_or(|b| dt < b);
                if in_win {
                    in_window_count += 1;
                }
            }
        }

        // Per-page progress: `oldest` dropping each page means pagination is
        // advancing toward the requested window; a stuck `oldest` means the
        // `page=N` fetch isn't returning older rows (it would then grind to the
        // page cap). One line per page is verbose but it is the only way to see
        // deep-history behaviour in a deployed, no-debug-logging environment.
        info!(
            page = page_num,
            count = summary.count,
            collected,
            in_window = in_window_count,
            oldest = ?oldest,
            "list page fetched"
        );

        if scrape_window_satisfied(collected, in_window_count, oldest, params) {
            break;
        }
        if pages_done >= max_pages {
            warn!(
                pages_done,
                max_pages,
                collected,
                "Reached max scrape pages — stopping fetch pagination (window may be incomplete)"
            );
            break;
        }

        page_num += 1;
        time::sleep(Duration::from_millis(interaction_delay_ms)).await;
    }

    info!(
        pages_done,
        collected,
        in_window = in_window_count,
        "Fetch pagination complete; mapping accumulated rows"
    );

    // Map every accumulated page via the provider's js_extract (it reads the
    // captures the fetches stashed). Returns the raw, still-undeduplicated rows;
    // the caller dedupes, parses, and filters.
    extract_via_js(page, js).await.unwrap_or_else(|e| {
        warn!(error = %e, "Final list extraction after fetch pagination failed");
        Vec::new()
    })
}

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

    activities.retain(|a| in_window(a, params));

    if let Some(limit) = params.limit {
        activities.truncate(limit as usize);
    }
}

/// Whether an activity falls inside the requested `(after, before)` date
/// window. The bounds are strict and either side may be unset. This is the
/// single source of truth shared by the paginate-to-date stop condition and
/// the final [`apply_activity_filters`] pass, so the loop never stops short of
/// — or pages past — what the filter will ultimately keep.
fn in_window(a: &Activity, params: &ActivityParams) -> bool {
    params.after.is_none_or(|after| a.start_date > after)
        && params.before.is_none_or(|before| a.start_date < before)
}

/// Pure stop decision for the list-page pagination loop.
///
/// `collected_len` is every row gathered so far; `in_window_count` and
/// `oldest` are the running tallies the date-bounded loop maintains over the
/// reverse-chronological feed. Returns `true` when pagination should stop:
///   - no `after` (count-bounded): once `limit` rows are collected.
///   - `after` set (date-bounded): once `limit` rows fall inside the window
///     (the newest in-window slice the caller asked for) or the oldest row
///     seen has crossed at/below `after` (nothing older can be in the window).
fn scrape_window_satisfied(
    collected_len: usize,
    in_window_count: usize,
    oldest: Option<DateTime<Utc>>,
    params: &ActivityParams,
) -> bool {
    let limit = params.limit.unwrap_or(20) as usize;
    params.after.map_or(collected_len >= limit, |after| {
        in_window_count >= limit || oldest.is_some_and(|o| o <= after)
    })
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

    /// `YYYY-MM-DD` -> midnight UTC, for building deterministic test dates.
    fn day(s: &str) -> DateTime<Utc> {
        parse_strava_date(s).expect("test date parses") // Safe: test fixture
    }

    fn params_window(after: Option<&str>, before: Option<&str>, limit: u32) -> ActivityParams {
        ActivityParams {
            limit: Some(limit),
            after: after.map(day),
            before: before.map(day),
            ..Default::default()
        }
    }

    fn activity_on(date: &str) -> Activity {
        build_activity_from_js_item(
            "t",
            &serde_json::json!({ "name": "x", "type": "Run", "date": date }),
        )
    }

    #[test]
    fn in_window_respects_strict_bounds() {
        let p = params_window(Some("2022-01-01"), Some("2023-01-01"), 20);
        assert!(
            in_window(&activity_on("2022-06-15"), &p),
            "inside the window"
        );
        assert!(
            !in_window(&activity_on("2024-01-01"), &p),
            "newer than before-bound"
        );
        assert!(
            !in_window(&activity_on("2021-06-15"), &p),
            "older than after-bound"
        );
    }

    #[test]
    fn in_window_open_bounds() {
        let after_only = params_window(Some("2022-01-01"), None, 20);
        assert!(in_window(&activity_on("2026-01-01"), &after_only));
        assert!(!in_window(&activity_on("2020-01-01"), &after_only));

        let unbounded = params_window(None, None, 20);
        assert!(in_window(&activity_on("1999-01-01"), &unbounded));
    }

    #[test]
    fn satisfied_count_mode_stops_at_limit() {
        let p = params_window(None, None, 20);
        assert!(!scrape_window_satisfied(19, 0, None, &p), "under limit");
        assert!(scrape_window_satisfied(20, 0, None, &p), "reached limit");
    }

    #[test]
    fn satisfied_date_mode_stops_when_enough_in_window() {
        let p = params_window(Some("2022-01-01"), Some("2023-01-01"), 20);
        // 500 collected but only 19 inside the window, oldest still inside ->
        // keep paging to gather the caller's full newest slice.
        assert!(!scrape_window_satisfied(
            500,
            19,
            Some(day("2022-03-01")),
            &p
        ));
        // 20 inside the window -> we have the newest slice asked for, stop.
        assert!(scrape_window_satisfied(
            500,
            20,
            Some(day("2022-03-01")),
            &p
        ));
    }

    #[test]
    fn satisfied_date_mode_stops_when_oldest_crosses_after() {
        let p = params_window(Some("2022-01-01"), Some("2023-01-01"), 400);
        // Far fewer than `limit` in-window, but the oldest row collected sits
        // below `after` (in 2021) -> nothing older can qualify, stop.
        assert!(scrape_window_satisfied(
            5000,
            30,
            Some(day("2021-12-01")),
            &p
        ));
        // Oldest still inside the window and under the limit -> keep paging.
        assert!(!scrape_window_satisfied(
            5000,
            30,
            Some(day("2022-02-01")),
            &p
        ));
    }

    #[test]
    fn build_detail_extracts_start_lat_lng_from_string_fields() {
        // The JS extractor emits start_latitude/start_longitude as strings
        // (every other field uses string-of-number for parser symmetry).
        let raw = serde_json::json!({
            "name": "Cold ski",
            "type": "NordicSki",
            "date": "2025-12-15",
            "moving_time": "1:30:00",
            "distance": "10.0 km",
            "start_latitude": "45.5017",
            "start_longitude": "-73.5673",
        });
        let activity = build_activity_from_detail("12345", &raw);
        let lat = activity.start_latitude.unwrap(); // Safe: test fed string-of-number
        let lng = activity.start_longitude.unwrap(); // Safe: test fed string-of-number
        assert!((lat - 45.5017).abs() < 1e-4, "expected ~45.5017, got {lat}");
        assert!(
            (lng - -73.5673).abs() < 1e-4,
            "expected ~-73.5673, got {lng}"
        );
    }

    #[test]
    fn merge_detail_fills_start_lat_lng_when_list_was_empty() {
        // List page doesn't surface coords; detail page should fill them in.
        let mut activity = build_activity_from_js_item(
            "9999",
            &serde_json::json!({
                "name": "Cold ski",
                "type": "NordicSki",
                "date": "2025-12-15",
                "time": "1:30:00",
                "distance": "10.0 km",
            }),
        );
        assert!(
            activity.start_latitude.is_none(),
            "list page must not preempt"
        );

        merge_detail_into_activity(
            &mut activity,
            &serde_json::json!({
                "start_latitude": "45.5017",
                "start_longitude": "-73.5673",
            }),
        );

        let lat = activity.start_latitude.unwrap(); // Safe: merged from test JSON
        let lng = activity.start_longitude.unwrap(); // Safe: merged from test JSON
        assert!((lat - 45.5017).abs() < 1e-4);
        assert!((lng - -73.5673).abs() < 1e-4);
    }

    #[test]
    fn parse_distance() {
        let d = parse_distance_string("5.2 km").unwrap(); // Safe: test with valid distance string
        assert!((d - 5200.0).abs() < 1.0);

        let d = parse_distance_string("3.1 mi").unwrap(); // Safe: test with valid distance string
        assert!((d - 4988.967).abs() < 1.0);

        let d = parse_distance_string("800m").unwrap(); // Safe: test with valid distance string
        assert!((d - 800.0).abs() < 1.0);
    }

    #[test]
    fn parse_speed() {
        let s = parse_speed_string("10 km/h").unwrap(); // Safe: test with valid speed string
        assert!((s - 2.7778).abs() < 0.01);

        let s = parse_speed_string("6.2 mph").unwrap(); // Safe: test with valid speed string
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
        let provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
        let selectors = LoginSelectors::from_provider(&provider).unwrap(); // Safe: test with valid default provider
        assert!(!selectors.email.is_empty());
        assert!(!selectors.password.is_empty());
        assert!(!selectors.button.is_empty());
    }

    #[test]
    fn login_selectors_from_provider_missing_email() {
        let mut provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
        provider.provider.login_email_selector = None;
        let result = LoginSelectors::from_provider(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("login_email_selector"));
    }

    #[test]
    fn login_selectors_from_provider_missing_password() {
        let mut provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
        provider.provider.login_password_selector = None;
        let result = LoginSelectors::from_provider(&provider);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("login_password_selector"));
    }

    #[test]
    fn login_selectors_from_provider_missing_button() {
        let mut provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
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
        let options: Vec<TwoFactorOptionWithCoords> = serde_json::from_str(json).unwrap(); // Safe: test with valid JSON
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].id, "otp");
        assert!((options[0].x - 150.5).abs() < 0.01);
        assert_eq!(options[1].id, "sms");
    }

    #[test]
    fn two_fa_option_with_coords_empty_json() {
        let options: Vec<TwoFactorOptionWithCoords> = serde_json::from_str("[]").unwrap(); // Safe: test with valid empty JSON array
        assert!(options.is_empty());
    }

    #[test]
    fn two_fa_option_serializes_to_json() {
        let option = TwoFactorOption {
            id: "otp".to_owned(),
            label: "Google Authenticator".to_owned(),
        };
        let json = serde_json::to_string(&option).unwrap(); // Safe: test with serializable struct
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
        let provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
        assert!(provider.provider.login_oauth_buttons.contains_key("google"));
        assert!(provider.provider.login_oauth_buttons.contains_key("apple"));
    }

    #[test]
    fn strava_provider_has_otp_selector() {
        let provider = ProviderConfig::strava_default().unwrap(); // Safe: test fixture
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
        let provider = ProviderConfig::garmin_default().unwrap(); // Safe: test fixture
        assert!(provider.provider.profile_url.is_some());
        assert!(provider.provider.profile_js_extract.is_some());
    }
}

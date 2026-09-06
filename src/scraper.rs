// ABOUTME: Chromiumoxide-based sport activity scraper driven by TOML provider configs
// ABOUTME: Implements ActivityScraper trait using headless Chrome via CDP with configurable selectors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::cmp::Reverse;
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
use chrono::{DateTime, Datelike, NaiveDateTime, Utc};
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
    Activity, ActivityParams, AthleteProfile, AuthSession, DailySummary, HealthParams, Lap,
    RouteBounds, RouteTrack, Split, SportType,
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
///
/// `challenge/number` is the phone-tap approval page shown *after* a method is
/// chosen. It carries no `[data-challengetype]` elements at all, so parsing it for
/// options polls the full settle budget to conclude what its URL already said, then
/// clicks an "Enter your password" link that is not there. Every phone-tap login paid
/// that, and logged a "no options matched" warning on each 200ms poll while it did.
const CHALLENGE_SKIP_PATTERNS: &[&str] = &[
    "challenge/pk",
    "challenge/pwd",
    "challenge/dp",
    "challenge/number",
];

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
    // Google's identifier field is `type="text"`, not `type="email"` — its stable
    // handles are the id and name. Matching only on type meant the field was present
    // and visible on a legitimate "Sign in to continue to Strava" page while the fill
    // reported "Element not found", which reads as a blocked or changed page rather
    // than a stale selector (seen 2026-08-17). Type is kept last as a fallback.
    email: r#"#identifierId, input[name="identifier"], input[type="email"]"#,
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
    /// The provider name this scraper serves (e.g. `"garmin"`, `"strava"`).
    #[must_use]
    pub fn provider_name(&self) -> &str {
        &self.provider.provider.name
    }

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

    /// Log what the page actually is when an OAuth field cannot be found.
    ///
    /// Without this an OAuth failure says only "Element not found for selector", which
    /// cannot distinguish the provider's button click never navigating, the OAuth page
    /// changing its markup, or the provider serving an interstitial instead.
    async fn log_oauth_page_forensics(page: &chromiumoxide::Page, method: &str, selector: &str) {
        let state = page
        .evaluate(
            r"(function() {
                var inputs = Array.from(document.querySelectorAll('input'))
                    .filter(function(i) { return i.offsetParent !== null; })
                    .map(function(i) { return i.type + '|name=' + i.name + '|id=' + i.id; });
                return JSON.stringify({
                    url: location.href,
                    title: document.title,
                    visible_inputs: inputs.slice(0, 12),
                    text: (document.body ? document.body.innerText : '').replace(/\s+/g, ' ').slice(0, 300)
                });
            })()",
        )
        .await
        .ok()
        .and_then(|r| r.value().and_then(|v| v.as_str().map(String::from)))
        .unwrap_or_else(|| "<page unreadable>".to_owned());
        warn!(method, selector, page_state = %state, "OAuth login failure forensics");
    }

    /// Log a forensic snapshot of the page after a selector login attempt fails.
    ///
    /// The intermittent selector failure (`element-not-found` / step timeout)
    /// has several indistinguishable causes — an under-rendered page, a
    /// bot-challenge, a changed provider login page, or a pure timing blip — and
    /// the error alone cannot tell them apart. This captures the page's actual
    /// state (URL, title, `readyState`, body length, challenge markers, and
    /// whether the expected selectors are present) so a future diagnosis reads
    /// ground truth instead of inferring it. Text-only, so it survives in the
    /// Cloud Run log stream (unlike the `/tmp` screenshot, which dies with the
    /// pod). Best-effort — never affects the login result.
    async fn log_selector_login_failure_forensics(
        &self,
        page: &chromiumoxide::Page,
        attempt: u8,
        reason: &str,
    ) {
        let (email_selector_present, password_selector_present) =
            match LoginSelectors::from_provider(&self.provider) {
                Ok(s) => (
                    element_exists(page, s.email).await,
                    element_exists(page, s.password).await,
                ),
                Err(_) => (false, false),
            };
        let page_state = page
            .evaluate(
                r#"(function(){var b=(document.body&&document.body.innerText)||"";var low=b.toLowerCase();var sigs=["just a moment","cloudflare","captcha","verify","are you human","unusual traffic","access denied","rate limit","too many requests","challenge","enable javascript","robot"];var markers=sigs.filter(function(m){return low.indexOf(m)>-1;});return JSON.stringify({url:location.href,title:document.title,ready:document.readyState,body_len:b.length,markers:markers,body_head:b.slice(0,200).replace(/\s+/g," ").trim()});})()"#,
            )
            .await
            .ok()
            .and_then(|r| r.value().and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_else(|| "<page eval failed>".to_owned());
        warn!(
            provider = %self.provider.provider.name,
            attempt,
            reason,
            email_selector_present,
            password_selector_present,
            page_state = %page_state,
            "Selector login failure forensics"
        );
    }

    /// Run the selector-based direct login, retrying once after a fresh page
    /// reload before the caller falls back to vision.
    ///
    /// The selector step fails intermittently (`element-not-found` / step
    /// timeout), the same transient class the list and interval fetch paths
    /// already absorb with a single retry; a fresh render clears it far more
    /// cheaply than a full vision round-trip. Each failed attempt logs a
    /// forensic page snapshot (see [`Self::log_selector_login_failure_forensics`])
    /// so the actual cause is observed, not inferred.
    async fn run_direct_credential_login_with_retry(
        &self,
        page: &chromiumoxide::Page,
        login_url: &str,
        email: &str,
        password: &str,
    ) -> ScraperResult<LoginResult> {
        match self
            .run_direct_credential_login(page, email, password)
            .await
        {
            Err(e) => {
                self.log_selector_login_failure_forensics(page, 1, &e.to_string())
                    .await;
                warn!(error = %e, "Selector login failed; reloading page and retrying once");
                if let Err(nav) = page.goto(login_url).await {
                    warn!(error = %nav, "Reload before selector retry failed");
                }
                time::sleep(Duration::from_secs(self.config.page_load_wait_secs)).await;
                dismiss_cookie_dialog(page).await;
                let retry = self
                    .run_direct_credential_login(page, email, password)
                    .await;
                if let Err(ref e2) = retry {
                    self.log_selector_login_failure_forensics(page, 2, &e2.to_string())
                        .await;
                }
                retry
            }
            other => other,
        }
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
        if let Err(e) = fill_input_field(page, oauth_form.email, email).await {
            // The selector-login path logs forensics on a missing field; this one did
            // not, so an OAuth failure reported only the selector name and left no way
            // to tell "the hop did not happen" from "the provider changed its page".
            Self::log_oauth_page_forensics(page, method, oauth_form.email).await;
            return Err(e);
        }
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
                self.run_direct_credential_login_with_retry(&page, &login_url, email, password)
                    .await
            }
        };

        // Hybrid mode: after the selector login (incl. its one reload-retry) still
        // fails, fall back to vision.
        #[cfg(feature = "vision")]
        if matches!(config.login_mode, LoginMode::Hybrid) {
            if let Err(ref e) = result {
                warn!(error = %e, "Selector login failed after retry, falling back to vision mode");
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
            // No number means this is Google's plain tap-Yes prompt, so wait for the
            // approval here. The generic poll below would hand this page to the
            // device-prompt handler, which clicks "Try another way" and navigates away
            // from the prompt the user is approving.
            wait_for_phone_approval(
                &page,
                config,
                Duration::from_secs(config.phone_tap_timeout_secs),
            )
            .await;
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

        // Numeric athlete id for the `/athletes/{id}/graph_date_range` interval
        // scrape (Strava training-log). `None` for providers/pages that don't
        // expose it — the interval path then yields nothing; the page-based and
        // click paths don't use it, so they are unaffected.
        let athlete_id = resolve_athlete_id(&page).await;
        if let Some(id) = &athlete_id {
            info!(athlete_id = %id, "Resolved athlete id");
        } else {
            debug!("Athlete id unresolved (non-interval provider or page)");
        }

        let target_count = params.limit.unwrap_or(20) as usize;
        let js = self.provider.list_extraction_js();
        let mut all_items = paginate_activity_list(
            &page,
            &js,
            athlete_id.as_deref(),
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

/// Poll `parse_two_fa_options` until the page yields options or `budget` elapses.
///
/// The parser drops any element whose `getBoundingClientRect()` is still
/// zero-sized, so a genuine 2FA chooser sampled before layout completes is
/// indistinguishable from a page carrying no 2FA options at all. Sampling once
/// after a fixed sleep made that a race against how fast the runner lays out the
/// page — and losing it sends the caller down the sign-in-method-chooser branch,
/// which clicks "Enter your password" and navigates away from a page it misread.
///
/// Polling is what makes a generous `budget` affordable: it is abandoned the
/// moment options appear, so a real 2FA chooser pays only the latency it needs
/// while a slow runner gets many more chances than a single sample allowed.
async fn poll_two_fa_options(
    page: &chromiumoxide::Page,
    config: &ScraperConfig,
    budget: Duration,
) -> Vec<TwoFactorOptionWithCoords> {
    let deadline = Instant::now() + budget;
    loop {
        let options = parse_two_fa_options(page).await;
        if !options.is_empty() {
            return options;
        }
        if Instant::now() >= deadline {
            debug!(
                budget_secs = budget.as_secs(),
                "No 2FA options after polling the full budget — treating as a sign-in method chooser"
            );
            return Vec::new();
        }
        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    }
}

/// How long to keep re-parsing the challenge selection page before concluding it
/// carries no 2FA options.
///
/// Deliberately far larger than `page_load_wait_secs`. That value has to stay
/// small because it is paid as an unconditional sleep on every step; this one is
/// a *ceiling* that [`poll_two_fa_options`] abandons the moment options appear,
/// so a page that is a 2FA chooser never pays it. Only a genuine
/// sign-in-method chooser waits the full budget, and delaying that rare branch a
/// few seconds is far cheaper than misreading a 2FA page and navigating away
/// from it — the failure that flaked `google_oauth_2fa_number_match` twice.
const TWO_FA_OPTIONS_SETTLE_BUDGET: Duration = Duration::from_secs(8);

/// Outcome of one poll iteration on Google's challenge selection page.
enum ChallengeSelectionOutcome {
    /// Real 2FA options were found — return this to the caller.
    Resolved(LoginResult),
    /// Still on the chooser (or the one-shot password recovery just fired) —
    /// the poll loop should `continue`.
    KeepPolling,
}

/// Handle a single poll iteration on Google's challenge selection page.
///
/// The page is either a 2FA chooser or a sign-in-method chooser, and the two are
/// told apart by whether any `[data-challengetype]` element parses. That test is
/// only meaningful once layout has given those elements a non-zero rect, hence
/// the polling in [`poll_two_fa_options`].
async fn handle_challenge_selection(
    page: &chromiumoxide::Page,
    config: &ScraperConfig,
    password: Option<&str>,
    tried_enter_password: &mut bool,
) -> ChallengeSelectionOutcome {
    let options = poll_two_fa_options(page, config, TWO_FA_OPTIONS_SETTLE_BUDGET).await;
    if !options.is_empty() {
        // Real 2FA options found — return to caller for orchestration
        let choices: Vec<TwoFactorOption> = options
            .into_iter()
            .map(|o| TwoFactorOption {
                id: o.id,
                label: o.label,
            })
            .collect();
        return ChallengeSelectionOutcome::Resolved(LoginResult::TwoFactorChoice(choices));
    }

    // No 2FA options — treat as the sign-in method chooser: click "Enter your
    // password", re-fill, submit.
    //
    // Latched like `tried_another_way`: the click navigates away, so firing it
    // on every poll of a page whose options never became measurable would keep
    // re-submitting the password against whatever we landed on. Once is a
    // recovery attempt; repeatedly is a loop.
    if *tried_enter_password {
        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
        return ChallengeSelectionOutcome::KeepPolling;
    }
    *tried_enter_password = true;

    info!("No 2FA options found, clicking 'Enter your password'");
    cdp_click_enter_password(page).await;
    time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
    if let Some(pwd) = password {
        let pwd_selector = r#"input[type="password"], input[name="Passwd"]"#;
        if element_exists(page, pwd_selector).await {
            info!("Re-filling password after sign-in method selection");
            let _ = fill_input_field(page, pwd_selector, pwd).await;
            time::sleep(Duration::from_millis(config.form_interaction_delay_ms)).await;
            let _ = click_element(page, "#passwordNext button, #passwordNext, text:Next").await;
            time::sleep(Duration::from_secs(config.page_load_wait_secs)).await;
        }
    }
    ChallengeSelectionOutcome::KeepPolling
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

/// Log a page navigation during credential-login polling, updating `last_url`.
/// The breadcrumb trail makes a hang show which pages the provider walked us
/// through before stalling.
fn log_login_nav(last_url: &mut String, url: &str) {
    if url != last_url {
        info!(from = %last_url, to = %url, "Credential login: page navigated");
        url.clone_into(last_url);
    }
}

/// Surface a no-navigation stall (throttled to every 15s): the submit fired but
/// the page never left the login form — the signature of the provider blocking
/// the (headless) browser or serving an inline challenge our selectors miss.
fn log_login_stall(last_stall_log: &mut Instant, url: &str, started: Instant) {
    if last_stall_log.elapsed() >= Duration::from_secs(15) {
        warn!(
            url = %url,
            elapsed_secs = started.elapsed().as_secs(),
            "Credential login still on the initial login page — no navigation after submit \
             (provider may be blocking the browser or showing an inline challenge)"
        );
        *last_stall_log = Instant::now();
    }
}

/// Build the credential-login timeout error, naming the last page reached so the
/// failure is diagnosable instead of a contextless "timed out".
fn credential_login_timeout_error(last_url: &str, started: Instant) -> ScraperError {
    let elapsed = started.elapsed().as_secs();
    warn!(
        last_url = %last_url,
        elapsed_secs = elapsed,
        "Credential login timed out — page never reached OTP or a success URL"
    );
    ScraperError::Auth {
        reason: format!("Credential login timed out after {elapsed}s — last page: {last_url}"),
    }
}

/// Hold on Google's plain "tap Yes" prompt until the user approves it.
///
/// That prompt carries no number to match — it just says "open the Gmail app and tap
/// Yes" — so the number-match path does not apply, and falling through to the poll loop
/// hands the page to the device-prompt handler, which clicks "Try another way". That
/// navigates away from the prompt the user is in the middle of approving and returns
/// them to the chooser they just answered, so the login can never complete no matter how
/// promptly they tap. Observed live on 2026-08-17.
///
/// Escaping is right when we reach the prompt without the user having asked for it; it
/// is wrong once they have chosen the phone. Returns true when the prompt clears.
async fn wait_for_phone_approval(
    page: &chromiumoxide::Page,
    config: &ScraperConfig,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        let url = page.url().await.ok().flatten().unwrap_or_default();

        // Leaving Google's challenge flow entirely is the approval landing. Waiting on
        // "no longer the device prompt" instead does not work: selecting the method does
        // not navigate to the prompt immediately, so that condition is already true on
        // the chooser and the wait returns before the prompt even appears — which is how
        // this reported an approval six seconds in, before anyone could tap.
        if !url.contains(CHALLENGE_URL_PATTERN) {
            info!(url = %url, "Phone approval received — left the challenge flow");
            return true;
        }

        // A code-entry page instead of an approval: nothing to wait for, the caller
        // reports OtpRequired. `challenge/dp` is excluded because "dp" is the prompt
        // itself, not a code page.
        if !url.contains(DEVICE_PROMPT_PATTERN) && path_contains_any(&url, OTP_URL_PATTERNS) {
            info!(url = %url, "Code entry page appeared instead of a phone approval");
            return false;
        }

        if Instant::now() >= deadline {
            warn!(
                budget_secs = budget.as_secs(),
                url = %url,
                "No phone approval within the budget — falling back to the other methods"
            );
            return false;
        }
        time::sleep(Duration::from_millis(config.login_poll_interval_ms)).await;
    }
}

/// Whether the poll loop should keep waiting for the password submit to navigate.
///
/// `initial_url` is sampled when the poll begins, which races the navigation that
/// submitting the password kicked off. Win that race and the initial URL is the
/// password page, so the challenge branches downstream run as intended. Lose it —
/// slower headless Chrome, a loaded runner, any delay between the click and the
/// first sample — and a challenge page *is* the initial URL. Treating "URL
/// unchanged" as "hasn't navigated yet" then skips those branches on every
/// iteration, and the login dies at its deadline sitting on a fully rendered 2FA
/// chooser it was never allowed to read.
///
/// Being on a challenge page is itself proof the submit navigated, which is the only
/// thing this guard exists to establish.
fn awaiting_post_submit_navigation(url: &str, initial_url: &str) -> bool {
    url == initial_url && !url.contains(CHALLENGE_URL_PATTERN)
}

/// Whether an OTP page reached right now should be reported to the caller.
///
/// Two kinds of caller ask opposite questions of this one loop. After submitting a
/// password (`after_password`), arriving at an OTP page **is** the answer. After
/// submitting a code or picking a 2FA method, the loop *starts* on the OTP page and
/// must wait for it to redirect away — reporting it there would ask for the same code
/// forever. The `password` argument already encodes which caller we are.
///
/// `url != initial_url` alone separated them only by luck: it assumes a password
/// submit never finishes navigating before `initial_url` is sampled. Losing that race
/// makes the OTP page the initial URL, and the password caller then never reports
/// `OtpRequired` at all — it polls to its deadline on the code entry form.
/// Reproduced in 2 of 6 runs before this guard existed.
fn otp_page_should_be_reported(url: &str, initial_url: &str, after_password: bool) -> bool {
    path_contains_any(url, OTP_URL_PATTERNS) && (url != initial_url || after_password)
}

/// Whether `url` is a challenge page worth parsing for 2FA options.
///
/// Every challenge page except the chooser has a dedicated branch or no options at
/// all, and parsing one of those costs the full settle budget to learn what the URL
/// already said.
fn is_two_fa_chooser_url(url: &str) -> bool {
    url.contains(CHALLENGE_URL_PATTERN) && !CHALLENGE_SKIP_PATTERNS.iter().any(|p| url.contains(p))
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
    // Latches the one-shot "Enter your password" recovery on the challenge
    // selection page; see the call site for why it must not repeat.
    let mut tried_enter_password = false;
    // Counts consecutive polls spent stuck on /challenge/dp after the single
    // "Try another way" click failed to navigate. Bounded so Hybrid login can
    // escalate to vision quickly instead of polling to the full timeout.
    let mut stuck_polls: u32 = 0;

    // Capture the initial URL so we can detect when the page actually changes
    let initial_url = page.url().await.ok().flatten().unwrap_or_default();
    debug!(initial_url = %initial_url, "Polling for login result");
    // Observability for the silent-hang class: track the last page so every
    // navigation is logged as a breadcrumb, a no-navigation stall is surfaced
    // periodically (not just at the final timeout), and the timeout error names
    // the page we died on. Without this a blocked/challenged login is a silent
    // multi-minute spin with a contextless "timed out".
    let started = Instant::now();
    let mut last_url = initial_url.clone();
    let mut last_stall_log = Instant::now();

    loop {
        if Instant::now() > deadline {
            save_timeout_screenshot(page, "login-timeout").await;
            return Err(credential_login_timeout_error(&last_url, started));
        }

        let url = page
            .url()
            .await
            .map_err(|e| ScraperError::Browser {
                reason: format!("Failed to get page URL: {e}"),
            })?
            .unwrap_or_default();

        // Breadcrumb: log every real navigation so a hang shows the trail of
        // pages the provider walked us through before stalling.
        log_login_nav(&mut last_url, &url);

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

        // Report an OTP page to the caller that submitted a password; keep waiting for
        // a redirect when we started on one after submitting a code.
        if otp_page_should_be_reported(&url, &initial_url, password.is_some()) {
            info!(url = %url, "OTP/2FA page detected");
            return Ok(LoginResult::OtpRequired);
        }

        // Wait for the submit to navigate before reading the page as a challenge.
        if awaiting_post_submit_navigation(&url, &initial_url) {
            log_login_stall(&mut last_stall_log, &url, started);
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
        if is_two_fa_chooser_url(&url) {
            match handle_challenge_selection(page, config, password, &mut tried_enter_password)
                .await
            {
                ChallengeSelectionOutcome::Resolved(result) => return Ok(result),
                ChallengeSelectionOutcome::KeepPolling => continue,
            }
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
    // Track the last page so the timeout names where we stalled (see the
    // matching breadcrumb logic in poll_credential_login_result).
    let mut last_url = String::new();

    loop {
        if Instant::now() > deadline {
            warn!(last_url = %last_url, "Login timed out — never reached a success URL");
            return Err(ScraperError::Auth {
                reason: format!(
                    "Login timed out after {timeout} seconds (last page: {last_url}) — \
                     close the browser and retry"
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

        if url != last_url {
            info!(from = %last_url, to = %url, "Login wait: page navigated");
            last_url = url.clone();
        }

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
    debug!(raw = %json_str, "js_extract raw result");

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
            // Strava CamelCase vocabulary first; Garmin emits disjoint snake_case
            // typeKeys (trail_running, gravel_cycling, …), so fall back to the
            // Garmin mapper when Strava leaves it Other. The raw key survives only
            // when neither vocabulary recognizes it.
            match SportType::from_strava(sport_type_str) {
                SportType::Other(_) => SportType::from_garmin(sport_type_str),
                mapped => mapped,
            }
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
        // The list page carries no track; the detail pass fills this in.
        route: None,
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
            |s| match SportType::from_strava(s) {
                SportType::Other(_) => SportType::from_garmin(s),
                mapped => mapped,
            },
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
        // Garmin's detail js_extract emits `distance` as a bare number (meters);
        // Strava emits a unit-bearing display string. Try the string parser first,
        // then fall back to the raw numeric so the Garmin detail path keeps it.
        distance_meters: data["distance"]
            .as_str()
            .and_then(parse_distance_string)
            .or_else(|| json_field_f64(&data["distance"])),
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
        route: parse_route_from_detail(data),
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
/// Build a [`RouteTrack`] from the detail extract's `route` object.
///
/// The three series must stay index-aligned, so a coordinate pair that fails to
/// parse drops the sample from every array at once rather than shortening one.
/// `altitudes_meters` and `distances_meters` are kept only when the provider
/// supplied a complete series — a partially-null elevation array cannot drive
/// climb detection, and a caller reading a padded one cannot tell which samples
/// were real.
///
/// Returns `None` for a track of fewer than two points: a single coordinate is
/// the activity's start, which [`Activity::start_latitude`] already carries.
fn parse_route_from_detail(data: &serde_json::Value) -> Option<RouteTrack> {
    let route = data.get("route")?;
    let raw = route.get("coordinates")?.as_array()?;

    let mut coordinates = Vec::with_capacity(raw.len());
    let mut keep = Vec::with_capacity(raw.len());
    for (idx, pair) in raw.iter().enumerate() {
        let pair = pair.as_array()?;
        let (Some(lat), Some(lon)) = (
            pair.first().and_then(serde_json::Value::as_f64),
            pair.get(1).and_then(serde_json::Value::as_f64),
        ) else {
            continue;
        };
        if !lat.is_finite() || !lon.is_finite() {
            continue;
        }
        coordinates.push((lat, lon));
        keep.push(idx);
    }
    if coordinates.len() < 2 {
        return None;
    }

    let expected = coordinates.len();
    let series = |key: &str| -> Option<Vec<f64>> {
        let arr = route.get(key)?.as_array()?;
        let picked: Vec<f64> = keep
            .iter()
            .filter_map(|&i| arr.get(i).and_then(serde_json::Value::as_f64))
            .filter(|v| v.is_finite())
            .collect();
        (picked.len() == expected).then_some(picked)
    };

    let altitudes_meters = series("altitudes");
    let distances_meters = series("distances");

    let bounds = route.get("bounds").and_then(|b| {
        Some(RouteBounds {
            min_latitude: b.get("min_latitude")?.as_f64()?,
            max_latitude: b.get("max_latitude")?.as_f64()?,
            min_longitude: b.get("min_longitude")?.as_f64()?,
            max_longitude: b.get("max_longitude")?.as_f64()?,
        })
    });

    Some(RouteTrack {
        coordinates,
        altitudes_meters,
        distances_meters,
        bounds,
    })
}

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

    // The list page never carries a track, so the detail page is the only
    // source and there is nothing to preempt. Merged here as well as in
    // `build_activity_from_detail` because the two paths do not share code: an
    // enriched list scrape reaches only this function, and a field wired into
    // the builder alone is silently absent from every multi-activity result.
    if activity.route.is_none() {
        activity.route = parse_route_from_detail(detail);
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

/// A year-month cursor driving Strava's training-log interval navigation.
///
/// The training log pages activities a month at a time via
/// `graph_date_range?interval_type=month&interval=YYYYMM&year_offset=N`, where
/// `year_offset` is how many calendar years back the interval's year sits from
/// the current year. Stepping the cursor back one month at a time walks the
/// reverse-chronological history a whole month per request (~12 fetches a year)
/// instead of ~20 activities per page — so a deep historical window is reached
/// in a dozen same-origin fetches rather than a hundred Chrome pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntervalCursor {
    year: i32,
    month: u32,
}

impl IntervalCursor {
    /// Cursor for the calendar month containing `dt`.
    fn containing(dt: DateTime<Utc>) -> Self {
        Self {
            year: dt.year(),
            month: dt.month(),
        }
    }

    /// Strava's `interval` value for `interval_type=month`: `YYYYMM`.
    fn interval_param(self) -> String {
        format!("{:04}{:02}", self.year, self.month)
    }

    /// `year_offset` = current calendar year − this interval's year (never negative).
    fn year_offset(self, now_year: i32) -> u32 {
        u32::try_from(now_year - self.year).unwrap_or(0)
    }

    /// The previous calendar month (rolls the year over at January).
    fn prev_month(self) -> Self {
        if self.month <= 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    /// Whether this month sits entirely before `after`'s month — i.e. stepping
    /// back to it would only surface activities older than the requested window,
    /// so the date-bounded walk can stop.
    fn is_before_month_of(self, after: DateTime<Utc>) -> bool {
        (self.year, self.month) < (after.year(), after.month())
    }
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
    athlete_id: Option<&str>,
    params: &ActivityParams,
    pagination: Option<&ListPagination>,
    max_scrape_pages: u32,
    interaction_delay_ms: u64,
) -> Vec<serde_json::Value> {
    // Fetch-based pagination (e.g. Strava): drive the list XHR with `&page=N`
    // directly so a deep `after` pages back years. Falls through to the legacy
    // "next page" button click for providers without a pagination config.
    if let Some(pagination) = pagination {
        // Interval mode (Strava training-log): the url_template carries the
        // `{interval}` placeholder and the scrape walks a calendar month at a
        // time via `graph_date_range` — a dozen fetches reach a deep year vs ~100
        // `&page=N` pages, so the walk completes before a serverless instance is
        // reclaimed. Needs the numeric athlete id for the `/athletes/{id}/` URL.
        if pagination.url_template.contains("{interval}") {
            let Some(aid) = athlete_id else {
                warn!("interval url_template configured but athlete id unresolved — no activities");
                return Vec::new();
            };
            return paginate_via_interval(
                page,
                js,
                aid,
                params,
                pagination,
                max_scrape_pages,
                interaction_delay_ms,
            )
            .await;
        }
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
async fn fetch_list_page(
    page: &chromiumoxide::Page,
    url: &str,
    csrf_header: &str,
    accept: &str,
) -> Option<FetchedListPage> {
    let url_lit = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_owned());
    let csrf_header_lit =
        serde_json::to_string(csrf_header).unwrap_or_else(|_| "\"x-csrf-token\"".to_owned());
    let accept_lit = serde_json::to_string(accept).unwrap_or_else(|_| "\"*/*\"".to_owned());
    let js = format!(
        r#"(async function() {{
            const url = {url_lit};
            const meta = document.querySelector('meta[name="csrf-token"]');
            const csrf = meta ? meta.content : '';
            const headers = {{
                'x-requested-with': 'XMLHttpRequest',
                'accept': {accept_lit}
            }};
            headers[{csrf_header_lit}] = csrf;
            const ctrl = new AbortController();
            const timer = setTimeout(function() {{ ctrl.abort(); }}, 20000);
            try {{
                const resp = await fetch(url, {{
                    credentials: 'same-origin',
                    signal: ctrl.signal,
                    headers: headers
                }});
                clearTimeout(timer);
                if (!resp.ok) return JSON.stringify({{ error: 'http ' + resp.status }});
                const body = await resp.text();
                window.__dravrCaptures = window.__dravrCaptures || {{}};
                window.__dravrCaptures[url] = {{ status: resp.status, body: body }};
                // Row shape varies by provider: Strava nests under `.models`,
                // Garmin returns a bare array (or `{{activityList:[...]}}`). Read
                // whichever is present so the stop-decision count is accurate.
                let rows = [];
                try {{
                    const p = JSON.parse(body);
                    if (Array.isArray(p)) rows = p;
                    else if (p && Array.isArray(p.models)) rows = p.models;
                    else if (p && Array.isArray(p.activityList)) rows = p.activityList;
                }} catch (e) {{}}
                const starts = [];
                for (let i = 0; i < rows.length; i++) {{
                    // Strava: `start_time` (RFC3339). Garmin: `startTimeGMT` /
                    // `startTimeLocal` as SQL-style "YYYY-MM-DD HH:MM:SS" — the
                    // space→T swap lets Date.parse handle it. This epoch only
                    // drives the cheap stop heuristic; js_extract owns the exact
                    // per-activity timestamp, so a few hours of tz slop is fine.
                    const raw = rows[i].start_time || rows[i].startTimeGMT || rows[i].startTimeLocal;
                    if (raw) {{
                        const norm = (typeof raw === 'string') ? raw.replace(' ', 'T') : raw;
                        const s = Math.floor(Date.parse(norm) / 1000);
                        if (!isNaN(s)) starts.push(s);
                    }}
                }}
                return JSON.stringify({{ count: rows.length, start_times: starts }});
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
        let mut summary =
            fetch_list_page(page, &url, &pagination.csrf_header, &pagination.accept).await;
        if summary.is_none() {
            warn!(page = page_num, "List page fetch failed; retrying once");
            summary =
                fetch_list_page(page, &url, &pagination.csrf_header, &pagination.accept).await;
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

        // Strava steps by 1 (`&page=N`); Garmin's `start=` is a row offset so it
        // steps by the page size. `.max(1)` guards a misconfigured 0 step from
        // looping forever on the same page.
        page_num += pagination.page_step.max(1);
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

/// Page-context JS that reports the logged-in athlete's numeric id, trying the
/// places Strava embeds it, in order of reliability. Returns a JSON string
/// (`{"id":"12345"}` or `{"id":null}`) — `evaluate` marshals through a string.
const ATHLETE_ID_JS: &str = r#"(function() {
    function digits(s) { var m = s && s.match(/(\d{3,})/); return m ? m[1] : null; }
    var id = null;
    var el = document.querySelector('[data-athlete-id]');
    if (el) { id = digits(el.getAttribute('data-athlete-id')); }
    if (!id) {
        try {
            if (window.pageView && typeof window.pageView.currentAthlete === 'function') {
                var a = window.pageView.currentAthlete();
                if (a && a.get) { id = digits(String(a.get('id'))); }
                else if (a && a.id) { id = digits(String(a.id)); }
            }
        } catch (e) {}
    }
    if (!id) {
        var link = document.querySelector('a[href*="/athletes/"]');
        if (link) { id = digits(link.getAttribute('href')); }
    }
    if (!id) { id = digits(window.location.pathname); }
    return JSON.stringify({ id: id });
})()"#;

/// Resolve the authenticated athlete's numeric id from an already-loaded,
/// cookie-authenticated Strava page. `None` if no source on the page exposes it
/// (the caller then has no `graph_date_range` interval path and returns nothing).
async fn resolve_athlete_id(page: &chromiumoxide::Page) -> Option<String> {
    let result = page.evaluate(ATHLETE_ID_JS).await.ok()?;
    let json_str = result.value().and_then(|v| v.as_str().map(String::from))?;
    let parsed: serde_json::Value = serde_json::from_str(&json_str).ok()?;
    let id = parsed.get("id")?.as_str()?.to_owned();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// One fetched training-log interval (a single month) summarized in-page: just
/// the count of `Activity` entries the `graph_date_range` body carries. The raw
/// body is stashed into `window.__dravrCaptures` for the final `js_extract`, so
/// only the cheap count crosses the CDP bridge each iteration (mirroring
/// [`fetch_list_page`]).
///
/// Unlike the list XHR (`{models:[...]}` JSON), `graph_date_range` returns an
/// HTML-escaped JS blob; an activity row shows up as the escaped marker
/// `&quot;entity&quot;:&quot;Activity&quot;`, so the count is the number of
/// occurrences of that literal in the raw body. Returns `None` on a
/// transport/HTTP error so the caller can end pagination with what it has.
async fn fetch_interval_page(page: &chromiumoxide::Page, url: &str) -> Option<usize> {
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
                const marker = '&quot;entity&quot;:&quot;Activity&quot;';
                const count = body.split(marker).length - 1;
                return JSON.stringify({{ count: count }});
            }} catch (err) {{
                clearTimeout(timer);
                return JSON.stringify({{ error: String(err) }});
            }}
        }})()"#
    );

    let result = match time::timeout(Duration::from_secs(30), page.evaluate(js)).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            warn!(error = %e, url, "Interval page fetch evaluate failed");
            return None;
        }
        Err(_) => {
            warn!(url, "Interval page fetch evaluate timed out (30s)");
            return None;
        }
    };
    let body = result.value().and_then(|v| v.as_str().map(String::from))?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        warn!(url, error = err, "Interval page fetch returned an error");
        return None;
    }
    let count = parsed
        .get("count")
        .and_then(serde_json::Value::as_u64)
        .and_then(|c| usize::try_from(c).ok())
        .unwrap_or(0);
    Some(count)
}

/// Page the activity history by walking Strava's training-log a calendar month
/// at a time via `graph_date_range`, instead of fetching `&page=N` of the
/// reverse-chronological list feed. A deep `after` is reached in ~12 fetches a
/// year rather than ~100 list pages — fast enough to complete before a
/// serverless instance is reclaimed.
///
/// Each month is fetched same-origin (cookies + CSRF) and stashed into
/// `window.__dravrCaptures` by [`fetch_interval_page`]; the provider's
/// `js_extract` then maps the accumulated months, exactly like
/// [`paginate_via_fetch`]. The Rust loop owns the stop decision, stepping the
/// [`IntervalCursor`] back one month per request.
async fn paginate_via_interval(
    page: &chromiumoxide::Page,
    js: &str,
    athlete_id: &str,
    params: &ActivityParams,
    pagination: &ListPagination,
    max_scrape_pages: u32,
    interaction_delay_ms: u64,
) -> Vec<serde_json::Value> {
    const ATHLETE_PLACEHOLDER: &str = "{athlete_id}";
    const INTERVAL_PLACEHOLDER: &str = "{interval}";
    const YEAR_OFFSET_PLACEHOLDER: &str = "{year_offset}";

    let max_months = max_scrape_pages.max(1);
    let target_count = params.limit.unwrap_or(20) as usize;
    let now_year = Utc::now().year();
    let date_bounded = params.after.is_some();

    let mut cursor = IntervalCursor::containing(params.before.unwrap_or_else(Utc::now));
    let mut collected: usize = 0;
    let mut months_done: u32 = 0;

    info!(
        start_month = cursor.interval_param(),
        max_months, date_bounded, "Starting interval-based (month-at-a-time) list pagination"
    );

    // Fresh-head top-up: `graph_date_range` only carries complete weeks, so the
    // newest days are invisible to the interval walk no matter how recent the
    // start month is. Fetch the live list's first page (stashed into
    // `window.__dravrCaptures`; the js_extract merges it deduped by id) so the
    // head of the list is as fresh as the site. Best-effort with one retry —
    // a failure degrades to interval-only coverage, never aborts the scrape.
    // Not counted toward `collected`: its rows overlap the current month's
    // interval capture, and inflating the count could stop the walk early.
    if let Some(template) = pagination.fresh_url_template.as_deref() {
        const PAGE_PLACEHOLDER: &str = "{page}";
        let url = template.replace(PAGE_PLACEHOLDER, &pagination.first_page.to_string());
        let mut fresh =
            fetch_list_page(page, &url, &pagination.csrf_header, &pagination.accept).await;
        if fresh.is_none() {
            warn!("Fresh head page fetch failed; retrying once");
            fresh = fetch_list_page(page, &url, &pagination.csrf_header, &pagination.accept).await;
        }
        if let Some(summary) = fresh {
            info!(count = summary.count, "Fresh head page fetched");
        } else {
            warn!("Fresh head page fetch failed twice — list head may lag the site");
        }
    }

    loop {
        let interval = cursor.interval_param();
        let url = pagination
            .url_template
            .replace(ATHLETE_PLACEHOLDER, athlete_id)
            .replace(INTERVAL_PLACEHOLDER, &interval)
            .replace(
                YEAR_OFFSET_PLACEHOLDER,
                &cursor.year_offset(now_year).to_string(),
            );

        let mut count = fetch_interval_page(page, &url).await;
        if count.is_none() {
            warn!(month = %interval, "Interval page fetch failed; retrying once");
            count = fetch_interval_page(page, &url).await;
        }
        let Some(count) = count else {
            warn!(
                month = %interval,
                collected, "Interval page fetch failed twice — stopping with partial results"
            );
            break;
        };
        months_done += 1;
        collected += count;

        info!(month = %interval, count, collected, "interval month fetched");

        // Date-bounded: we just fetched the month at/after `after`, so stepping
        // back would only surface older activities. Count-bounded: `limit` rows
        // are in hand. `max_months` backstops a runaway walk.
        if let Some(after) = params.after {
            if cursor.prev_month().is_before_month_of(after) {
                break;
            }
        } else if collected >= target_count {
            break;
        }
        if months_done >= max_months {
            warn!(
                months_done,
                max_months,
                collected,
                "Reached max scrape pages — stopping interval pagination (window may be incomplete)"
            );
            break;
        }

        cursor = cursor.prev_month();
        time::sleep(Duration::from_millis(interaction_delay_ms * 3)).await;
    }

    info!(
        months_done,
        collected, "Interval pagination complete; mapping accumulated months"
    );

    // Map every accumulated month via the provider's js_extract (it reads the
    // graph_date_range captures the fetches stashed). Returns the raw,
    // still-undeduplicated rows; the caller dedupes, parses, and filters.
    extract_via_js(page, js).await.unwrap_or_else(|e| {
        warn!(error = %e, "Final list extraction after interval pagination failed");
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

    // Newest-first before the limit cut: rows arrive in capture order (the
    // fresh-head page lands after the interval months), not date order, so
    // truncating unsorted could keep a stale head and drop the newest days.
    activities.sort_by_key(|a| Reverse(a.start_date));
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

/// Normalize a locale-formatted number to a Rust-parseable `f64` string.
///
/// Strava renders distances with the viewer's locale separators: English
/// `"1,250.5"` (comma grouping, dot decimal) but French/German/Spanish/Portuguese
/// `"1 250,5"` or `"5,41"` (space/dot grouping, COMMA decimal). Stripping the
/// comma blindly turns `"5,41"` into `541` — a 100x distance error. So: when both
/// `,` and `.` appear the last-occurring one is the decimal separator and the
/// other is grouping; a lone comma is the decimal separator (Strava never renders
/// a comma-grouped distance without a dot decimal); whitespace is always grouping.
fn normalize_decimal(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    match (s.rfind(','), s.rfind('.')) {
        // Comma after dot => European: dots are grouping, comma is decimal.
        (Some(c), Some(d)) if c > d => s.replace('.', "").replace(',', "."),
        // Dot after comma => English: commas are grouping, dot is decimal.
        (Some(_), Some(_)) => s.replace(',', ""),
        // Lone comma => decimal separator.
        (Some(_), None) => s.replace(',', "."),
        // Lone dot or integer => already parseable.
        (None, _) => s,
    }
}

/// Strip HTML tags from a scraped stat value.
///
/// Strava's training-log interval feed wraps the unit in an `<abbr>` tag, e.g.
/// `"8,19<abbr class='unit' title='kilomètres'> km</abbr>"`. The tags — and
/// their attribute text, which carries the localized unit word
/// ("kilomètres"/"miles") — would otherwise defeat numeric parsing and mislead
/// unit detection. The visible unit (" km") sits OUTSIDE the tags, so stripping
/// only the `<...>` spans leaves a clean `"8,19 km"` the parsers handle. A value
/// with no tags is returned unchanged.
fn strip_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Parse distance strings like "5.2 km", "5,41 km" (fr), or "3.1 mi" into meters.
///
/// Tolerates the interval feed's `<abbr>`-wrapped values (e.g.
/// `"8,19<abbr class='unit'> km</abbr>"`) by stripping HTML tags first.
fn parse_distance_string(s: &str) -> Option<f64> {
    let s = strip_html_tags(s);
    let s = s.trim().to_lowercase();

    if s.contains("km") {
        let km: f64 = normalize_decimal(&s.replace("km", "")).parse().ok()?;
        Some(km * 1000.0)
    } else if s.contains("mi") {
        let mi: f64 = normalize_decimal(&s.replace("mi", "")).parse().ok()?;
        Some(mi * 1609.344)
    } else if s.contains('m') {
        normalize_decimal(&s.replace('m', "")).parse().ok()
    } else {
        normalize_decimal(&s).parse().ok()
    }
}

/// Parse speed strings like "10 km/h" or "6.2 mph" into m/s
fn parse_speed_string(s: &str) -> Option<f64> {
    let s = s.trim().to_lowercase();
    if s.contains("km/h") || s.contains("kph") {
        let num: f64 = normalize_decimal(&s.replace("km/h", "").replace("kph", ""))
            .parse()
            .ok()?;
        Some(num / 3.6)
    } else if s.contains("mph") {
        let num: f64 = normalize_decimal(&s.replace("mph", "")).parse().ok()?;
        Some(num * 0.447_04)
    } else {
        normalize_decimal(&s).parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).single().unwrap() // Safe: test fixture
    }

    #[test]
    fn filters_sort_newest_first_before_the_limit_cut() {
        let item = |id: &str, date: &str| {
            build_activity_from_js_item(
                id,
                &serde_json::json!({ "name": id, "type": "Ride", "date": date, "time": 3600 }),
            )
        };
        // Capture order = interval months first, fresh-head page appended last,
        // so the newest activity arrives at the END of the vec.
        let mut activities = vec![
            item("stale-1", "2026-07-18T12:00:00Z"),
            item("stale-2", "2026-07-17T12:00:00Z"),
            item("fresh-1", "2026-07-20T12:00:00Z"),
        ];
        apply_activity_filters(
            &mut activities,
            &ActivityParams {
                limit: Some(2),
                ..ActivityParams::default()
            },
        );
        let ids: Vec<&str> = activities.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["fresh-1", "stale-1"],
            "the limit cut must keep the newest rows regardless of capture order"
        );
    }

    #[test]
    fn interval_cursor_param_and_offset() {
        let c = IntervalCursor::containing(at(2022, 6, 15));
        assert_eq!(c.interval_param(), "202206");
        assert_eq!(c.year_offset(2026), 4);
        // Same-year interval => offset 0; future-year clamps to 0 (never negative).
        assert_eq!(
            IntervalCursor::containing(at(2026, 1, 1)).year_offset(2026),
            0
        );
        assert_eq!(
            IntervalCursor::containing(at(2027, 1, 1)).year_offset(2026),
            0
        );
    }

    #[test]
    fn interval_cursor_steps_back_a_month_and_rolls_the_year() {
        let jan = IntervalCursor::containing(at(2022, 1, 10));
        assert_eq!(jan.interval_param(), "202201");
        let dec_prev = jan.prev_month();
        assert_eq!(dec_prev, IntervalCursor::containing(at(2021, 12, 1)));
        assert_eq!(dec_prev.interval_param(), "202112");
        // Mid-year step is a plain decrement.
        assert_eq!(
            IntervalCursor::containing(at(2022, 7, 1))
                .prev_month()
                .interval_param(),
            "202206"
        );
    }

    #[test]
    fn interval_cursor_stops_below_the_after_month() {
        let after = at(2022, 1, 1);
        // Walking back from Dec 2022 stays in-window until it crosses Jan 2022.
        assert!(!IntervalCursor::containing(at(2022, 1, 31)).is_before_month_of(after));
        assert!(IntervalCursor::containing(at(2021, 12, 31)).is_before_month_of(after));
    }

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

    /// Real Garmin values, taken from a live `/activity-service/activity/{id}/details`
    /// capture on 2026-09-06 (activity 24235239873, a 6.6 km trail run).
    fn garmin_route_fixture() -> serde_json::Value {
        serde_json::json!({
            "route": {
                "coordinates": [
                    [45.852_678_017_690_78, -74.088_834_123_685_96],
                    [45.852_725_878_357_89, -74.088_854_324_072_6],
                    [45.852_734_511_718_154, -74.088_857_090_100_65],
                    [45.852_844_985_201_955, -74.088_766_314_089_3]
                ],
                "altitudes": [274.6, 274.6, 274.8, 289.6],
                "distances": [0.0, 5.4, 6.4, 6637.59],
                "bounds": {
                    "min_latitude": 45.845_002_289_861_44,
                    "max_latitude": 45.854_990_249_499_68,
                    "min_longitude": -74.111_985_946_074_13,
                    "max_longitude": -74.088_766_314_089_3
                }
            }
        })
    }

    #[test]
    fn parse_route_reads_coordinates_altitudes_and_bounds() {
        let track = parse_route_from_detail(&garmin_route_fixture())
            .expect("fixture carries a four-point track"); // Safe: literal test fixture

        assert_eq!(track.coordinates.len(), 4);
        assert!((track.coordinates[0].0 - 45.852_678).abs() < 1e-6);
        assert!((track.coordinates[0].1 - -74.088_834).abs() < 1e-6);
        assert!((track.coordinates[3].0 - 45.852_845).abs() < 1e-6);

        let alt = track.altitudes_meters.expect("elevation is complete"); // Safe: literal test fixture
        assert_eq!(alt.len(), 4);
        assert!((alt[3] - 289.6).abs() < 1e-6);

        let dist = track.distances_meters.expect("distance is complete"); // Safe: literal test fixture
        assert!((dist[3] - 6637.59).abs() < 1e-6);

        let b = track.bounds.expect("Garmin precomputes the box"); // Safe: literal test fixture
        assert!((b.min_latitude - 45.845_002).abs() < 1e-6);
        assert!((b.max_longitude - -74.088_766).abs() < 1e-6);
    }

    #[test]
    fn parse_route_drops_an_incomplete_series_but_keeps_the_track() {
        // One missing elevation must not yield a shorter, silently misaligned
        // array — the whole series goes, the coordinates stay.
        let mut raw = garmin_route_fixture();
        raw["route"]["altitudes"] = serde_json::json!([274.6, null, 274.8, 289.6]);

        let track = parse_route_from_detail(&raw).expect("coordinates are still good"); // Safe: literal test fixture
        assert_eq!(track.coordinates.len(), 4, "the track survives");
        assert!(
            track.altitudes_meters.is_none(),
            "a gapped elevation series is dropped whole, not padded"
        );
        assert!(
            track.distances_meters.is_some(),
            "an unaffected series is untouched"
        );
    }

    #[test]
    fn parse_route_rejects_a_track_too_short_to_draw() {
        let mut raw = garmin_route_fixture();
        raw["route"]["coordinates"] = serde_json::json!([[45.85, -74.08]]);
        raw["route"]["altitudes"] = serde_json::json!([274.6]);
        raw["route"]["distances"] = serde_json::json!([0.0]);
        assert!(
            parse_route_from_detail(&raw).is_none(),
            "one point is a start coordinate, not a route"
        );
    }

    #[test]
    fn parse_route_is_absent_when_the_provider_sends_no_track() {
        let raw = serde_json::json!({ "name": "Treadmill", "distance": 5000.0 });
        assert!(parse_route_from_detail(&raw).is_none());
    }

    #[test]
    fn merge_detail_fills_route_when_list_had_none() {
        // Guards the gap this field was most likely to fall into: the enriched
        // list path reaches merge_detail_into_activity and never the builder,
        // so a route wired only into the builder is absent from every
        // multi-activity scrape.
        let mut activity = build_activity_from_js_item(
            "24235239873",
            &serde_json::json!({
                "name": "Prévost Trail",
                "type": "trail_running",
                "date": "2026-09-04",
                "time": "0:54:51",
                "distance": "6.6 km",
            }),
        );
        assert!(activity.route.is_none(), "list page must not preempt");

        merge_detail_into_activity(&mut activity, &garmin_route_fixture());

        let track = activity.route.expect("detail pass supplies the track"); // Safe: merged from test JSON
        assert_eq!(track.coordinates.len(), 4);
        assert!((track.coordinates[0].0 - 45.852_678).abs() < 1e-6);
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

        // European comma-decimal (Strava renders fr/de/es/pt this way). The
        // regression that prompted this: "5,41 km" must be 5410 m, NOT 541000.
        let d = parse_distance_string("5,41 km").unwrap(); // Safe: test with valid distance string
        assert!((d - 5410.0).abs() < 1.0, "comma-decimal km: got {d}");
        let d = parse_distance_string("12,07 km").unwrap(); // Safe: test with valid distance string
        assert!((d - 12070.0).abs() < 1.0, "comma-decimal km: got {d}");
        // Grouped + decimal, both locales, must agree (1250.5 km).
        let en = parse_distance_string("1,250.5 km").unwrap(); // Safe: test with valid distance string
        let fr = parse_distance_string("1 250,5 km").unwrap(); // Safe: test with valid distance string
        assert!((en - 1_250_500.0).abs() < 1.0, "en grouped: {en}");
        assert!((fr - en).abs() < 1.0, "fr {fr} must equal en {en}");

        // The training-log INTERVAL feed wraps the unit in an <abbr> tag (real
        // captured value). Without stripping the HTML the parse failed and the
        // fr-comma was dropped, yielding 819 m for an 8.19 km run (shown as
        // "0.02 km" on Telegram). Must now be 8190 m.
        let d =
            parse_distance_string("8,19<abbr class='unit' title='kilomètres'> km</abbr>").unwrap(); // Safe: test with valid distance string
        assert!((d - 8190.0).abs() < 1.0, "abbr-wrapped fr km: got {d}");
        // Same shape with a miles athlete must use the mi factor, not km.
        let d = parse_distance_string("15.4<abbr class='unit' title='miles'> mi</abbr>").unwrap(); // Safe: test with valid distance string
        assert!((d - 24_783.9).abs() < 1.0, "abbr-wrapped mi: got {d}");
    }

    #[test]
    fn strip_html_tags_keeps_text_drops_tags() {
        assert_eq!(
            strip_html_tags("8,19<abbr class='unit' title='kilomètres'> km</abbr>"),
            "8,19 km"
        );
        assert_eq!(strip_html_tags("no tags here"), "no tags here");
    }

    #[test]
    fn normalize_decimal_handles_both_locales() {
        assert_eq!(normalize_decimal("5,41"), "5.41"); // fr decimal
        assert_eq!(normalize_decimal("5.41"), "5.41"); // en decimal
        assert_eq!(normalize_decimal("1 250,5"), "1250.5"); // fr grouped+decimal
        assert_eq!(normalize_decimal("1,250.5"), "1250.5"); // en grouped+decimal
        assert_eq!(normalize_decimal("1.250,5"), "1250.5"); // de grouped+decimal
        assert_eq!(normalize_decimal("523"), "523"); // integer
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
    fn garmin_post_mfa_landing_is_success() {
        // The URL a real Garmin login lands on after the MFA code is accepted. It was
        // reported as a timeout because the patterns only matched the sub-pages Garmin
        // routes to afterwards, so an authenticated session looked like a failed login.
        let provider = ProviderConfig::garmin_default().unwrap(); // Safe: test fixture
        let success = provider.provider.login_success_patterns;
        assert!(
            url_path_matches("https://connect.garmin.com/app/", &success),
            "the post-MFA landing page must count as success"
        );
        // The pages it routes to next must keep matching.
        assert!(url_path_matches(
            "https://connect.garmin.com/app/home",
            &success
        ));
        assert!(url_path_matches(
            "https://connect.garmin.com/modern/activities",
            &success
        ));
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

    // The credential poll samples `initial_url` after submitting the password, which
    // races the navigation that submit started. These pin both outcomes of that race,
    // because only one of them used to work.
    const LOGIN_PAGE: &str = "https://accounts.google.com/v3/signin/identifier";
    const CHALLENGE_SELECTION: &str = "https://accounts.google.com/v3/signin/challenge/selection";

    #[test]
    fn poll_keeps_waiting_while_the_submit_has_not_navigated() {
        // Still on the page we submitted from: nothing to read yet, keep polling.
        assert!(awaiting_post_submit_navigation(LOGIN_PAGE, LOGIN_PAGE));
    }

    #[test]
    fn poll_reads_a_challenge_page_it_started_on() {
        // Lost the race: the navigation completed before `initial_url` was sampled, so
        // the challenge page IS the initial URL. Waiting for it to "change" would skip
        // the challenge branches until the deadline and fail the login on a rendered
        // 2FA chooser — the timeout that reds google_oauth_2fa_number_match.
        assert!(!awaiting_post_submit_navigation(
            CHALLENGE_SELECTION,
            CHALLENGE_SELECTION
        ));
    }

    #[test]
    fn poll_reads_every_challenge_kind_it_started_on() {
        // The device prompt and passkey pages have their own branches in the same
        // loop and are reachable as the initial URL by the same race.
        for url in [
            "https://accounts.google.com/v3/signin/challenge/dp",
            "https://accounts.google.com/v3/signin/challenge/pk",
            "https://accounts.google.com/v3/signin/challenge/totp",
        ] {
            assert!(
                !awaiting_post_submit_navigation(url, url),
                "{url} is a challenge page and must be read, not waited on"
            );
        }
    }

    #[test]
    fn poll_proceeds_once_the_url_changed() {
        // Won the race: the ordinary path, unaffected by the fix.
        assert!(!awaiting_post_submit_navigation(
            CHALLENGE_SELECTION,
            LOGIN_PAGE
        ));
    }

    const TOTP_PAGE: &str = "https://accounts.google.com/v3/signin/challenge/totp";

    #[test]
    fn otp_page_is_reported_to_the_caller_that_submitted_a_password() {
        // Won the race: URL changed to the OTP page.
        assert!(otp_page_should_be_reported(TOTP_PAGE, LOGIN_PAGE, true));
        // Lost it: the navigation landed before initial_url was sampled, so the OTP
        // page IS the initial URL. Reported anyway — the password caller asked "what
        // challenge appeared", and this is the answer regardless of sampling order.
        assert!(otp_page_should_be_reported(TOTP_PAGE, TOTP_PAGE, true));
    }

    #[test]
    fn otp_page_is_not_reported_back_to_the_caller_that_submitted_a_code() {
        // submit_otp / select_two_factor start ON the OTP page and wait for it to
        // redirect away. Reporting it here would ask for the same code forever.
        assert!(!otp_page_should_be_reported(TOTP_PAGE, TOTP_PAGE, false));
        // But a move to a DIFFERENT OTP page is a genuine new challenge.
        assert!(otp_page_should_be_reported(
            TOTP_PAGE,
            "https://accounts.google.com/v3/signin/challenge/sms",
            false
        ));
    }

    #[test]
    fn a_non_otp_page_is_never_reported_as_otp() {
        assert!(!otp_page_should_be_reported(LOGIN_PAGE, LOGIN_PAGE, true));
        assert!(!otp_page_should_be_reported(
            CHALLENGE_SELECTION,
            LOGIN_PAGE,
            true
        ));
    }

    #[test]
    fn only_the_chooser_is_parsed_for_2fa_options() {
        assert!(is_two_fa_chooser_url(CHALLENGE_SELECTION));

        // Each of these either has its own branch in the poll loop or carries no
        // [data-challengetype] elements at all. Parsing them spends the whole settle
        // budget re-reading an empty list, then clicks an "Enter your password" link
        // that is not on the page.
        for url in [
            "https://accounts.google.com/v3/signin/challenge/number",
            "https://accounts.google.com/v3/signin/challenge/dp",
            "https://accounts.google.com/v3/signin/challenge/pk",
            "https://accounts.google.com/v3/signin/challenge/pwd",
        ] {
            assert!(
                !is_two_fa_chooser_url(url),
                "{url} carries no 2FA options and must not be parsed for them"
            );
        }

        // Not a challenge page at all.
        assert!(!is_two_fa_chooser_url(LOGIN_PAGE));
    }
}

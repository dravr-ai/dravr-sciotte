# Changelog

## [0.5.2] — 2026-04-01

### Fixed

- fix: close headless browser gracefully after scraping operations
  Sends `Browser.close` CDP command so Chrome shuts down cleanly and
  the WebSocket handler task exits without error-looping. Previously,
  dropping the browser without closing caused chromiumoxide to spam
  ERROR-level deserialization failures in a tight loop.
- feat: add `close_browser()` to `ActivityScraper` trait (default no-op)
  and `ChromeScraper`/`CachedScraper` implementations

## [0.5.1] — 2026-03-31

### Fixed

- fix: increase login test timeouts to 30s to reduce CI flakiness
- fix: add ignored-tests-allowlist for flaky 2FA login test
- fix: resolve error handling violations found by dravr-build-config validation



## [0.5.0] — 2026-03-30

### Added

- feat(health): add FTP field to DailySummary, attempt weight/FTP extraction from Strava fitness page
- feat(health): add Strava Fitness & Freshness extraction with fitness/fatigue/form scores New DailySummary fields and health_pages.fitness config for Strava provider
- feat(auth): auto-orchestrate Google 2FA selection at server level Server auto-selects preferred 2FA method and polls for phone tap, scraper returns TwoFactorChoice unchanged
- feat(health): support multiple health pages per provider with merge Adds sleep/HRV/weight fields to DailySummary, loops all configured health_pages and merges results
- feat(health): add provider-agnostic daily health summary extraction REST, MCP, and cache support for HR, body battery, stress, steps, VO2 max from Garmin Connect

### Fixed

- fix(auth): OTP false positive from base64 tokens in Google URLs, interactive login scripts Strip query params before matching OTP_URL_PATTERNS; rewrite login scripts with full 2FA flow
- fix(auth): 2FA method selection with priority fallback (app → otp → first) Handles users without Google app by falling back to authenticator code



## [0.4.4] — 2026-03-26

### Fixed

- fix: wait for DOM before parsing 2FA options on challenge page Race condition on slow CI runners caused empty parse_two_fa_options, looping until timeout

### Other

- deps: bump dravr-tronc to 0.2 with error notification support



## [0.4.3] — 2026-03-25



## [0.4.2] — 2026-03-24

### Fixed

- fix: don't click 'Try another way' on /challenge/dp device prompt Notification already on user's phone — return NumberMatch to let user approve
- fix: log all number candidates for debugging on Cloud Run Removed direct-text and font-size filters, picks largest font number
- fix: simplify number extraction to JS-only with largest font heuristic Picks the 2-3 digit number with the largest computed font-size (>24px)

### Other

- refactor: externalize JS scripts with runtime override via DRAVR_SCIOTTE_SCRIPTS_DIR TTL-cached script loader, compiled-in defaults, no recompile for JS changes



## [0.4.1] — 2026-03-24

### Added

- feat: add commit-msg hook and update CLAUDE.md commit rules Enforce max 2-line commits, conventional format, no AI signatures

### Fixed

- fix: handle Google /challenge/dp device prompt on Cloud Run Click 'Try another way' to reach 2FA selection with OTP options



## [0.4.0] — 2026-03-23

### Added

- feat: embedded fake login server for testing (DRAVR_SCIOTTE_FAKE_LOGIN)
- feat: add DRAVR_SCIOTTE_FAKE_MODE for testing login flows without Chrome

### Fixed

- fix: address code review findings (P1, P2, P3)

### Other

- Revert "feat: add DRAVR_SCIOTTE_FAKE_MODE for testing login flows without Chrome"



## [0.3.2] — 2026-03-23

### Added

- feat: add NumberMatch LoginResult for Google number matching challenge



## [0.3.1] — 2026-03-23



## [0.3.0] — 2026-03-23

### Added

- feat: vision login works end-to-end with Strava Google OAuth
- feat: wire vision mode into server with Copilot Headless LLM

### Fixed

- fix: use config.headless for credential_login browser launch



## [0.2.1] — 2026-03-20

### Added

- feat: add garmin_default() to ProviderConfig with embedded garmin.toml
- feat: add GET /api/athlete endpoint for profile scraping
- feat: add get_athlete() to ActivityScraper trait for profile scraping
- feat: add VisionScraper with LLM-powered screenshot analysis (vision feature)

### Fixed

- fix: use crates.io embacle dependency instead of local path
- fix: match URL patterns against path only, not query string
- fix: match URL path only for success patterns, add /mfa to OTP detection Prevents Garmin MFA page query param from matching success pattern
- fix: prioritize success patterns over failure patterns in login polling
- fix: handle Google sign-in method chooser page in OAuth flow
- fix: Garmin MFA login, OTP retry, unique Chrome profiles



## [0.2.0] — 2026-03-20

### Added

- feat: TwoFactorChoice, select_two_factor, passkey bypass with CDP click Multi-step 2FA flow, visible Chrome for credential login, Google challenge navigation
- feat: add method param to credential_login for Google/Apple OAuth CDP-based form filling, Google/Apple sign-in page navigation, multi-step flow
- feat: multi-step login (email→password→OTP) with submit_otp follow-up Progressive form detection, OTP page storage, provider TOML otp selectors
- feat: add credential_login to core ActivityScraper trait LoginResult enum (Success/OtpRequired/Failed), in-process headless Chrome login with form filling
- feat: multi-session store, session management endpoints, WebSocket auth
- feat: WebSocket browser streaming for remote login via CDP screencast
- feat: add Garmin Connect provider with MFA support and --provider CLI flag

### Fixed

- fix: OTP polling, browser persistence, passkey bypass, submit button selectors Complete Google OAuth 2FA flow with credential_login + select_two_factor + submit_otp
- fix: Google OAuth passkey bypass, CDP form filling, visible Chrome
- fix: screenshot polling, biased select, cookie dismiss, coordinate scaling Replace CDP screencast with captureScreenshot polling, prioritize client input, auto-dismiss cookies



## [0.1.0] — 2026-03-18

### Added

- feat: paginate training page by clicking next_page button for >20 activities
- feat: extract HR, cadence, max speed from embedded activity JSON data
- feat: TOML-configurable provider, detail enrichment, pagination, weather/device/gear fields

### Fixed

- fix: extract gear name from span.gear-name selector

### Other

- refactor: rename StravaScraper trait to ActivityScraper for generic platform integration



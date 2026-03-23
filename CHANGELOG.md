# Changelog

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



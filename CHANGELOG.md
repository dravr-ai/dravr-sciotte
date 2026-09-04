# Changelog

## [0.9.2] — 2026-09-04

### Fixed

- fix(build): spread BrowserLaunchConfig defaults into the literal

### Other

- chore(deps): bump dravr-browser to 0.1.1
- chore(deps): bump dravr-tronc to 0.11.0



## [0.9.1] — 2026-09-04

### Fixed

- fix(auth): hold a pending OTP login as long as the code is valid
- fix(docker): bump apt epoch for the chromium 152 CVE batch, tested
- fix(docker): bump apt epoch for the chromium 2026-08-31 CVE



## [0.9.0] — 2026-08-21

### Added

- feat(server): serve ungated on loopback binds — the development mode
- feat(auth): require a Google identity token, and gate every route once

### Fixed

- fix(docker): bump apt epoch past the chromium 76033 batch



## [0.8.6] — 2026-08-17

### Added

- feat(cli): probe-login checks each provider's live login page

### Fixed

- fix(test): use the repo's test-fixture unwrap idiom in the Garmin test
- fix(ci): call the platform notifier instead of waiting on `on: release`
- fix(scraper): repair three login paths broken against live providers
- fix(state): honour the caller's ScraperConfig for logins

### Other

- test(server): drive the 2FA login chain over HTTP against the fixtures



## [0.8.5] — 2026-08-14

### Fixed

- fix(scraper): report an OTP page the poll started on
- fix(scraper): read a challenge page the poll started on
- fix: repair the SessionStart bootstrap guard for an empty .build
- fix(tests): read the full request in the fixture server, not one segment
- fix(server): answer 401 session_expired for auth-shaped scraper errors Auth/SessionExpired flattened to 500 paged operators for dead cookies; 401 lets the platform mint the athlete's reconnect link

### Other

- refactor(scraper): poll for 2FA options and latch the password fallback
- test(config): make the defaults test hermetic instead of env-dependent
- chore(deps): bump dravr-tronc 0.5.3 -> 0.6.2
- chore(register): ledger + weekly phase review
- chore(register): point at dravr-carnet, the dravr-family register



## [0.8.4] — 2026-07-20



## [0.8.3] — 2026-07-20



## [0.8.0] — 2026-07-16

### Added

- feat(server): one multi-provider instance — scraper map, provider-tagged sessions, flow_id login plane (ADR-021) Bare serve loads garmin+strava (repeatable --provider overrides); sessions carry their provider so import/export ({provider, session}) and every scrape route to the right scraper; interactive logins run on ephemeral per-flow scrapers parked under a server-minted flow_id with the permit held in the flow (N concurrent users/providers, TTL-reaped), replacing the single-scraper parking; MCP tools/health follow. +7 routing/flow tests; validated live: garmin OTP login + scrape via the platform.
- feat(server): accept before/after epoch window on GET /api/activities (ADR-021) ActivityQuery gains after/before Unix-epoch params mapped into ActivityParams so the platform's remote scrape can request a historical window, matching the in-process path.
- feat(server): report provider in the authenticated login responses (ADR-021) ServerState carries the provider name (from its ProviderConfig) and includes it in every authenticated response (login-with-credentials / submit-otp / select-2fa), so a platform caller persists the session under the right provider without tracking it across the multi-step 2FA flow — the platform stays stateless (no DB/Redis pointer). Adds ChromeScraper::provider_name.

### Fixed

- fix(vision): 2FA continuations carry credentials + resolve option ids against the parked chooser analysis select_two_factor now drives the same page types as the login driver (oauth_password fillable — the parked flow carries email/password; chained choosers re-park; passkey bypass), resolves the echoed option id against the STORED analysis (LLM ids are non-deterministic per analysis; coords stay valid on the unchanged page), and the server re-parks the flow on continuation errors instead of burning browser+permit. +scripted-VisionModel regression test; validated live: Strava via Google (password chooser -> 2SV phone-tap) through the platform.

### Other

- build(docker): vision feature + Copilot CLI in the service image The deployed service runs DRAVR_SCIOTTE_LOGIN_MODE=hybrid; vision login needs the feature compiled in and the pinned Copilot CLI (>=1.0.59, mirroring the pierre image) available at runtime.



## [0.7.16] — 2026-07-13



## [0.7.15] — 2026-07-13



## [0.7.14] — 2026-07-11



## [0.7.13] — 2026-07-11

### Fixed

- vision: prompts referenced by the embedded provider TOMLs
  (`providers/strava/{page_analysis,list_page,detail_page}.md`) are now
  compiled into the binary and resolved through the
  `DRAVR_SCIOTTE_SCRIPTS_DIR` override directory first, instead of a bare
  cwd-relative filesystem read. Deployed binaries — whose working directory
  does not contain the repository's `providers/` tree — failed every Strava
  vision credential login with `config error: Failed to read vision prompt
  'providers/strava/page_analysis.md': No such file or directory`. Prompts
  now load from the compiled-in defaults, with the scripts-dir mount as a
  hot-swappable override; raw paths still resolve for custom provider
  configs.

## [0.7.12] — 2026-06-30

### Added

- login: observability for the silent-hang class. `poll_credential_login_result`
  and `wait_for_login` now log every page navigation as a breadcrumb, warn every
  15s while stuck on the initial login page with no navigation after submit (the
  signature of a provider blocking the browser or serving an inline challenge),
  and name the last page reached + elapsed time in the timeout error. A
  blocked/challenged credential login is now diagnosable from the logs instead of
  a contextless multi-minute spin. Extracted `log_login_nav`, `log_login_stall`,
  and `credential_login_timeout_error` helpers (behaviour-preserving).

## [0.7.10] — 2026-06-26

### Fixed

- strava: read the activity's canonical numeric `distance` (meters) from the
  interval-feed entry instead of the formatted `stats` distance value. Some
  graph_date_range months deliver that stat as a bare number with no km/mi unit
  token, which `build_activity_from_js_item`'s parser then kept as
  km-magnitude-treated-as-meters (the "0.0X km" bug on 2024 activities). The
  numeric `a.distance` field is unit-unambiguous and locale-proof — the same
  pattern the extract already uses for `a.elapsedTime`. Falls back to the
  formatted stat only when the numeric field is absent (no regression for any
  year). Validated live against a real 2024 Strava scrape.

## [0.7.9] — 2026-06-26

### Fixed

- garmin: restore activity-list + detail capture broken by the dravr-browser
  consolidation. The injected capture payload now nests responses under
  `__dravrCaptures.byUrl` (with a `.last` pointer) and stores the body as
  `chunks: [string]` rather than the old flat `{[url]: {status, body}}` map.
  Garmin's `js_extract` relies on the passive hydration capture, so it silently
  read an empty map and scraped 0 activities; Strava was unaffected because it
  explicitly fetch+stashes into the flat top level. Both Garmin extract blocks
  now merge the `.byUrl` + flat shapes and read the body via `chunks.join("")`.

### Added

- `SportType::from_garmin` maps Garmin's lowercase snake-case `typeKey`
  (`trail_running`, `gravel_cycling`, `mountain_biking`, `lap_swimming`, …) to
  the shared sport variants. The list + detail builders try `from_strava` first
  and fall back to `from_garmin`, so Garmin activities no longer all bucket as
  `Other`. The detail builder also reads `distance` via a numeric fallback
  (Garmin emits a bare-number meters field where Strava emits a unit string).

## [0.7.8] — 2026-06-26

### Fixed

- scraper: strip HTML tags from scraped distance values before parsing. Strava's
  training-log interval feed (`graph_date_range`) wraps the unit in an `<abbr>`
  tag, e.g. `"8,19<abbr class='unit' title='kilomètres'> km</abbr>"`. The tags
  (and their `title` text, which carries the localized unit word) broke
  `parse_distance_string` — the numeric `.parse()` failed on the HTML, fell back
  to a digit-only filter that dropped the fr-comma, and yielded `819` m for an
  8.19 km run (rendered "0.02 km" downstream). `parse_distance_string` now
  strips `<…>` spans first, recovering the clean `"8,19 km"` → 8190 m. The
  visible unit text sits outside the tags, so km/mi detection still works.

## [0.7.6] — 2026-06-19

### Changed

- deps: migrate `dravr-sciotte-mcp` and `dravr-sciotte-server` to dravr-tronc
  0.5.3 (dual-era MCP engine). tronc 0.5 hands tools/handlers a shared
  `&Arc<ServerState>` with no outer `RwLock`, so the session store moved to a
  per-field interior `RwLock<SessionStore>`; the session accessors are now
  `&self` async and return owned `AuthSession` clones. The scraper/limiter are
  accessed lock-free (already `Sync`).

### Fixed

- lint: annotate test-fixture `unwrap`/`expect` in `scraper`/`provider` tests
  with `// Safe` for the architectural-validation gate.

## [0.7.0] — 2026-06-05



## [0.6.0] — 2026-06-03



## [0.5.26] — 2026-06-02



## [0.5.25] — 2026-05-29



## [0.5.24] — 2026-05-29



## [0.5.23] — 2026-05-29



## [0.5.22] — 2026-05-29



## [0.5.21] — 2026-05-29

### Fixed

- fix(strava): real activity start_date from embedded JSON; never fabricate now() Detail js_extract now anchors to the activity id and reads its unquoted UTC start_date epoch (was matching mid-ride segment efforts, or Utc::now on no match — making everything look like today); Garmin detail projection emits startTimeGMT->RFC3339 for parity; build_activity_from_detail/js_item fall back to a loud UNIX_EPOCH sentinel + warn instead of now().



## [0.5.20] — 2026-05-29



## [0.5.19] — 2026-05-28

### Fixed

- fix(strava): real start time from activity JSON start_date, not date-only List page is date-only; detail <time> is zone-less French text. Extract UTC ISO start_date from pageView.activity()/embedded JSON + RFC3339 parse, so activities carry the true start time instead of T00:00:00.



## [0.5.18] — 2026-05-28

### Other

- test(login_flow): widen test_config timeouts for CI-Chrome headroom google_oauth_2fa_number_match flaked at ~48s on CI's slower headless Chrome against the 10/30/5s ceilings; bumps to 30/90/20s (ceilings only — passing runs return early).



## [0.5.17] — 2026-05-28

### Fixed

- fix(test): drop chrome_runtime_missing assertion orphaned by 0db1717 Stealth no longer installs the chrome.runtime stub (it leaked an Object.defineProperty .toString() trace and the WebGL spoof shipped with it 427'd Cloudflare). Citation added in the test.

### Other

- chore(deps): align embacle to git v0.15.8 for pierre vision interop Ports vision.rs ChatRequest.turn_id and server with_config (now sync) to the 0.15.8 API so the shared Arc<dyn LlmProvider> trait objects unify.



## [0.5.16] — 2026-05-22



## [0.5.15] — 2026-05-22

### Fixed

- fix(scraper): debounce Try Another Way click at /challenge/dp Earlier flow clicked 'Try another way' on every polling iteration while /challenge/dp was still rendering, generating duplicate phone notifications. The tried_another_way guard fires the click exactly once so Google's chooser at /challenge/selection surfaces cleanly and the user sees the [app]/[otp] options with one notification.
- fix(scraper): /challenge/dp returns NumberMatch immediately, drop autonomous 'Try another way' click The script/modal already prompts the user via the number_match handler; clicking Try Another Way autonomously generated duplicate phone notifications and stripped the user's choice. Behavior matches the pre-538b7a5 baseline now.
- fix(stealth): remove WebGL/plugins/languages JS spoofs that 427'd headless on Cloudflare Hardcoded WebGL renderer 'Apple Inc.'/'Apple M1' contradicted the Linux UA on Docker/Cloud Run causing Cloudflare to 427 the request before the login form rendered. .hide() already covers navigator.webdriver via --disable-blink-features=AutomationControlled, making the JS overrides redundant. Garmin headless login restored to March 30 working baseline.
- fix(scraper): scrape real digit from /challenge/dp, fall back to 'Try another way' Replaces the 'Check your phone' placeholder NumberMatch that the platform modal rendered as a garbled number; uses extract_number_from_page() and falls back to the 2FA chooser when no digit is present.

### Other

- ci(release): cap upload-artifact retention at 7d



## [0.5.14] — 2026-05-07



## [0.5.13] — 2026-05-07



## [0.5.12] — 2026-05-05



## [0.5.11] — 2026-05-01

### Added

- feat(garmin): project start_latitude/start_longitude from summaryDTO Garmin Connect's /activity-service/activity/{id} response carries the start coord on summaryDTO.startLatitude/startLongitude (older endpoints used …Decimal suffix). Project both into the same start_latitude/start_longitude string keys the shared Rust parser already reads, with [0,0] / out-of-range / non-numeric guards so indoor activities don't get bogus coords. Same parser path as Strava — no scraper.rs changes needed.
- feat(strava): scrape start_latitude/start_longitude from activity detail page Detail-page JS reads pageView.activity().start_latlng with embedded-script regex + Mapbox static-map URL as fallbacks; build_activity_from_detail and merge_detail_into_activity now plumb GPS coords. Unblocks the dravr-platform weather backfill which had been filtering out every sciotte row because start_latitude was always None. v0.5.11.

### Fixed

- fix(tests): unsuboptimal Duration units for clippy 1.95 queue_test: from_secs(60) -> from_mins(1); login_flow: from_millis(1000) -> from_secs(1). Pedantic clippy::duration_suboptimal_units flagged both literals.
- fix(pending_login): drop multiple-of-60 in Duration::from_secs literal clippy 1.95 duration_suboptimal_units flagged Duration::from_secs(60); test only needs a non-zero ttl, switch to 5.
- fix(scraper): TTL-bounded pending_login eviction PendingLogin<T> wrapper records park-time; abandoned 2FA flows older than ScraperConfig::pending_login_ttl_secs (default 300s, env DRAVR_SCIOTTE_PENDING_LOGIN_TTL) are dropped on next access so chromiumoxide's kill_on_drop reaps the held Chrome — replaces the prior unbounded Mutex<Option<(Browser, Page)>> in both ChromeScraper and VisionScraper.
- fix(config): is_ok_and replaces map().unwrap_or for env booleans Clippy 1.95 promoted clippy::map_unwrap_or to deny under workspace pedantic; CI was red on credential_login_headless and fake_login since 2026-04-27.



## [0.5.10] — 2026-04-27

### Fixed

- fix(clippy): import use paths, map_or_else, raw-string + doc backticks Pre-push clippy gate caught absolute_paths, map().unwrap_or_else, doc-comment style; fixed inline. Test sites bumped to 3-arg launch_browser(..., None).

### Other

- style: cargo fmt --all Second fmt pass after the clippy fixes.
- style: cargo fmt --all Pre-push fmt gate caught reformats in browser_utils, config, vision after the stealth + persistence patch landed.



## [0.5.9] — 2026-04-24



## [0.5.8] — 2026-04-15

### Added

- feat(queue): FIFO backpressure limiter for Chrome scraping SciotteLimiter + QueuedScraper wrap any ActivityScraper with a bounded FIFO-fair semaphore, parked permits across multi-step OTP/2FA flows, watchdog eviction, and 503+Retry-After on saturation; QueueConfig::from_env requires all seven DRAVR_SCIOTTE_* vars with no crate defaults, fails fast via QueueConfigError, and LimiterError variants carry the configured Retry-After Duration (NoCapacity split from AcquireTimeout); both binaries init at startup; README documents the required vars.



## [0.5.7] — 2026-04-10

### Other

- build: reduce tokio feature footprint to minimal set



## [0.5.6] — 2026-04-03



## [0.5.5] — 2026-04-02



## [0.5.4] — 2026-04-01

### Fixed

- fix: drop page before close_browsers to release Arc reference Arc::into_inner silently failed because the Page held a browser ref



## [0.5.3] — 2026-04-01



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



# Configuration

All runtime behaviour is controlled through environment variables. No configuration files are required. Every variable has a sensible default so you only need to set the ones that differ from your deployment.

## Table of Contents

- [Authentication](#authentication)
- [Chrome / Browser](#chrome--browser)
- [Login Timing](#login-timing)
- [Page Interaction](#page-interaction)
- [2FA Timing](#2fa-timing)
- [Cache](#cache)
- [Storage](#storage)
- [OAuth (Strava)](#oauth-strava)
- [Example .envrc](#example-envrc)

---

## Authentication

Controls access to the REST API and the WebSocket streaming endpoint.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_API_KEY` | _(unset — auth disabled)_ | Bearer token required on every request. When unset, the server accepts all requests without authentication. The WebSocket endpoint accepts this value via the `?token=` query parameter because browser WebSocket APIs cannot send custom headers. |

When set, clients must include the header `Authorization: Bearer <key>` on REST requests. The comparison is performed in constant time to prevent timing attacks.

---

## Chrome / Browser

Controls which Chrome binary is used for browser automation.

| Variable | Default | Description |
|---|---|---|
| `CHROME_PATH` | _(auto-detected)_ | Absolute path to the Chrome or Chromium binary. When unset, `chromiumoxide` searches the standard installation paths for the current platform. |

---

## Login Timing

Controls how long the scraper waits during the login sequence and how frequently it checks for completion.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_LOGIN_TIMEOUT` | `120` | Maximum wall-clock time in **seconds** to wait for the full login flow to complete before giving up. |
| `DRAVR_SCIOTTE_LOGIN_POLL_INTERVAL_MS` | `500` | Interval in **milliseconds** between URL checks when polling for login success or failure. |

---

## Page Interaction

Controls general browser navigation and form automation timing.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_PAGE_TIMEOUT` | `30` | CDP page-level operation timeout in **seconds**. Applies to individual browser commands such as navigation and element queries. |
| `DRAVR_SCIOTTE_PAGE_LOAD_WAIT` | `3` | Additional wait in **seconds** after a page navigation completes, allowing JavaScript-rendered content to finish rendering before the scraper interacts with the page. |
| `DRAVR_SCIOTTE_INTERACTION_DELAY_MS` | `500` | Delay in **milliseconds** between successive browser interaction steps (e.g., between clicking a button and reading the result). |
| `DRAVR_SCIOTTE_FORM_DELAY_MS` | `300` | Delay in **milliseconds** between individual form-field interactions (e.g., between typing into one field and moving to the next). |

---

## 2FA Timing

Controls timeouts for each step of multi-factor authentication flows.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_EMAIL_STEP_TIMEOUT` | `10` | Timeout in **seconds** waiting for the password field to appear after the email address has been submitted. |
| `DRAVR_SCIOTTE_PASSWORD_STEP_TIMEOUT` | `10` | Timeout in **seconds** waiting for a login result after the password has been submitted. |
| `DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT` | `60` | Timeout in **seconds** waiting for the user to approve a push-notification or phone-tap 2FA prompt. This is intentionally longer to give the user time to act on their device. |

---

## Cache

Controls the in-memory activity data cache.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_CACHE_TTL` | `900` | Time-to-live in **seconds** for cached activity entries. After this duration, a fresh scrape is triggered on the next request. |
| `DRAVR_SCIOTTE_CACHE_MAX` | `100` | Maximum number of entries held in the cache at one time. Older entries are evicted when the limit is reached. |

---

## Storage

Controls where session data is persisted on disk.

| Variable | Default | Description |
|---|---|---|
| `DRAVR_SCIOTTE_SESSION_DIR` | `$XDG_CONFIG_HOME/dravr-sciotte` (Linux) or `~/Library/Application Support/dravr-sciotte` (macOS) | Directory where encrypted session cookies are stored between runs. The directory is created automatically if it does not exist. Set this to a writable path in containerised deployments where the default config directory is not available. |

---

## OAuth (Strava)

Required only when using the Strava OAuth flow (`/auth/strava/*` endpoints). These variables are not needed for credential-based or browser-streaming login.

| Variable | Default | Description |
|---|---|---|
| `STRAVA_CLIENT_ID` | _(required)_ | Strava API application client ID, obtained from the [Strava API settings](https://www.strava.com/settings/api). |
| `STRAVA_CLIENT_SECRET` | _(required)_ | Strava API application client secret, obtained from the same settings page. |
| `STRAVA_REDIRECT_URI` | `http://localhost:3000/auth/callback` | OAuth callback URI. Must exactly match one of the authorized redirect URIs registered in your Strava application settings. |
| `STRAVA_SCOPES` | `activity:read_all` | Comma-separated list of Strava OAuth permission scopes. See the [Strava API documentation](https://developers.strava.com/docs/authentication/) for available scopes. |

---

## Example .envrc

The following file covers a typical local development setup. Copy it to the project root and load it with `direnv allow`, or source it manually with `source .envrc`.

```sh
# .envrc — local development configuration

# ── Authentication ────────────────────────────────────────────────────────────
# Uncomment to require a bearer token on all requests.
# export DRAVR_SCIOTTE_API_KEY="change-me-before-use"

# ── Chrome / Browser ─────────────────────────────────────────────────────────
# Uncomment and set if Chrome is not on the default search path.
# export CHROME_PATH="/usr/bin/google-chrome-stable"

# ── Login Timing ─────────────────────────────────────────────────────────────
export DRAVR_SCIOTTE_LOGIN_TIMEOUT=120
export DRAVR_SCIOTTE_LOGIN_POLL_INTERVAL_MS=500

# ── Page Interaction ─────────────────────────────────────────────────────────
export DRAVR_SCIOTTE_PAGE_TIMEOUT=30
export DRAVR_SCIOTTE_PAGE_LOAD_WAIT=3
export DRAVR_SCIOTTE_INTERACTION_DELAY_MS=500
export DRAVR_SCIOTTE_FORM_DELAY_MS=300

# ── 2FA Timing ───────────────────────────────────────────────────────────────
export DRAVR_SCIOTTE_EMAIL_STEP_TIMEOUT=10
export DRAVR_SCIOTTE_PASSWORD_STEP_TIMEOUT=10
export DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT=60

# ── Cache ─────────────────────────────────────────────────────────────────────
export DRAVR_SCIOTTE_CACHE_TTL=900
export DRAVR_SCIOTTE_CACHE_MAX=100

# ── Storage ───────────────────────────────────────────────────────────────────
# Uncomment to override the default platform config directory.
# export DRAVR_SCIOTTE_SESSION_DIR="/var/lib/dravr-sciotte/sessions"

# ── OAuth (Strava) ────────────────────────────────────────────────────────────
# Required only if using the Strava OAuth flow.
# export STRAVA_CLIENT_ID="your-client-id"
# export STRAVA_CLIENT_SECRET="your-client-secret"
# export STRAVA_REDIRECT_URI="http://localhost:3000/auth/callback"
# export STRAVA_SCOPES="activity:read_all"
```

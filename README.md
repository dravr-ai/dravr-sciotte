# Sciotte — Sport Activity Scraper

[![crates.io](https://img.shields.io/crates/v/dravr-sciotte.svg)](https://crates.io/crates/dravr-sciotte)
[![docs.rs](https://docs.rs/dravr-sciotte/badge.svg)](https://docs.rs/dravr-sciotte)
[![CI](https://github.com/dravr-ai/dravr-sciotte/actions/workflows/ci.yml/badge.svg)](https://github.com/dravr-ai/dravr-sciotte/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue.svg)](LICENSE.md)

Sport activity scraper with headless Chrome, TOML-configurable providers, and in-memory caching. Logs into sport platforms via a browser (no API credentials needed), scrapes training data from activity pages, and exposes it through four integration surfaces: Rust trait, REST API, MCP server, and CLI.

## Table of Contents

- [Install](#install)
- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [Credential Login](#credential-login)
- [REST API Server](#rest-api-server-dravr-sciotte-server)
- [MCP Server](#mcp-server-dravr-sciotte-mcp)
- [Library Usage](#library-usage-rust-trait)
- [Provider Configuration](#provider-configuration)
- [Activity Data Model](#activity-data-model)
- [Environment Variables](#environment-variables)
- [Docker](#docker)
- [Architecture](#architecture)
- [License](#license)

## Install

### Homebrew (macOS / Linux) — recommended

```bash
brew tap dravr-ai/tap
brew install dravr-sciotte
```

This installs two binaries:

- **`dravr-sciotte-server`** — REST API + MCP server + CLI (start with `dravr-sciotte-server serve`)
- **`dravr-sciotte-mcp`** — standalone MCP server for editor integration

Once installed, login and scrape:

```bash
dravr-sciotte-server login
dravr-sciotte-server activities --limit 20
```

### Docker

```bash
docker pull ghcr.io/dravr-ai/dravr-sciotte:latest
docker run -p 3000:3000 ghcr.io/dravr-ai/dravr-sciotte
```

### Cargo (library)

```toml
[dependencies]
dravr-sciotte = "0.2"
```

## Quick Start

```bash
# Login (opens a browser — log in to your account, no API keys needed)
dravr-sciotte-server login

# List activities (fast, from the training page — paginated)
dravr-sciotte-server activities --limit 50

# List with full detail (navigates each activity page for HR, cadence, weather, device, etc.)
dravr-sciotte-server activities --limit 5 --detail --format json

# Auto-login + fetch in one command
dravr-sciotte-server activities --login --limit 10

# Start REST + MCP server
dravr-sciotte-server serve --port 3000

# Start MCP stdio transport (for Claude integration)
dravr-sciotte-server --transport stdio
```

## How It Works

1. **Browser login** — opens a visible Chrome window to the provider's login page. You log in normally. Session cookies are captured and encrypted at rest (AES-256-GCM).
2. **List page scraping** — navigates to the training/activity list page in headless Chrome, extracts activity rows using CSS selectors defined in the provider TOML.
3. **Pagination** — automatically clicks the "next page" button to load more than the initial 20 activities.
4. **Detail enrichment** (opt-in via `--detail`) — navigates into each activity page and extracts full metrics (HR, cadence, weather, device, gear) using a JS snippet from the provider TOML, including structured data from embedded JSON.
5. **Caching** — results are cached in-memory with configurable TTL (default 15 min).

## Credential Login

In addition to the interactive browser login, the library supports fully programmatic login with email and password. This flow runs headless Chrome, fills the login form automatically, and handles multi-factor authentication without any user interaction with a browser window.

### Login Methods

Three login flows are supported, selected via the `method` parameter:

| Method | Description |
|--------|-------------|
| `email` | Fill the provider's native email/password form directly (default) |
| `google` | Click the Google OAuth button, then fill Google's email/password form |
| `apple` | Click the Apple OAuth button, then fill Apple's sign-in form |

### 2FA Handling

The `credential_login` call returns one of four outcomes:

| Status | Meaning | Next Step |
|--------|---------|-----------|
| `authenticated` | Login succeeded, session is ready | Use the returned `session_id` |
| `otp_required` | Provider requires a one-time password or 2FA code | Call `POST /auth/submit-otp` |
| `two_factor_choice` | Provider shows multiple 2FA options | Call `POST /auth/select-2fa` with an `option_id` |
| `failed` | Wrong credentials or account locked | Check the `reason` field |

The browser session is kept alive between `credential_login` and `submit_otp` / `select_two_factor` calls, so the 2FA page remains open until you submit the code or select a method.

**Full 2FA flow example:**

```bash
# Step 1 — attempt login
curl -X POST http://localhost:3000/auth/login-with-credentials \
  -H "Content-Type: application/json" \
  -d '{"email": "you@example.com", "password": "s3cr3t", "method": "google"}'

# Response if 2FA method selection is needed:
# {"status":"two_factor_choice","options":[{"id":"otp","label":"Google Authenticator"},{"id":"app","label":"Tap Yes on your phone"}]}

# Step 2a — select a 2FA method (triggers the provider to send a code or prompt app approval)
curl -X POST http://localhost:3000/auth/select-2fa \
  -H "Content-Type: application/json" \
  -d '{"option_id": "otp"}'

# Response if a code is now required:
# {"status":"otp_required"}

# Step 2b — submit the OTP code
curl -X POST http://localhost:3000/auth/submit-otp \
  -H "Content-Type: application/json" \
  -d '{"code": "123456"}'

# Response on success:
# {"status":"authenticated","session_id":"...","cookie_count":12}
```

For the `app` method (phone tap), `select_two_factor` polls for up to `DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT` seconds (default 60 s) waiting for the user to approve on their phone, then returns `authenticated` directly without a code.

### WebSocket Browser Streaming

For cases where credential login is not suitable (CAPTCHA, unsupported provider, or user preference), the server also exposes a WebSocket endpoint that streams live Chrome screenshots to the client. The client can interact with the browser remotely — click, type, scroll — and the server detects login completion automatically.

```
GET /browser/login?method=direct
GET /browser/login?method=google
GET /browser/login?method=apple
```

If `DRAVR_SCIOTTE_API_KEY` is set, pass it as a query parameter since the browser WebSocket API cannot send custom headers:

```
GET /browser/login?token=my-secret
```

The WebSocket sends:
- Binary frames — JPEG screenshots of the Chrome viewport (1280×1024, ~12 fps)
- JSON text frames with `{"type":"status","state":"...","message":"..."}` during setup
- JSON text frame with `{"type":"login_success","session_id":"...","cookie_count":N}` on completion
- JSON text frame with `{"type":"login_failed","reason":"timeout"}` if the 120 s deadline is exceeded

The client sends JSON text frames to dispatch input:

```json
{"type":"click","x":640,"y":512}
{"type":"text","text":"my@email.com"}
{"type":"keydown","key":"Enter","code":"Enter"}
{"type":"scroll","x":640,"y":512,"deltaX":0,"deltaY":300}
```

## REST API Server (`dravr-sciotte-server`)

A unified HTTP server with built-in MCP support that serves scraped activity data. Supports `--transport stdio` for MCP-only mode (editor integration).

### Usage

```bash
# Start on localhost:3000
dravr-sciotte-server serve

# Specify port and host
dravr-sciotte-server serve --port 8080 --host 0.0.0.0

# MCP-only mode via stdio (for editor/client integration)
dravr-sciotte-server --transport stdio
```

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/auth/login` | Trigger browser login |
| `GET` | `/auth/status` | Check authentication (supports `X-Session-Id` header) |
| `GET` | `/auth/sessions` | List all active session IDs |
| `DELETE` | `/auth/sessions/{id}` | Remove a specific session |
| `POST` | `/auth/login-with-credentials` | Programmatic login with email/password |
| `POST` | `/auth/submit-otp` | Submit OTP/2FA code after `otp_required` |
| `POST` | `/auth/select-2fa` | Select a 2FA method after `two_factor_choice` |
| `GET` | `/browser/login` | WebSocket — stream Chrome frames for interactive login |
| `GET` | `/api/activities?limit=20` | List scraped activities |
| `GET` | `/api/activities/{id}` | Single activity detail |
| `GET` | `/health` | Health check with cache stats |
| `POST` | `/mcp` | MCP Streamable HTTP (JSON-RPC 2.0) |

Activity endpoints accept an optional `X-Session-Id` header to target a specific session when multiple sessions are active. Without the header, the most recently created session is used.

### MCP Streamable HTTP

The server also speaks [MCP](https://modelcontextprotocol.io/) at `POST /mcp`, accepting JSON-RPC 2.0 requests. Any MCP-compatible client can connect over HTTP instead of stdio.

```bash
# MCP initialize handshake
curl http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"curl"}}}'

# List available tools
curl http://localhost:3000/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
```

Add `Accept: text/event-stream` to receive SSE-wrapped responses instead of plain JSON.

### Authentication

Optional. Set `DRAVR_SCIOTTE_API_KEY` to require bearer token auth on all REST endpoints. When unset, all requests are allowed through (localhost development mode).

```bash
DRAVR_SCIOTTE_API_KEY=my-secret dravr-sciotte-server serve
curl http://localhost:3000/api/activities -H "Authorization: Bearer my-secret"
```

The `/browser/login` WebSocket endpoint accepts the token as a `?token=` query parameter instead of a header.

## MCP Server (`dravr-sciotte-mcp`)

A library and standalone binary that exposes the activity scraper via the [Model Context Protocol](https://modelcontextprotocol.io/). Connect any MCP-compatible client (Claude Desktop, Claude Code, editors, custom agents) to scrape sport activities.

### Usage

```bash
# Stdio transport (default — for editor/client integration)
dravr-sciotte-mcp

# HTTP transport (for network-accessible deployments)
dravr-sciotte-mcp --transport http --host 0.0.0.0 --port 3000
```

### MCP Tools

| Tool | Description |
|------|-------------|
| `auth_status` | Check if the session is authenticated and valid |
| `browser_login` | Open a browser window for the user to log in (no API keys needed) |
| `get_activities` | Scrape activities from the training page |
| `get_activity` | Scrape detailed data for a single activity by ID |
| `cache_status` | Get cache hit/miss statistics and entry counts |
| `cache_clear` | Clear all cached activity data |

### Client Configuration

Add to your MCP client config (e.g. Claude Desktop `claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "dravr-sciotte": {
      "command": "dravr-sciotte-mcp"
    }
  }
}
```

For Claude Code, add the same configuration to your MCP settings.

## Library Usage (Rust Trait)

```toml
[dependencies]
dravr-sciotte = "0.2"
```

### Browser login

```rust
use dravr_sciotte::{ChromeScraper, CachedScraper, ActivityScraper};
use dravr_sciotte::config::CacheConfig;
use dravr_sciotte::models::ActivityParams;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scraper = ChromeScraper::default_config();
    let cached = CachedScraper::new(scraper, &CacheConfig::default());

    // Browser login (opens Chrome, user logs in, cookies captured)
    let session = cached.browser_login().await?;

    // Scrape activities
    let params = ActivityParams { limit: Some(20), ..Default::default() };
    let activities = cached.get_activities(&session, &params).await?;

    for activity in &activities {
        println!("{}: {} ({})", activity.id, activity.name, activity.sport_type);
    }
    Ok(())
}
```

### Credential login with 2FA handling

```rust
use dravr_sciotte::{ChromeScraper, CachedScraper, ActivityScraper};
use dravr_sciotte::config::CacheConfig;
use dravr_sciotte::error::LoginResult;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scraper = ChromeScraper::default_config();
    let cached = CachedScraper::new(scraper, &CacheConfig::default());

    // Attempt programmatic login — method is "email", "google", or "apple"
    let result = cached.credential_login("you@example.com", "s3cr3t", "google").await?;

    let session = match result {
        LoginResult::Success(session) => session,

        LoginResult::TwoFactorChoice(options) => {
            // Provider returned multiple 2FA methods — select one
            println!("Choose a 2FA method:");
            for opt in &options {
                println!("  {} — {}", opt.id, opt.label);
            }
            // Select the first option; in practice, prompt the user
            let result = cached.select_two_factor(&options[0].id).await?;
            match result {
                LoginResult::Success(session) => session,
                LoginResult::OtpRequired => {
                    // Method requires a code (e.g., Google Authenticator)
                    let code = "123456"; // read from user input
                    match cached.submit_otp(code).await? {
                        LoginResult::Success(session) => session,
                        other => return Err(format!("Unexpected OTP result: {other:?}").into()),
                    }
                }
                other => return Err(format!("Unexpected 2FA result: {other:?}").into()),
            }
        }

        LoginResult::OtpRequired => {
            // Provider went straight to a code entry page
            let code = "123456"; // read from user input
            match cached.submit_otp(code).await? {
                LoginResult::Success(session) => session,
                other => return Err(format!("Unexpected OTP result: {other:?}").into()),
            }
        }

        LoginResult::Failed(reason) => {
            return Err(format!("Login failed: {reason}").into());
        }
    };

    println!("Authenticated, session_id={}", session.session_id);
    Ok(())
}
```

All scraping is driven by a TOML provider config. The `ActivityScraper` trait can be wrapped by platform crates (e.g. `pierre-scraper`) with error bridging, following the same pattern as embacle's `LlmProvider`.

## Provider Configuration

Scraping rules are defined in TOML files under `providers/`. The default provider is Strava (`providers/strava.toml`), compiled into the binary. A Garmin Connect provider (`providers/garmin.toml`) is also included.

```toml
[provider]
name = "strava"
login_url = "https://www.strava.com/login"
login_success_patterns = ["/dashboard", "/athlete", "/feed"]
login_failure_patterns = ["/login", "/session"]

# CSS selectors for the native email/password form (required for credential_login)
login_email_selector = '#email, input[name="email"]'
login_password_selector = '#password, input[name="password"]'
login_button_selector = 'button[type="submit"], #login-button'

# CSS selector for the login error message (used to detect wrong password)
login_error_selector = '.alert-error, .alert-danger, [class*="error-message"]'

# CSS selector for the OTP/2FA code input (required for submit_otp)
login_otp_selector = 'input[name="code"], input[type="tel"], input[autocomplete="one-time-code"]'

# OAuth button selectors — keys match the `method` parameter in credential_login
[provider.login_oauth_buttons]
google = 'text:Sign In With Google'
apple = 'text:Sign In With Apple'

[list_page]
url = "https://www.strava.com/athlete/training"
row_selector = "tr.training-activity-row"
link_selector = 'a[data-field-name="name"]'
id_regex = '/\/activities\/(\d+)/'

[list_page.fields]
name = 'a[data-field-name="name"]'
sport_type = 'td[data-field-name="sport_type"]'
date = "td.col-date"
time = 'td[data-field-name="time"]'
distance = "td.col-dist"
elevation = "td.col-elev"
suffer_score = "td.col-suffer-score"

[detail_page]
url_template = "https://www.strava.com/activities/{id}"
js_extract = '''
(function() { /* JS that extracts all activity data and returns JSON */ })()
'''
```

OAuth button selectors support a `text:` prefix for matching by button text content in addition to standard CSS selectors. For example, `text:Sign In With Google` clicks the first button, anchor, or `role=button` element whose text contains that string.

To add a new provider, create a TOML file with the same structure and load it via `ProviderConfig::from_file()`.

## Activity Data Model

Activities scraped from detail pages include:

| Category | Fields |
|----------|--------|
| Core | id, name, sport_type, start_date, duration_seconds |
| Distance | distance_meters, elevation_gain, pace, gap |
| Heart Rate | average_heart_rate, max_heart_rate |
| Power | average_power, max_power, normalized_power |
| Cadence | average_cadence |
| Speed | average_speed, max_speed |
| Training | suffer_score, calories, elapsed_time_seconds |
| Weather | temperature, feels_like, humidity, wind_speed, wind_direction, weather |
| Equipment | device_name, gear_name |
| Location | city, region, country |
| Other | perceived_exertion, sport_type_detail, workout_type |

## Environment Variables

All variables are optional. Unset variables use the defaults shown below.

### Server

| Variable | Default | Description |
|----------|---------|-------------|
| `DRAVR_SCIOTTE_API_KEY` | _(unset)_ | Bearer token required on all REST endpoints. When unset, no authentication is enforced. |
| `CHROME_PATH` | _(auto-detected)_ | Path to a Chrome or Chromium binary. |

### Timing (credential login and scraping)

| Variable | Default | Description |
|----------|---------|-------------|
| `DRAVR_SCIOTTE_PAGE_TIMEOUT` | `30` | Page load timeout in seconds. |
| `DRAVR_SCIOTTE_INTERACTION_DELAY_MS` | `500` | Delay between page interactions in milliseconds. |
| `DRAVR_SCIOTTE_LOGIN_POLL_INTERVAL_MS` | `500` | Interval between URL polls during login detection in milliseconds. |
| `DRAVR_SCIOTTE_LOGIN_TIMEOUT` | `120` | Overall browser login timeout in seconds (interactive mode). |
| `DRAVR_SCIOTTE_PAGE_LOAD_WAIT` | `3` | Wait time after navigation for JS to render, in seconds. |
| `DRAVR_SCIOTTE_FORM_DELAY_MS` | `300` | Delay between form field interactions in milliseconds. |
| `DRAVR_SCIOTTE_EMAIL_STEP_TIMEOUT` | `10` | Timeout waiting for the password field to appear after email submit, in seconds. |
| `DRAVR_SCIOTTE_PASSWORD_STEP_TIMEOUT` | `10` | Timeout waiting for login result after password submit, in seconds. |
| `DRAVR_SCIOTTE_PHONE_TAP_TIMEOUT` | `60` | Timeout waiting for phone tap / app approval during 2FA, in seconds. |

### Cache and Session

| Variable | Default | Description |
|----------|---------|-------------|
| `DRAVR_SCIOTTE_CACHE_TTL` | `900` | Activity cache TTL in seconds (15 minutes). |
| `DRAVR_SCIOTTE_CACHE_MAX` | `100` | Maximum number of cached activity entries. |
| `DRAVR_SCIOTTE_SESSION_DIR` | `~/.config/dravr-sciotte` | Directory where encrypted session files are stored. |

## Docker

Pull the image from GitHub Container Registry:

```bash
docker pull ghcr.io/dravr-ai/dravr-sciotte:latest
```

The image includes `dravr-sciotte-server`, `dravr-sciotte-mcp`, and Chromium for headless scraping.

```bash
# Start the REST + MCP server
docker run -p 3000:3000 ghcr.io/dravr-ai/dravr-sciotte

# Mount session directory for persistent login
docker run -p 3000:3000 \
  -v ~/.config/dravr-sciotte:/home/dravr/.config/dravr-sciotte \
  ghcr.io/dravr-ai/dravr-sciotte

# Run the MCP server
docker run --entrypoint dravr-sciotte-mcp ghcr.io/dravr-ai/dravr-sciotte
```

## Architecture

```
Your Application
    └── dravr-sciotte (this library)
            │
            ├── Provider Config (TOML-driven)
            │   ├── providers/strava.toml  → login URLs, CSS selectors, JS extraction
            │   └── providers/garmin.toml  → Garmin Connect variant
            │
            ├── Chrome Scraper (chromiumoxide CDP)
            │   ├── browser_login()        → visible Chrome, user logs in, cookies captured
            │   ├── credential_login()     → headless Chrome, fills form, handles OAuth
            │   ├── submit_otp()           → submits OTP/2FA code on pending login page
            │   ├── select_two_factor()    → selects 2FA method on pending login page
            │   ├── get_activities()       → headless Chrome, list page + pagination
            │   └── get_activity()         → headless Chrome, detail page JS extraction
            │
            ├── Cache Layer (moka TTL cache)
            │   └── CachedScraper         → wraps ActivityScraper with in-memory TTL cache
            │
            ├── Auth Persistence (AES-256-GCM)
            │   └── ~/.config/dravr-sciotte/session.enc
            │
            ├── Multi-Session Store
            │   └── ServerState           → HashMap<session_id, AuthSession>, X-Session-Id routing
            │
            ├── MCP Server (library + binary crate)
            │   └── dravr-sciotte-mcp     → JSON-RPC 2.0 over stdio or HTTP/SSE
            │
            └── Unified REST API + MCP + CLI (binary crate)
                └── dravr-sciotte-server  → REST endpoints, MCP HTTP, WebSocket streaming, CLI
```

The core `ActivityScraper` trait:
- **`browser_login()`** — open browser, capture session
- **`credential_login()`** — programmatic login with email/password and OAuth support
- **`submit_otp()`** — submit OTP/2FA code after `credential_login` returned `OtpRequired`
- **`select_two_factor()`** — select a 2FA method after `credential_login` returned `TwoFactorChoice`
- **`get_activities()`** — scrape activity list with pagination
- **`get_activity()`** — scrape single activity detail
- **`is_authenticated()`** — check session validity

For detailed API docs see [docs.rs/dravr-sciotte](https://docs.rs/dravr-sciotte).

## License

Licensed under MIT OR Apache-2.0.

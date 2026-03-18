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
- [REST API Server](#rest-api-server-dravr-sciotte-server)
- [MCP Server](#mcp-server-dravr-sciotte-mcp)
- [Library Usage](#library-usage-rust-trait)
- [Provider Configuration](#provider-configuration)
- [Activity Data Model](#activity-data-model)
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
dravr-sciotte = "0.1"
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
| `GET` | `/auth/status` | Check authentication |
| `GET` | `/api/activities?limit=20` | List scraped activities |
| `GET` | `/api/activities/{id}` | Single activity detail |
| `GET` | `/health` | Health check with cache stats |
| `POST` | `/mcp` | MCP Streamable HTTP (JSON-RPC 2.0) |

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

Optional. Set `DRAVR_SCIOTTE_API_KEY` to require bearer token auth on all endpoints. When unset, all requests are allowed through (localhost development mode).

```bash
DRAVR_SCIOTTE_API_KEY=my-secret dravr-sciotte-server serve
curl http://localhost:3000/api/activities -H "Authorization: Bearer my-secret"
```

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
dravr-sciotte = "0.1"
```

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

All scraping is driven by a TOML provider config. The `ActivityScraper` trait can be wrapped by platform crates (e.g. `pierre-scraper`) with error bridging, following the same pattern as embacle's `LlmProvider`.

## Provider Configuration

Scraping rules are defined in TOML files under `providers/`. The default provider is Strava (`providers/strava.toml`), compiled into the binary.

```toml
[provider]
name = "strava"
login_url = "https://www.strava.com/login"
login_success_patterns = ["/dashboard", "/athlete", "/feed"]
login_failure_patterns = ["/login", "/session"]

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
            │   └── providers/strava.toml → login URLs, CSS selectors, JS extraction
            │
            ├── Chrome Scraper (chromiumoxide CDP)
            │   ├── browser_login()     → visible Chrome, user logs in, cookies captured
            │   ├── get_activities()    → headless Chrome, list page + pagination
            │   └── get_activity()      → headless Chrome, detail page JS extraction
            │
            ├── Cache Layer (moka TTL cache)
            │   └── CachedScraper      → wraps ActivityScraper with in-memory TTL cache
            │
            ├── Auth Persistence (AES-256-GCM)
            │   └── ~/.config/dravr-sciotte/session.enc
            │
            ├── MCP Server (library + binary crate)
            │   └── dravr-sciotte-mcp  → JSON-RPC 2.0 over stdio or HTTP/SSE
            │
            └── Unified REST API + MCP + CLI (binary crate)
                └── dravr-sciotte-server → REST endpoints, MCP HTTP, CLI commands
```

The core `ActivityScraper` trait:
- **`browser_login()`** — open browser, capture session
- **`get_activities()`** — scrape activity list with pagination
- **`get_activity()`** — scrape single activity detail
- **`is_authenticated()`** — check session validity

For detailed API docs see [docs.rs/dravr-sciotte](https://docs.rs/dravr-sciotte).

## License

Licensed under MIT OR Apache-2.0.

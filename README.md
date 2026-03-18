# dravr-sciotte

Sport activity scraper with headless Chrome, TOML-configurable providers, and in-memory caching.

Logs into sport platforms via a browser (no API credentials needed), scrapes training data from activity pages, and exposes it through four integration surfaces: Rust trait, REST API, MCP server, and CLI.

## Quick Start

```bash
# Login (opens a browser — log in to your account)
dravr-sciotte-server login

# List activities (fast, from the training page)
dravr-sciotte-server activities --limit 20

# List with full detail (navigates each activity page for HR, cadence, weather, device, etc.)
dravr-sciotte-server activities --limit 5 --detail --format json

# Start REST + MCP server
dravr-sciotte-server serve --port 3000

# Start MCP stdio transport (for Claude integration)
dravr-sciotte-server --transport stdio
```

## How It Works

1. **Browser login** — opens a visible Chrome window to the provider's login page. You log in normally. Session cookies are captured and encrypted at rest.
2. **List page scraping** — navigates to the training/activity list page in headless Chrome, extracts activity rows using CSS selectors defined in the provider TOML.
3. **Detail enrichment** (opt-in via `--detail`) — navigates into each activity page and extracts full metrics using a JS snippet from the provider TOML.
4. **Caching** — results are cached in-memory with configurable TTL (default 15 min).

## Provider Configuration

Scraping rules are defined in TOML files under `providers/`. The default provider is Strava (`providers/strava.toml`), compiled into the binary.

### Example: Strava (`providers/strava.toml`)

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
(function() {
    // ... JS that extracts activity data and returns JSON ...
})()
'''
```

To add a new provider, create a TOML file with the same structure and load it via `ProviderConfig::from_file()`.

## Integration Modes

### CLI

```bash
dravr-sciotte-server login                          # Browser login
dravr-sciotte-server activities --limit 10          # List activities
dravr-sciotte-server activities --detail --format json  # Full detail as JSON
dravr-sciotte-server auth-status                    # Check session
dravr-sciotte-server serve --port 3000              # Start REST server
```

### REST API

| Method | Path | Description |
|--------|------|-------------|
| POST | `/auth/login` | Trigger browser login |
| GET | `/auth/status` | Check authentication |
| GET | `/api/activities?limit=20` | List activities |
| GET | `/api/activities/{id}` | Activity detail |
| GET | `/health` | Health check |
| POST | `/mcp` | MCP HTTP transport |

### MCP (Model Context Protocol)

6 tools available via stdio or HTTP transport:

| Tool | Description |
|------|-------------|
| `auth_status` | Check session status |
| `browser_login` | Open browser for login |
| `get_activities` | Scrape activity list |
| `get_activity` | Scrape single activity detail |
| `cache_status` | Cache hit/miss stats |
| `cache_clear` | Clear cached data |

### Rust Trait

```rust
use dravr_sciotte::{ChromeScraper, CachedScraper, ActivityScraper};
use dravr_sciotte::config::CacheConfig;

let scraper = ChromeScraper::default_config();
let cached = CachedScraper::new(scraper, &CacheConfig::default());

let session = cached.browser_login().await?;
let activities = cached.get_activities(&session, &params).await?;
```

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

## Architecture

```
dravr-sciotte/
├── providers/strava.toml          # Provider config (selectors, JS, URLs)
├── src/                           # Core library
│   ├── provider.rs                # TOML config loading and JS generation
│   ├── scraper.rs                 # Chrome-based scraping engine
│   ├── models.rs                  # Activity data model
│   ├── cache.rs                   # In-memory TTL cache
│   ├── auth.rs                    # Session encryption/persistence
│   └── types.rs                   # ActivityScraper trait
├── crates/dravr-sciotte-mcp/      # MCP server (stdio + HTTP)
└── crates/dravr-sciotte-server/   # REST API + CLI
```

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `CHROME_PATH` | Path to Chrome/Chromium binary | auto-detect |
| `DRAVR_SCIOTTE_API_KEY` | Bearer token for REST auth | none (open) |
| `DRAVR_SCIOTTE_CACHE_TTL` | Cache TTL in seconds | 900 (15 min) |
| `DRAVR_SCIOTTE_CACHE_MAX` | Max cache entries | 100 |
| `DRAVR_SCIOTTE_SESSION_DIR` | Session storage directory | `~/.config/dravr-sciotte/` |

## License

MIT OR Apache-2.0

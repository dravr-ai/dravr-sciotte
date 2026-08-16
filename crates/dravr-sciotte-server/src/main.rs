// ABOUTME: Unified CLI entry point for dravr-sciotte server and commands
// ABOUTME: Supports serve (REST+MCP), login (browser), activities (scrape), and MCP stdio
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::str_to_string
    )
)]

use std::error::Error;
use std::io::Error as IoError;
use std::path::Path;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use dravr_sciotte::auth;
use dravr_sciotte::cache::CachedScraper;
#[cfg(feature = "vision")]
use dravr_sciotte::config::LoginMode;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::models::{Activity, ActivityParams};
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::queue::{QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::state::AppScraper;
use dravr_sciotte_mcp::state::LoginScraperDecorator;
use dravr_sciotte_mcp::{build_tool_registry, ServerState};
use dravr_sciotte_server::router;
use dravr_tronc::mcp::transport::stdio;
use dravr_tronc::server::tracing_init;
use dravr_tronc::McpServer;
use tokio::net::TcpListener;
use tracing::info;

#[derive(Parser)]
#[command(
    name = "dravr-sciotte-server",
    version,
    about = "Sport activity scraper"
)]
struct Cli {
    /// Provider config file(s). Repeatable for `serve` (one server, several
    /// providers). No flag: `serve` loads the built-in strava + garmin pair;
    /// single-provider CLI commands (login, activities) default to strava.
    #[arg(long, short, global = true, action = clap::ArgAction::Append)]
    provider: Vec<String>,

    /// Transport mode for MCP (when no subcommand)
    #[arg(long, default_value = "http")]
    transport: String,

    /// HTTP host
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP port
    #[arg(long, default_value = "3000")]
    port: u16,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the REST + MCP HTTP server
    Serve {
        /// HTTP host
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// HTTP port
        #[arg(long, default_value = "3000")]
        port: u16,
    },
    /// Login to Strava (opens a browser window)
    Login,
    /// Scrape and display activities (auto-login if needed)
    Activities {
        /// Maximum number of activities
        #[arg(long, default_value = "20")]
        limit: u32,
        /// Filter by sport type
        #[arg(long)]
        sport_type: Option<String>,
        /// Only activities on/after this date (YYYY-MM-DD); pages the feed back to it
        #[arg(long)]
        after: Option<String>,
        /// Only activities strictly before this date (YYYY-MM-DD)
        #[arg(long)]
        before: Option<String>,
        /// Output format
        #[arg(long, default_value = "table")]
        format: String,
        /// Force re-login even if a session exists
        #[arg(long)]
        login: bool,
        /// Navigate into each activity detail page for full metrics (HR, cadence, weather, etc.)
        #[arg(long)]
        detail: bool,
    },
    /// Check authentication status
    AuthStatus,
    /// Clear the activity cache
    CacheClear,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    tracing_init::init_with_notifications(&cli.transport);

    match cli.command {
        Some(Command::Serve { host, port }) => {
            run_server(host, port, load_provider_configs(&cli.provider)?).await
        }
        Some(Command::Login) => run_login(load_single_provider_config(&cli.provider)?).await,
        Some(Command::Activities {
            limit,
            sport_type,
            after,
            before,
            format,
            login,
            detail,
        }) => {
            let params = build_activity_params(limit, sport_type, after, before, detail);
            run_activities(
                params,
                format,
                login,
                load_single_provider_config(&cli.provider)?,
            )
            .await
        }
        Some(Command::AuthStatus) => run_auth_status().await,
        Some(Command::CacheClear) => {
            run_cache_clear();
            Ok(())
        }
        None => {
            if cli.transport == "stdio" {
                run_mcp_stdio(load_provider_configs(&cli.provider)?).await
            } else {
                run_server(cli.host, cli.port, load_provider_configs(&cli.provider)?).await
            }
        }
    }
}

/// Load the provider configs a server instance will serve.
///
/// Explicit `--provider` files load verbatim; no flag loads the built-in
/// strava + garmin pair, so a bare `serve` is multi-provider out of the box
/// (one service serves every sciotte-scraped provider — ADR-021).
fn load_provider_configs(
    paths: &[String],
) -> Result<Vec<ProviderConfig>, Box<dyn Error + Send + Sync>> {
    if paths.is_empty() {
        return Ok(vec![
            ProviderConfig::strava_default()?,
            ProviderConfig::garmin_default()?,
        ]);
    }
    paths
        .iter()
        .map(|p| {
            info!(provider = %p, "Loading provider config");
            Ok(ProviderConfig::from_file(Path::new(p))?)
        })
        .collect()
}

/// Single-provider CLI commands (login, activities): the first `--provider`
/// file, or the built-in Strava default when none is given.
fn load_single_provider_config(
    paths: &[String],
) -> Result<ProviderConfig, Box<dyn Error + Send + Sync>> {
    match paths.first() {
        Some(p) => {
            info!(provider = %p, "Loading provider config");
            Ok(ProviderConfig::from_file(Path::new(p))?)
        }
        None => Ok(ProviderConfig::strava_default()?),
    }
}

/// Build the shared backpressure limiter from `DRAVR_SCIOTTE_*` environment
/// variables. The crate ships no numeric defaults — operators must supply
/// every knob via env (Terraform / .envrc in production). Fail fast on
/// missing or malformed config so the binary never silently papers over
/// bad infra.
fn build_limiter() -> Result<Arc<SciotteLimiter>, Box<dyn Error + Send + Sync>> {
    let config = QueueConfig::from_env().map_err(|e| -> Box<dyn Error + Send + Sync> {
        Box::new(IoError::other(format!(
            "invalid sciotte queue configuration: {e}"
        )))
    })?;
    info!(
        max_concurrent = config.max_concurrent,
        max_queue_depth = config.max_queue_depth,
        acquire_timeout_secs = config.acquire_timeout.as_secs(),
        parked_permit_ttl_secs = config.parked_permit_ttl.as_secs(),
        watchdog_interval_secs = config.watchdog_interval.as_secs(),
        retry_after_hint_secs = config.retry_after_hint.as_secs(),
        closed_retry_after_secs = config.closed_retry_after.as_secs(),
        "Sciotte queue configuration loaded from environment"
    );
    Ok(SciotteLimiter::new(config))
}

/// Build one provider's scrape-plane scraper, applying the vision decorator
/// when one is enabled (vision matters on the login fallbacks the scrape
/// plane's `browser_login` path can still hit).
fn create_scraper(
    provider: ProviderConfig,
    limiter: Arc<SciotteLimiter>,
    decorator: Option<&LoginScraperDecorator>,
) -> AppScraper {
    let config = ScraperConfig::default();
    let mut chrome = ChromeScraper::new(config, provider);
    if let Some(decorate) = decorator {
        chrome = decorate(chrome);
    }
    let queued = QueuedScraper::new(chrome, limiter);
    CachedScraper::new(queued, &CacheConfig::default())
}

/// Adapts an embacle `CopilotHeadlessRunner` to sciotte's `VisionModel` trait,
/// keeping the embacle dependency on the consumer side (sciotte itself no
/// longer depends on embacle).
#[cfg(feature = "vision")]
struct EmbacleVisionModel(Arc<embacle::CopilotHeadlessRunner>);

#[cfg(feature = "vision")]
#[async_trait::async_trait]
impl dravr_sciotte::VisionModel for EmbacleVisionModel {
    async fn analyze_screenshot(
        &self,
        prompt: &str,
        screenshot_png_b64: &str,
    ) -> Result<String, dravr_sciotte::VisionModelError> {
        use embacle::types::{ChatMessage, ChatRequest, ImagePart, LlmProvider};

        let image = ImagePart::new(screenshot_png_b64.to_owned(), "image/png")
            .map_err(|e| format!("invalid image part: {e}"))?;
        let message = ChatMessage::user_with_images(prompt.to_owned(), vec![image]);
        let request = ChatRequest {
            messages: vec![message],
            model: None,
            temperature: Some(0.0),
            max_tokens: Some(4096),
            stream: false,
            tools: None,
            tool_choice: None,
            top_p: None,
            stop: None,
            response_format: None,
            turn_id: None,
        };
        let response = self
            .0
            .complete(&request)
            .await
            .map_err(|e| format!("vision LLM call failed: {e}"))?;
        Ok(response.content)
    }
}

/// Build the vision decorator when the configured login mode uses it: a
/// closure attaching one shared Copilot-headless vision model to every
/// scrape-plane scraper and every ephemeral login-flow scraper.
#[cfg(feature = "vision")]
fn build_login_decorator() -> Option<LoginScraperDecorator> {
    let config = ScraperConfig::default();
    if matches!(config.login_mode, LoginMode::Vision | LoginMode::Hybrid) {
        let headless_config = embacle::CopilotHeadlessConfig::from_env();
        info!(login_mode = ?config.login_mode, "Initializing Copilot Headless LLM for vision login...");
        let llm = Arc::new(embacle::CopilotHeadlessRunner::with_config(headless_config));
        let vision: Arc<dyn dravr_sciotte::VisionModel> = Arc::new(EmbacleVisionModel(llm));
        Some(Box::new(move |scraper: ChromeScraper| {
            scraper.with_llm(Arc::clone(&vision))
        }))
    } else {
        None
    }
}

/// Vision feature disabled: no decorator, selector login only.
#[cfg(not(feature = "vision"))]
fn build_login_decorator() -> Option<LoginScraperDecorator> {
    None
}

/// Build the shared server state serving every loaded provider (ADR-021:
/// one multi-tenant, multi-provider instance).
fn build_state(providers: Vec<ProviderConfig>, limiter: &Arc<SciotteLimiter>) -> Arc<ServerState> {
    let decorator = build_login_decorator();
    let pairs: Vec<(ProviderConfig, AppScraper)> = providers
        .into_iter()
        .map(|provider| {
            let scraper = create_scraper(provider.clone(), Arc::clone(limiter), decorator.as_ref());
            (provider, scraper)
        })
        .collect();
    Arc::new(ServerState::new(
        pairs,
        Arc::clone(limiter),
        decorator,
        ScraperConfig::default(),
    ))
}

/// Load the disk-persisted session into the store — only meaningful for a
/// single-provider instance: the session file carries no provider tag, so a
/// multi-provider server relies on the platform re-importing its
/// sessions-of-record instead (ADR-021).
async fn load_persisted_session(state: &Arc<ServerState>) {
    if !state.single_provider() {
        return;
    }
    if let Ok(Some(session)) = auth::load_session().await {
        if let Ok(provider) = state.resolve_provider(None) {
            info!("Loaded persisted session");
            state.add_session(session, provider).await;
        }
    }
}

async fn run_server(
    host: String,
    port: u16,
    providers: Vec<ProviderConfig>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let watchdog = limiter.spawn_watchdog();

    let state = build_state(providers, &limiter);
    info!(providers = ?state.provider_names(), "Serving providers");
    // Reap abandoned interactive logins on the same cadence and TTL as the
    // limiter's parked permits — each stale flow frees a browser and a slot.
    let flow_reaper = state.spawn_login_flow_reaper(
        limiter.config().parked_permit_ttl,
        limiter.config().watchdog_interval,
    );

    load_persisted_session(&state).await;

    let app = router::build_router(state);

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!(address = %addr, "Server listening");
    let serve_result = axum::serve(listener, app).await;
    watchdog.abort();
    flow_reaper.abort();
    serve_result?;
    Ok(())
}

async fn run_mcp_stdio(providers: Vec<ProviderConfig>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let watchdog = limiter.spawn_watchdog();

    let state = build_state(providers, &limiter);
    load_persisted_session(&state).await;

    let server = Arc::new(McpServer::new(
        "dravr-sciotte",
        env!("CARGO_PKG_VERSION"),
        build_tool_registry(),
        state,
    ));
    let result = stdio::run(server).await;
    watchdog.abort();
    result
}

async fn run_login(provider: ProviderConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let cached = create_scraper(provider, limiter, build_login_decorator().as_ref());

    println!("Opening browser for login...");
    println!("Complete the login in the browser window that opens.");
    println!("The browser will close automatically once login is detected.\n");

    let session = cached.browser_login().await?;

    auth::save_session(&session).await?;

    println!("Login successful! Session saved.");
    println!("Session ID: {}", session.session_id);
    println!("Cookies captured: {}", session.cookies.len());
    Ok(())
}

/// Build the activity query from CLI args, parsing `--after`/`--before` as
/// `YYYY-MM-DD` (midnight UTC). An unparseable date is treated as unbounded on
/// that side so a typo widens the window rather than aborting the scrape.
fn build_activity_params(
    limit: u32,
    sport_type: Option<String>,
    after: Option<String>,
    before: Option<String>,
    detail: bool,
) -> ActivityParams {
    let parse_day = |s: Option<String>| -> Option<chrono::DateTime<chrono::Utc>> {
        s.and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
            .and_then(|nd| nd.and_hms_opt(0, 0, 0))
            .map(|ndt| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(ndt, chrono::Utc))
    };
    ActivityParams {
        limit: Some(limit),
        sport_type,
        after: parse_day(after),
        before: parse_day(before),
        enrich_details: detail,
        ..Default::default()
    }
}

async fn run_activities(
    params: ActivityParams,
    format: String,
    force_login: bool,
    provider: ProviderConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let cached = create_scraper(provider, limiter, build_login_decorator().as_ref());

    let session = if force_login {
        println!("Opening browser for login...");
        let s = cached.browser_login().await?;
        auth::save_session(&s).await?;
        println!("Login successful!\n");
        s
    } else if let Some(s) = auth::load_session().await? {
        s
    } else {
        println!("No saved session — opening browser for Strava login...");
        let s = cached.browser_login().await?;
        auth::save_session(&s).await?;
        println!("Login successful!\n");
        s
    };

    println!("Scraping activities...");
    let activities = cached.get_activities(&session, &params).await?;

    if activities.is_empty() {
        println!("No activities found.");
        return Ok(());
    }

    if format.as_str() == "json" {
        println!("{}", serde_json::to_string_pretty(&activities)?);
    } else {
        print_activity_table(&activities);
    }

    Ok(())
}

fn print_activity_table(activities: &[Activity]) {
    println!(
        "{:<12} {:<30} {:<15} {:<12} {:<10} {:<8}",
        "ID", "Name", "Type", "Date", "Distance", "Time"
    );
    println!("{}", "-".repeat(87));
    for a in activities {
        let distance = a
            .distance_meters
            .map_or_else(|| "--".to_owned(), |d| format!("{:.1} km", d / 1000.0));
        let duration = format_duration(a.duration_seconds);
        let date = a.start_date.format("%Y-%m-%d").to_string();
        let name: String = a.name.chars().take(28).collect();

        println!(
            "{:<12} {:<30} {:<15} {:<12} {:<10} {:<8}",
            a.id,
            name,
            a.sport_type.display_name(),
            date,
            distance,
            duration
        );
    }
    println!("\n{} activities found.", activities.len());
}

async fn run_auth_status() -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(session) = auth::load_session().await? {
        println!("Authenticated: yes");
        println!("Session ID: {}", session.session_id);
        println!("Created: {}", session.created_at);
        println!("Cookies: {}", session.cookies.len());
        if let Some(expires) = session.expires_at {
            println!("Expires: {expires}");
        }
    } else {
        println!("Authenticated: no");
        println!("Run 'dravr-sciotte-server login' to authenticate.");
    }
    Ok(())
}

fn run_cache_clear() {
    // Cache is in-memory only — each CLI invocation starts fresh
    println!("Cache cleared (note: CLI cache is per-invocation).");
}

fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins}:{secs:02}")
    }
}

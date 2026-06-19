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
    /// Provider config file (default: built-in strava)
    #[arg(long, short, global = true)]
    provider: Option<String>,

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

    let provider = load_provider_config(cli.provider.as_deref())?;

    match cli.command {
        Some(Command::Serve { host, port }) => run_server(host, port, provider).await,
        Some(Command::Login) => run_login(provider).await,
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
            run_activities(params, format, login, provider).await
        }
        Some(Command::AuthStatus) => run_auth_status().await,
        Some(Command::CacheClear) => {
            run_cache_clear();
            Ok(())
        }
        None => {
            if cli.transport == "stdio" {
                run_mcp_stdio(provider).await
            } else {
                run_server(cli.host, cli.port, provider).await
            }
        }
    }
}

/// Load provider config from a TOML file path, or use the built-in Strava default
fn load_provider_config(
    path: Option<&str>,
) -> Result<ProviderConfig, Box<dyn Error + Send + Sync>> {
    match path {
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

fn create_scraper(provider: ProviderConfig, limiter: Arc<SciotteLimiter>) -> AppScraper {
    let config = ScraperConfig::default();
    let chrome = ChromeScraper::new(config, provider);
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

/// Create a scraper with vision-based login via Copilot Headless LLM
#[cfg(feature = "vision")]
fn create_vision_scraper(provider: ProviderConfig, limiter: Arc<SciotteLimiter>) -> AppScraper {
    let headless_config = embacle::CopilotHeadlessConfig::from_env();
    info!("Initializing Copilot Headless LLM for vision login...");
    let llm = Arc::new(embacle::CopilotHeadlessRunner::with_config(headless_config));

    let config = ScraperConfig::default();
    info!(login_mode = ?config.login_mode, "Vision scraper ready");
    let chrome = ChromeScraper::new(config, provider).with_llm(Arc::new(EmbacleVisionModel(llm)));
    let queued = QueuedScraper::new(chrome, limiter);
    CachedScraper::new(queued, &CacheConfig::default())
}

async fn run_server(
    host: String,
    port: u16,
    provider: ProviderConfig,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let watchdog = limiter.spawn_watchdog();

    #[cfg(feature = "vision")]
    let cached = {
        let config = ScraperConfig::default();
        if matches!(config.login_mode, LoginMode::Vision | LoginMode::Hybrid) {
            create_vision_scraper(provider, Arc::clone(&limiter))
        } else {
            create_scraper(provider, Arc::clone(&limiter))
        }
    };
    #[cfg(not(feature = "vision"))]
    let cached = create_scraper(provider, Arc::clone(&limiter));
    let state = Arc::new(ServerState::new(cached, limiter));

    if let Ok(Some(session)) = auth::load_session().await {
        info!("Loaded persisted session");
        state.set_session(session).await;
    }

    let app = router::build_router(state);

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    info!(address = %addr, "Server listening");
    let serve_result = axum::serve(listener, app).await;
    watchdog.abort();
    serve_result?;
    Ok(())
}

async fn run_mcp_stdio(provider: ProviderConfig) -> Result<(), Box<dyn Error + Send + Sync>> {
    let limiter = build_limiter()?;
    let watchdog = limiter.spawn_watchdog();

    let cached = create_scraper(provider, Arc::clone(&limiter));
    let state = Arc::new(ServerState::new(cached, limiter));

    if let Ok(Some(session)) = auth::load_session().await {
        state.set_session(session).await;
    }

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
    let cached = create_scraper(provider, limiter);

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
    let cached = create_scraper(provider, limiter);

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

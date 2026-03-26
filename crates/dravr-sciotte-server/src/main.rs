// ABOUTME: Unified CLI entry point for dravr-sciotte server and commands
// ABOUTME: Supports serve (REST+MCP), login (browser), activities (scrape), and MCP stdio
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::path::Path;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::RwLock;
use tracing::info;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::provider::ProviderConfig;
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte::ActivityScraper;
use dravr_sciotte_mcp::{build_tool_registry, ServerState};

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
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    dravr_tronc::server::tracing_init::init_with_notifications(&cli.transport);

    let provider = load_provider_config(cli.provider.as_deref())?;

    match cli.command {
        Some(Command::Serve { host, port }) => run_server(host, port, provider).await,
        Some(Command::Login) => run_login(provider).await,
        Some(Command::Activities {
            limit,
            sport_type,
            format,
            login,
            detail,
        }) => run_activities(limit, sport_type, format, login, detail, provider).await,
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
) -> Result<ProviderConfig, Box<dyn std::error::Error + Send + Sync>> {
    match path {
        Some(p) => {
            info!(provider = %p, "Loading provider config");
            Ok(ProviderConfig::from_file(Path::new(p))?)
        }
        None => Ok(ProviderConfig::strava_default()),
    }
}

fn create_scraper(provider: ProviderConfig) -> CachedScraper<ChromeScraper> {
    let config = ScraperConfig::default();
    let scraper = ChromeScraper::new(config, provider);
    CachedScraper::new(scraper, &CacheConfig::default())
}

/// Create a scraper with vision-based login via Copilot Headless LLM
#[cfg(feature = "vision")]
async fn create_vision_scraper(
    provider: ProviderConfig,
) -> Result<CachedScraper<ChromeScraper>, Box<dyn std::error::Error + Send + Sync>> {
    use std::sync::Arc;

    let headless_config = embacle::CopilotHeadlessConfig::from_env();
    info!("Initializing Copilot Headless LLM for vision login...");
    let llm = Arc::new(embacle::CopilotHeadlessRunner::with_config(headless_config).await);

    let config = ScraperConfig::default();
    info!(login_mode = ?config.login_mode, "Vision scraper ready");
    let scraper = ChromeScraper::new(config, provider).with_llm(llm);
    Ok(CachedScraper::new(scraper, &CacheConfig::default()))
}

async fn run_server(
    host: String,
    port: u16,
    provider: ProviderConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "vision")]
    let cached = {
        let config = ScraperConfig::default();
        if matches!(
            config.login_mode,
            dravr_sciotte::config::LoginMode::Vision | dravr_sciotte::config::LoginMode::Hybrid
        ) {
            create_vision_scraper(provider).await?
        } else {
            create_scraper(provider)
        }
    };
    #[cfg(not(feature = "vision"))]
    let cached = create_scraper(provider);
    let state = Arc::new(RwLock::new(ServerState::new(cached)));

    if let Ok(Some(session)) = dravr_sciotte::auth::load_session().await {
        info!("Loaded persisted session");
        state.write().await.set_session(session);
    }

    let app = dravr_sciotte_server::router::build_router(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(address = %addr, "Server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_mcp_stdio(
    provider: ProviderConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cached = create_scraper(provider);
    let state = Arc::new(RwLock::new(ServerState::new(cached)));

    if let Ok(Some(session)) = dravr_sciotte::auth::load_session().await {
        state.write().await.set_session(session);
    }

    let server = Arc::new(dravr_tronc::McpServer::new(
        "dravr-sciotte",
        env!("CARGO_PKG_VERSION"),
        build_tool_registry(),
        state,
    ));
    dravr_tronc::mcp::transport::stdio::run(server).await
}

async fn run_login(
    provider: ProviderConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cached = create_scraper(provider);

    println!("Opening browser for login...");
    println!("Complete the login in the browser window that opens.");
    println!("The browser will close automatically once login is detected.\n");

    let session = cached.browser_login().await?;

    dravr_sciotte::auth::save_session(&session).await?;

    println!("Login successful! Session saved.");
    println!("Session ID: {}", session.session_id);
    println!("Cookies captured: {}", session.cookies.len());
    Ok(())
}

async fn run_activities(
    limit: u32,
    sport_type: Option<String>,
    format: String,
    force_login: bool,
    detail: bool,
    provider: ProviderConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cached = create_scraper(provider);

    let session = if force_login {
        println!("Opening browser for login...");
        let s = cached.browser_login().await?;
        dravr_sciotte::auth::save_session(&s).await?;
        println!("Login successful!\n");
        s
    } else if let Some(s) = dravr_sciotte::auth::load_session().await? {
        s
    } else {
        println!("No saved session — opening browser for Strava login...");
        let s = cached.browser_login().await?;
        dravr_sciotte::auth::save_session(&s).await?;
        println!("Login successful!\n");
        s
    };

    let params = ActivityParams {
        limit: Some(limit),
        sport_type,
        enrich_details: detail,
        ..Default::default()
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

fn print_activity_table(activities: &[dravr_sciotte::models::Activity]) {
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

async fn run_auth_status() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let Some(session) = dravr_sciotte::auth::load_session().await? {
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

// ABOUTME: Unified CLI entry point for dravr-sciotte server and commands
// ABOUTME: Supports serve (REST+MCP), login (browser), activities (scrape), and MCP stdio
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::RwLock;
use tracing::info;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte::StravaScraper;
use dravr_sciotte_mcp::transport::stdio::StdioTransport;
use dravr_sciotte_mcp::transport::McpTransport;
use dravr_sciotte_mcp::{build_tool_registry, McpServer, ServerState};

#[derive(Parser)]
#[command(
    name = "dravr-sciotte-server",
    version,
    about = "Strava training data scraper"
)]
struct Cli {
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
    /// Scrape and display activities from Strava (auto-login if needed)
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
    },
    /// Check authentication status
    AuthStatus,
    /// Clear the activity cache
    CacheClear,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dravr_sciotte=info,dravr_sciotte_server=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Serve { host, port }) => run_server(host, port).await,
        Some(Command::Login) => run_login().await,
        Some(Command::Activities {
            limit,
            sport_type,
            format,
            login,
        }) => run_activities(limit, sport_type, format, login).await,
        Some(Command::AuthStatus) => run_auth_status().await,
        Some(Command::CacheClear) => {
            run_cache_clear();
            Ok(())
        }
        None => {
            if cli.transport == "stdio" {
                run_mcp_stdio().await
            } else {
                run_server(cli.host, cli.port).await
            }
        }
    }
}

async fn run_server(
    host: String,
    port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scraper = ChromeScraper::new(ScraperConfig::default());
    let cached = CachedScraper::new(scraper, &CacheConfig::default());
    let state = Arc::new(RwLock::new(ServerState::new(cached)));

    if let Ok(Some(session)) = dravr_sciotte::auth::load_session().await {
        info!("Loaded persisted session");
        state.write().await.set_session(session);
    }

    let tools = build_tool_registry();
    let mcp_server = Arc::new(McpServer::new(state.clone(), tools));
    let app = dravr_sciotte_server::router::build_router(state, mcp_server);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(address = %addr, "Server listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_mcp_stdio() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scraper = ChromeScraper::new(ScraperConfig::default());
    let cached = CachedScraper::new(scraper, &CacheConfig::default());
    let state = Arc::new(RwLock::new(ServerState::new(cached)));

    if let Ok(Some(session)) = dravr_sciotte::auth::load_session().await {
        state.write().await.set_session(session);
    }

    let tools = build_tool_registry();
    let server = Arc::new(McpServer::new(state, tools));
    StdioTransport.serve(server).await
}

async fn run_login() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scraper = ChromeScraper::new(ScraperConfig::default());

    println!("Opening browser for Strava login...");
    println!("Log in to Strava in the browser window that opens.");
    println!("The browser will close automatically once login is detected.\n");

    let session = scraper.browser_login().await?;

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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scraper = ChromeScraper::new(ScraperConfig::default());
    let cached = CachedScraper::new(scraper, &CacheConfig::default());

    let session = if force_login {
        println!("Opening browser for Strava login...");
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
        ..Default::default()
    };

    println!("Scraping activities from Strava...");
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

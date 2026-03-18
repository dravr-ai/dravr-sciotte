// ABOUTME: Standalone MCP server binary for dravr-sciotte
// ABOUTME: Supports stdio and HTTP transports for Claude integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use clap::Parser;
use tokio::sync::RwLock;
use tracing::info;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::{CacheConfig, ScraperConfig};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::transport::http::HttpTransport;
use dravr_sciotte_mcp::transport::stdio::StdioTransport;
use dravr_sciotte_mcp::transport::McpTransport;
use dravr_sciotte_mcp::{build_tool_registry, McpServer, ServerState};

#[derive(Parser)]
#[command(
    name = "dravr-sciotte-mcp",
    version,
    about = "Strava scraper MCP server"
)]
struct Cli {
    /// Transport mode
    #[arg(long, default_value = "stdio")]
    transport: String,

    /// HTTP host (when transport=http)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP port (when transport=http)
    #[arg(long, default_value = "3000")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Only log to stderr so we don't pollute the stdio MCP channel
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dravr_sciotte=info,dravr_sciotte_mcp=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let scraper = ChromeScraper::new(ScraperConfig::default());
    let cached = CachedScraper::new(scraper, &CacheConfig::default());
    let state = Arc::new(RwLock::new(ServerState::new(cached)));
    let tools = build_tool_registry();
    let server = Arc::new(McpServer::new(state, tools));

    info!(transport = %cli.transport, "Starting MCP server");

    match cli.transport.as_str() {
        "stdio" => StdioTransport.serve(server).await,
        "http" => HttpTransport::new(cli.host, cli.port).serve(server).await,
        other => Err(format!("Unknown transport: {other}").into()),
    }
}

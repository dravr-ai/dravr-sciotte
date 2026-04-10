// ABOUTME: Standalone MCP server binary for dravr-sciotte
// ABOUTME: Supports stdio and HTTP transports for Claude integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;
use std::sync::Arc;

use clap::Parser;
use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::CacheConfig;
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::{build_tool_registry, ServerState};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::transport::{http, stdio};
use dravr_tronc::server::cli::McpArgs;
use dravr_tronc::server::tracing_init;
use tokio::sync::RwLock;

/// dravr-sciotte-mcp — MCP server exposing Strava scraping via Model Context Protocol
#[derive(Parser)]
#[command(name = "dravr-sciotte-mcp", version, about)]
struct Cli {
    #[command(flatten)]
    server: McpArgs,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    tracing_init::init(&cli.server.transport);

    let scraper = ChromeScraper::default_config();
    let cached = CachedScraper::new(scraper, &CacheConfig::default());
    let state = Arc::new(RwLock::new(ServerState::new(cached)));
    let tool_registry = build_tool_registry();
    let server = Arc::new(McpServer::new(
        "dravr-sciotte-mcp",
        env!("CARGO_PKG_VERSION"),
        tool_registry,
        state,
    ));

    tracing::info!(
        transport = %cli.server.transport,
        "Starting dravr-sciotte MCP server"
    );

    match cli.server.transport.as_str() {
        "stdio" => stdio::run(server).await?,
        "http" => {
            http::serve(server, &cli.server.host, cli.server.port).await?;
        }
        other => {
            return Err(format!("Unknown transport: {other}. Valid: stdio, http").into());
        }
    }

    Ok(())
}

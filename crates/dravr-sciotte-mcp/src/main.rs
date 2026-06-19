// ABOUTME: Standalone MCP server binary for dravr-sciotte
// ABOUTME: Supports stdio and HTTP transports for Claude integration
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
use std::sync::Arc;

use clap::Parser;
use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::config::CacheConfig;
use dravr_sciotte::queue::{QueueConfig, QueuedScraper, SciotteLimiter};
use dravr_sciotte::scraper::ChromeScraper;
use dravr_sciotte_mcp::{build_tool_registry, ServerState};
use dravr_tronc::mcp::server::McpServer;
use dravr_tronc::mcp::transport::{http, stdio};
use dravr_tronc::server::cli::McpArgs;
use dravr_tronc::server::tracing_init;

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

    let queue_config = QueueConfig::from_env().map_err(|e| -> Box<dyn Error + Send + Sync> {
        Box::new(IoError::other(format!(
            "invalid sciotte queue configuration: {e}"
        )))
    })?;
    tracing::info!(
        max_concurrent = queue_config.max_concurrent,
        max_queue_depth = queue_config.max_queue_depth,
        acquire_timeout_secs = queue_config.acquire_timeout.as_secs(),
        parked_permit_ttl_secs = queue_config.parked_permit_ttl.as_secs(),
        watchdog_interval_secs = queue_config.watchdog_interval.as_secs(),
        retry_after_hint_secs = queue_config.retry_after_hint.as_secs(),
        closed_retry_after_secs = queue_config.closed_retry_after.as_secs(),
        "Sciotte queue configuration loaded from environment"
    );
    let limiter = SciotteLimiter::new(queue_config);
    let watchdog = limiter.spawn_watchdog();

    let chrome = ChromeScraper::default_config()?;
    let queued = QueuedScraper::new(chrome, Arc::clone(&limiter));
    let cached = CachedScraper::new(queued, &CacheConfig::default());
    let state = Arc::new(ServerState::new(cached, limiter));
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

    let run_result = match cli.server.transport.as_str() {
        "stdio" => stdio::run(server).await,
        "http" => http::serve(server, &cli.server.host, cli.server.port).await,
        other => {
            watchdog.abort();
            return Err(format!("Unknown transport: {other}. Valid: stdio, http").into());
        }
    };

    watchdog.abort();
    run_result?;
    Ok(())
}

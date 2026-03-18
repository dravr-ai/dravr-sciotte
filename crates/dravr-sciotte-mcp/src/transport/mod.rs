// ABOUTME: Transport abstraction for MCP server communication channels
// ABOUTME: Defines the McpTransport trait implemented by stdio and HTTP backends
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod http;
pub mod stdio;

use std::sync::Arc;

use async_trait::async_trait;

use crate::server::McpServer;

/// Transport layer for MCP JSON-RPC message exchange
#[async_trait]
pub trait McpTransport: Send {
    /// Start serving MCP requests, blocking until the transport shuts down
    async fn serve(
        self,
        server: Arc<McpServer>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

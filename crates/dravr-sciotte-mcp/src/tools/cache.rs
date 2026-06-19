// ABOUTME: MCP tools for cache management (status and clear)
// ABOUTME: Exposes cache_status and cache_clear tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::{McpTool, ToolContext};
use serde_json::{json, Value};

use crate::state::{ServerState, SharedState};

/// Get cache hit/miss statistics
pub struct CacheStatusTool;

#[async_trait]
impl McpTool<ServerState> for CacheStatusTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "cache_status".to_owned(),
            description: "Get cache hit/miss statistics and entry counts".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: None,
        }
    }

    async fn execute(
        &self,
        shared: &SharedState,
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        let cache_stats = shared.scraper().stats();
        ToolResponse::text(serde_json::to_string_pretty(&cache_stats).unwrap_or_default())
    }
}

/// Clear all cached data
pub struct CacheClearTool;

#[async_trait]
impl McpTool<ServerState> for CacheClearTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "cache_clear".to_owned(),
            description: "Clear all cached activity data".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            annotations: None,
        }
    }

    async fn execute(
        &self,
        shared: &SharedState,
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        shared.scraper().clear();
        ToolResponse::text(json!({"status": "ok", "message": "Cache cleared"}).to_string())
    }
}

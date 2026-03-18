// ABOUTME: MCP tools for cache management (status and clear)
// ABOUTME: Exposes cache_status and cache_clear tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::protocol::{CallToolResult, ToolDefinition};
use crate::state::SharedState;
use crate::tools::McpTool;

/// Get cache hit/miss statistics
pub struct CacheStatusTool;

#[async_trait]
impl McpTool for CacheStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cache_status".to_owned(),
            description: "Get cache hit/miss statistics and entry counts".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, shared: &SharedState, _arguments: Value) -> CallToolResult {
        let guard = shared.read().await;
        let cache_stats = guard.scraper().stats();
        CallToolResult::text(serde_json::to_string_pretty(&cache_stats).unwrap_or_default())
    }
}

/// Clear all cached data
pub struct CacheClearTool;

#[async_trait]
impl McpTool for CacheClearTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "cache_clear".to_owned(),
            description: "Clear all cached activity data".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, shared: &SharedState, _arguments: Value) -> CallToolResult {
        let guard = shared.read().await;
        guard.scraper().clear();
        CallToolResult::text(json!({"status": "ok", "message": "Cache cleared"}).to_string())
    }
}

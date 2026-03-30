// ABOUTME: Tool registry builder mapping MCP tool names to handler implementations
// ABOUTME: Delegates McpTool trait and ToolRegistry to dravr-tronc, registers sciotte-specific tools
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod activities;
pub mod auth;
pub mod cache;
pub mod health;

use dravr_tronc::mcp::tool::ToolRegistry;

use crate::state::ServerState;

/// Build the default tool registry with all available tools
pub fn build_tool_registry() -> ToolRegistry<ServerState> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(auth::AuthStatusTool));
    registry.register(Box::new(auth::BrowserLoginTool));
    registry.register(Box::new(activities::GetActivitiesTool));
    registry.register(Box::new(activities::GetActivityTool));
    registry.register(Box::new(cache::CacheStatusTool));
    registry.register(Box::new(cache::CacheClearTool));
    registry.register(Box::new(health::GetDailySummaryTool));
    registry
}

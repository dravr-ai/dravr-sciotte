// ABOUTME: MCP tool registry infrastructure with trait-based tool dispatch
// ABOUTME: Defines McpTool trait and ToolRegistry for registration and execution
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

pub mod activities;
pub mod auth;
pub mod cache;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::protocol::{CallToolResult, ToolDefinition};
use crate::state::SharedState;

/// Trait for MCP tools that can be registered and dispatched
#[async_trait]
pub trait McpTool: Send + Sync {
    /// Return the tool definition for `tools/list`
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given arguments
    async fn execute(&self, state: &SharedState, arguments: Value) -> CallToolResult;
}

/// Registry of MCP tools keyed by tool name
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn McpTool>>,
}

impl ToolRegistry {
    fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    fn register(&mut self, tool: impl McpTool + 'static) {
        let def = tool.definition();
        self.tools.insert(def.name, Arc::new(tool));
    }

    /// List all tool definitions
    pub fn list_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Execute a tool by name
    pub async fn execute(
        &self,
        name: &str,
        state: &SharedState,
        arguments: Value,
    ) -> CallToolResult {
        match self.tools.get(name) {
            Some(tool) => tool.execute(state, arguments).await,
            None => CallToolResult::error(format!("Unknown tool: {name}")),
        }
    }
}

/// Build the default tool registry with all available tools
pub fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(auth::AuthStatusTool);
    registry.register(auth::BrowserLoginTool);
    registry.register(activities::GetActivitiesTool);
    registry.register(activities::GetActivityTool);
    registry.register(cache::CacheStatusTool);
    registry.register(cache::CacheClearTool);
    registry
}

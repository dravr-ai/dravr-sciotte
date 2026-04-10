// ABOUTME: MCP tools for authentication status and browser-based login
// ABOUTME: Exposes auth_status and browser_login tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_sciotte::auth;
use dravr_sciotte::ActivityScraper;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::McpTool;
use serde_json::{json, Value};

use crate::state::{ServerState, SharedState};

/// Check authentication status
pub struct AuthStatusTool;

#[async_trait]
impl McpTool<ServerState> for AuthStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "auth_status".to_owned(),
            description: "Check if the Strava session is authenticated and valid".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, state: &SharedState, _arguments: Value) -> CallToolResult {
        let guard = state.read().await;

        if let Some(session) = guard.session() {
            let authenticated = guard.scraper().is_authenticated(session).await;
            let result = json!({
                "authenticated": authenticated,
                "session_id": session.session_id,
                "created_at": session.created_at.to_rfc3339(),
                "expires_at": session.expires_at.map(|t| t.to_rfc3339()),
                "cookie_count": session.cookies.len(),
            });
            CallToolResult::text(serde_json::to_string_pretty(&result).unwrap_or_default())
        } else {
            let result = json!({
                "authenticated": false,
                "message": "No session found. Use browser_login to authenticate."
            });
            CallToolResult::text(serde_json::to_string_pretty(&result).unwrap_or_default())
        }
    }
}

/// Launch browser for user to log in to Strava
pub struct BrowserLoginTool;

#[async_trait]
impl McpTool<ServerState> for BrowserLoginTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser_login".to_owned(),
            description: "Open a browser window for the user to log in to Strava. No API credentials needed — the user logs in directly on strava.com and session cookies are captured.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    async fn execute(&self, state: &SharedState, _arguments: Value) -> CallToolResult {
        let session = {
            let guard = state.read().await;
            match guard.scraper().browser_login().await {
                Ok(s) => s,
                Err(e) => return CallToolResult::error(format!("Login failed: {e}")),
            }
        };

        if let Err(e) = auth::save_session(&session).await {
            return CallToolResult::error(format!(
                "Login succeeded but failed to save session: {e}"
            ));
        }

        let session_id = session.session_id.clone();
        let cookie_count = session.cookies.len();
        state.write().await.set_session(session);

        let result = json!({
            "authenticated": true,
            "session_id": session_id,
            "cookie_count": cookie_count,
            "message": "Successfully logged in to Strava"
        });
        CallToolResult::text(serde_json::to_string_pretty(&result).unwrap_or_default())
    }
}

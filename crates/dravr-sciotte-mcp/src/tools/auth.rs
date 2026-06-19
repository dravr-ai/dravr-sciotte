// ABOUTME: MCP tools for authentication status and browser-based login
// ABOUTME: Exposes auth_status and browser_login tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_sciotte::auth;
use dravr_sciotte::ActivityScraper;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::{McpTool, ToolContext};
use serde_json::{json, Value};

use crate::state::{ServerState, SharedState};

/// Check authentication status
pub struct AuthStatusTool;

#[async_trait]
impl McpTool<ServerState> for AuthStatusTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "auth_status".to_owned(),
            description: "Check if the Strava session is authenticated and valid".to_owned(),
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
        state: &SharedState,
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        if let Some(session) = state.session().await {
            let authenticated = state.scraper().is_authenticated(&session).await;
            let result = json!({
                "authenticated": authenticated,
                "session_id": session.session_id,
                "created_at": session.created_at.to_rfc3339(),
                "expires_at": session.expires_at.map(|t| t.to_rfc3339()),
                "cookie_count": session.cookies.len(),
            });
            ToolResponse::text(serde_json::to_string_pretty(&result).unwrap_or_default())
        } else {
            let result = json!({
                "authenticated": false,
                "message": "No session found. Use browser_login to authenticate."
            });
            ToolResponse::text(serde_json::to_string_pretty(&result).unwrap_or_default())
        }
    }
}

/// Launch browser for user to log in to Strava
pub struct BrowserLoginTool;

#[async_trait]
impl McpTool<ServerState> for BrowserLoginTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "browser_login".to_owned(),
            description: "Open a browser window for the user to log in to Strava. No API credentials needed — the user logs in directly on strava.com and session cookies are captured.".to_owned(),
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
        state: &SharedState,
        _ctx: &ToolContext,
        _arguments: Value,
    ) -> ToolResponse {
        let session = match state.scraper().browser_login().await {
            Ok(s) => s,
            Err(e) => return ToolResponse::error(format!("Login failed: {e}")),
        };

        if let Err(e) = auth::save_session(&session).await {
            return ToolResponse::error(format!("Login succeeded but failed to save session: {e}"));
        }

        let session_id = session.session_id.clone();
        let cookie_count = session.cookies.len();
        state.set_session(session).await;

        let result = json!({
            "authenticated": true,
            "session_id": session_id,
            "cookie_count": cookie_count,
            "message": "Successfully logged in to Strava"
        });
        ToolResponse::text(serde_json::to_string_pretty(&result).unwrap_or_default())
    }
}

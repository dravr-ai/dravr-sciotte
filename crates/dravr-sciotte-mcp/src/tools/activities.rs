// ABOUTME: MCP tools for scraping Strava activities
// ABOUTME: Exposes get_activities and get_activity tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::McpTool;
use serde_json::{json, Value};

use crate::state::SharedState;

use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::ActivityScraper;

/// Scrape activities from the Strava training page
pub struct GetActivitiesTool;

#[async_trait]
impl McpTool<crate::state::ServerState> for GetActivitiesTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_activities".to_owned(),
            description: "Scrape activities from the Strava training page. Requires an active authenticated session.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of activities to return (default: 20)"
                    },
                    "sport_type": {
                        "type": "string",
                        "description": "Filter by sport type (e.g., 'Run', 'Ride', 'Swim')"
                    }
                },
                "required": []
            }),
        }
    }

    async fn execute(&self, state: &SharedState, arguments: Value) -> CallToolResult {
        let state = state.read().await;

        let Some(session) = state.session() else {
            return CallToolResult::error(
                "Not authenticated. Use auth_status to check and get_auth_url to start login."
                    .to_owned(),
            );
        };

        let params = ActivityParams {
            limit: arguments["limit"].as_u64().map(|v| v as u32),
            sport_type: arguments["sport_type"].as_str().map(String::from),
            ..Default::default()
        };

        match state.scraper().get_activities(session, &params).await {
            Ok(activities) => {
                let result = json!({
                    "count": activities.len(),
                    "activities": activities,
                });
                CallToolResult::text(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
            Err(e) => CallToolResult::error(format!("Failed to scrape activities: {e}")),
        }
    }
}

/// Get detailed data for a single activity
pub struct GetActivityTool;

#[async_trait]
impl McpTool<crate::state::ServerState> for GetActivityTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_activity".to_owned(),
            description: "Scrape detailed data for a single Strava activity by ID".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "activity_id": {
                        "type": "string",
                        "description": "The Strava activity ID"
                    }
                },
                "required": ["activity_id"]
            }),
        }
    }

    async fn execute(&self, state: &SharedState, arguments: Value) -> CallToolResult {
        let state = state.read().await;

        let Some(session) = state.session() else {
            return CallToolResult::error(
                "Not authenticated. Use auth_status to check and get_auth_url to start login."
                    .to_owned(),
            );
        };

        let Some(activity_id) = arguments["activity_id"].as_str() else {
            return CallToolResult::error("Missing required parameter: activity_id".to_owned());
        };

        match state.scraper().get_activity(session, activity_id).await {
            Ok(activity) => {
                CallToolResult::text(serde_json::to_string_pretty(&activity).unwrap_or_default())
            }
            Err(e) => {
                CallToolResult::error(format!("Failed to scrape activity {activity_id}: {e}"))
            }
        }
    }
}

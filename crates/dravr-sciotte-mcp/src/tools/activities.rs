// ABOUTME: MCP tools for scraping Strava activities
// ABOUTME: Exposes get_activities and get_activity tools via MCP
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_sciotte::models::ActivityParams;
use dravr_sciotte::ActivityScraper;
use dravr_tronc::mcp::schema::{Tool, ToolResponse};
use dravr_tronc::{McpTool, ToolContext};
use serde_json::{json, Value};

use crate::state::{ServerState, SharedState};

/// Scrape activities from the Strava training page
pub struct GetActivitiesTool;

#[async_trait]
impl McpTool<ServerState> for GetActivitiesTool {
    fn definition(&self) -> Tool {
        Tool {
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
            annotations: None,
        }
    }

    async fn execute(
        &self,
        state: &SharedState,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        let Some(entry) = state.session_entry().await else {
            return ToolResponse::error(
                "Not authenticated. Use auth_status to check and browser_login to start login."
                    .to_owned(),
            );
        };
        let Some(scraper) = state.scraper_for(&entry.provider) else {
            return ToolResponse::error(format!(
                "Session provider '{}' is not served by this instance",
                entry.provider
            ));
        };

        let params = ActivityParams {
            limit: arguments["limit"].as_u64().map(|v| v as u32),
            sport_type: arguments["sport_type"].as_str().map(String::from),
            ..Default::default()
        };

        match scraper.get_activities(&entry.session, &params).await {
            Ok(activities) => {
                let result = json!({
                    "count": activities.len(),
                    "activities": activities,
                });
                ToolResponse::text(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
            Err(e) => ToolResponse::error(format!("Failed to scrape activities: {e}")),
        }
    }
}

/// Get detailed data for a single activity
pub struct GetActivityTool;

#[async_trait]
impl McpTool<ServerState> for GetActivityTool {
    fn definition(&self) -> Tool {
        Tool {
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
            annotations: None,
        }
    }

    async fn execute(
        &self,
        state: &SharedState,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> ToolResponse {
        let Some(entry) = state.session_entry().await else {
            return ToolResponse::error(
                "Not authenticated. Use auth_status to check and browser_login to start login."
                    .to_owned(),
            );
        };
        let Some(scraper) = state.scraper_for(&entry.provider) else {
            return ToolResponse::error(format!(
                "Session provider '{}' is not served by this instance",
                entry.provider
            ));
        };

        let Some(activity_id) = arguments["activity_id"].as_str() else {
            return ToolResponse::error("Missing required parameter: activity_id".to_owned());
        };

        match scraper.get_activity(&entry.session, activity_id).await {
            Ok(activity) => {
                ToolResponse::text(serde_json::to_string_pretty(&activity).unwrap_or_default())
            }
            Err(e) => ToolResponse::error(format!("Failed to scrape activity {activity_id}: {e}")),
        }
    }
}

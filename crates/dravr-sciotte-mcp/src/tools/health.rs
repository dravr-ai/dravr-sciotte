// ABOUTME: MCP tool for scraping daily health/wellness summary from provider dashboards
// ABOUTME: Exposes get_daily_summary tool via MCP for sleep, HR, stress, steps, etc.
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use async_trait::async_trait;
use dravr_tronc::mcp::protocol::{CallToolResult, ToolDefinition};
use dravr_tronc::McpTool;
use serde_json::{json, Value};

use crate::state::SharedState;

use dravr_sciotte::models::HealthParams;
use dravr_sciotte::ActivityScraper;

/// Scrape daily health/wellness summary for a given date
pub struct GetDailySummaryTool;

#[async_trait]
impl McpTool<crate::state::ServerState> for GetDailySummaryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "get_daily_summary".to_owned(),
            description: "Scrape daily health/wellness summary (heart rate, body battery, stress, steps, VO2 max, etc.) for a given date. Requires an active authenticated session and a provider with health page support.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "date": {
                        "type": "string",
                        "description": "Calendar date in YYYY-MM-DD format"
                    }
                },
                "required": ["date"]
            }),
        }
    }

    async fn execute(&self, state: &SharedState, arguments: Value) -> CallToolResult {
        let state = state.read().await;

        let Some(session) = state.session() else {
            return CallToolResult::error(
                "Not authenticated. Use auth_status to check and browser_login to start login."
                    .to_owned(),
            );
        };

        let Some(date_str) = arguments["date"].as_str() else {
            return CallToolResult::error(
                "Missing required parameter: date (YYYY-MM-DD)".to_owned(),
            );
        };

        let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
            return CallToolResult::error(format!(
                "Invalid date format '{date_str}', expected YYYY-MM-DD"
            ));
        };

        let params = HealthParams { date };

        match state.scraper().get_daily_summary(session, &params).await {
            Ok(summary) => {
                CallToolResult::text(serde_json::to_string_pretty(&summary).unwrap_or_default())
            }
            Err(e) => CallToolResult::error(format!("Failed to scrape daily summary: {e}")),
        }
    }
}

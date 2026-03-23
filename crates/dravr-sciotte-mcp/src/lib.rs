// ABOUTME: Library root re-exporting MCP server modules for use by dravr-sciotte-server
// ABOUTME: Delegates protocol, server, and transport to dravr-tronc; provides tools and state
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # dravr-sciotte-mcp
//!
//! MCP server library exposing Strava scraping via Model Context Protocol.
//! Supports stdio and HTTP/SSE transports with activity and auth management tools.
//!
//! ## Re-exports
//!
//! - [`McpServer`] — JSON-RPC request dispatcher (from dravr-tronc)
//! - [`ServerState`] / [`SharedState`] — shared server state with scraper and session store
//! - [`build_tool_registry`] — default tool registry with all MCP tools

pub mod state;
pub mod tools;

pub use dravr_tronc::McpServer;
pub use state::{ServerState, SharedState};
pub use tools::build_tool_registry;

// ABOUTME: Library root re-exporting MCP server modules for use by dravr-sciotte-server
// ABOUTME: Exposes protocol, server, state, tools, and transport as public modules
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! # dravr-sciotte-mcp
//!
//! MCP server library exposing Strava scraping via Model Context Protocol.
//! Supports stdio and HTTP/SSE transports with activity and auth management tools.

pub mod protocol;
pub mod server;
pub mod state;
pub mod tools;
pub mod transport;

pub use server::McpServer;
pub use state::{ServerState, SharedState};
pub use tools::build_tool_registry;
pub use transport::McpTransport;

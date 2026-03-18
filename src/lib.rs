// ABOUTME: Strava training data scraper with headless Chrome, OAuth, and in-memory caching
// ABOUTME: Trait-based architecture with REST, MCP, and CLI integration surfaces
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![deny(unsafe_code)]

//! # dravr-sciotte
//!
//! A Strava training data scraper that authenticates via OAuth and uses headless
//! Chrome to scrape activity data from `https://www.strava.com/athlete/training`.
//!
//! ## Integration Modes
//!
//! 1. **Programmatic** — use the [`StravaScraper`] trait directly in your Rust code
//! 2. **REST API** — run `dravr-sciotte-server serve` for HTTP endpoints
//! 3. **MCP** — run `dravr-sciotte-server --transport stdio` for Claude integration
//! 4. **CLI** — run `dravr-sciotte-server login` / `activities` subcommands
//!
//! ## Quick Start (Programmatic)
//!
//! ```rust,no_run
//! use dravr_sciotte::{ChromeScraper, CachedScraper, StravaScraper};
//! use dravr_sciotte::config::{ScraperConfig, CacheConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let scraper = ChromeScraper::default_config();
//! let cached = CachedScraper::new(scraper, &CacheConfig::default());
//!
//! // Opens a browser — user logs in to Strava, cookies are captured
//! let session = cached.browser_login().await?;
//! # Ok(())
//! # }
//! ```

/// Scraper error types
pub mod error;

/// Data models: Activity, `SportType`, `AuthSession`, query params
pub mod models;

/// Configuration: OAuth, scraper, cache settings
pub mod config;

/// Core `StravaScraper` trait
pub mod types;

/// OAuth flow helpers and encrypted session persistence
pub mod auth;

/// Chromiumoxide-based Strava scraper implementation
pub mod scraper;

/// In-memory TTL cache layer
pub mod cache;

// Re-export primary types for consumers
pub use cache::CachedScraper;
pub use scraper::ChromeScraper;
pub use types::StravaScraper;

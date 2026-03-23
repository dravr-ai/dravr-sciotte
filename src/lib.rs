// ABOUTME: Sport activity scraper with headless Chrome, browser login, and in-memory caching
// ABOUTME: TOML-configurable providers (Strava, etc.) with REST, MCP, and CLI integration
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![deny(unsafe_code)]

//! # dravr-sciotte
//!
//! A sport activity scraper that uses headless Chrome to scrape training data
//! from sport platforms. Provider configurations (URLs, selectors, JS extraction)
//! are defined in TOML files, making it easy to add new providers.
//!
//! ## Integration Modes
//!
//! 1. **Programmatic** — use the [`ActivityScraper`] trait directly in your Rust code
//! 2. **REST API** — run `dravr-sciotte-server serve` for HTTP endpoints
//! 3. **MCP** — run `dravr-sciotte-server --transport stdio` for Claude integration
//! 4. **CLI** — run `dravr-sciotte-server login` / `activities` subcommands
//!
//! ## Quick Start (Programmatic)
//!
//! ```rust,no_run
//! use dravr_sciotte::{ChromeScraper, CachedScraper, ActivityScraper};
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

/// Core `ActivityScraper` trait
pub mod types;

/// OAuth flow helpers and encrypted session persistence
pub mod auth;

/// Shared browser automation utilities (launch, cookies, CDP input)
pub mod browser_utils;

/// Embedded fake login server for testing
pub mod fake_login;

/// JavaScript string escaping utilities for CDP evaluate calls
pub mod js_utils;

/// TOML-based provider configuration (selectors, URLs, JS extraction rules)
pub mod provider;

/// Chromiumoxide-based scraper implementation driven by provider config
pub mod scraper;

/// In-memory TTL cache layer
pub mod cache;

/// Vision-based scraper using LLM screenshot analysis (requires `vision` feature)
#[cfg(feature = "vision")]
pub mod vision;

// Re-export primary types for consumers
pub use cache::CachedScraper;
pub use scraper::ChromeScraper;
pub use types::ActivityScraper;

#[cfg(feature = "vision")]
pub use vision::VisionScraper;

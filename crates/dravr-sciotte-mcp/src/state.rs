// ABOUTME: Shared server state holding the cached scraper and optional auth session
// ABOUTME: Thread-safe via Arc<RwLock> for concurrent access from transport handlers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::sync::Arc;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::scraper::ChromeScraper;
use tokio::sync::RwLock;

/// Type alias for the shared state handle used across the server
pub type SharedState = Arc<RwLock<ServerState>>;

/// Central server state holding the scraper and authentication session
pub struct ServerState {
    scraper: CachedScraper<ChromeScraper>,
    session: Option<AuthSession>,
}

impl ServerState {
    /// Create server state with the given cached scraper
    pub fn new(scraper: CachedScraper<ChromeScraper>) -> Self {
        Self {
            scraper,
            session: None,
        }
    }

    /// Get a reference to the cached scraper
    pub const fn scraper(&self) -> &CachedScraper<ChromeScraper> {
        &self.scraper
    }

    /// Get the current authentication session
    pub fn session(&self) -> Option<&AuthSession> {
        self.session.as_ref()
    }

    /// Set or replace the authentication session
    pub fn set_session(&mut self, session: AuthSession) {
        self.session = Some(session);
    }

    /// Clear the authentication session
    pub fn clear_session(&mut self) {
        self.session = None;
    }
}

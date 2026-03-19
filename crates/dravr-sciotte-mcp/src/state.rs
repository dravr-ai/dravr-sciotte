// ABOUTME: Shared server state holding the cached scraper and multi-session store
// ABOUTME: Thread-safe via Arc<RwLock> for concurrent access from transport handlers
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::collections::HashMap;
use std::sync::Arc;

use dravr_sciotte::cache::CachedScraper;
use dravr_sciotte::models::AuthSession;
use dravr_sciotte::scraper::ChromeScraper;
use tokio::sync::RwLock;

/// Type alias for the shared state handle used across the server
pub type SharedState = Arc<RwLock<ServerState>>;

/// Central server state holding the scraper and multi-session authentication store.
///
/// Sessions are keyed by `session_id`. The `latest_session_id` tracks the most
/// recently created session for backward compatibility with single-session callers.
pub struct ServerState {
    scraper: CachedScraper<ChromeScraper>,
    sessions: HashMap<String, AuthSession>,
    latest_session_id: Option<String>,
}

impl ServerState {
    /// Create server state with the given cached scraper and no sessions
    pub fn new(scraper: CachedScraper<ChromeScraper>) -> Self {
        Self {
            scraper,
            sessions: HashMap::new(),
            latest_session_id: None,
        }
    }

    /// Get a reference to the cached scraper
    pub const fn scraper(&self) -> &CachedScraper<ChromeScraper> {
        &self.scraper
    }

    /// Look up a session by ID
    pub fn get_session(&self, session_id: &str) -> Option<&AuthSession> {
        self.sessions.get(session_id)
    }

    /// Add a new session (or replace an existing one with the same ID)
    pub fn add_session(&mut self, session: AuthSession) {
        let id = session.session_id.clone();
        self.sessions.insert(id.clone(), session);
        self.latest_session_id = Some(id);
    }

    /// Remove a session by ID, returning it if it existed
    pub fn remove_session(&mut self, session_id: &str) -> Option<AuthSession> {
        if self.latest_session_id.as_deref() == Some(session_id) {
            self.latest_session_id = None;
        }
        self.sessions.remove(session_id)
    }

    /// List all active session IDs
    pub fn list_session_ids(&self) -> Vec<&str> {
        self.sessions.keys().map(String::as_str).collect()
    }

    /// Get the number of active sessions
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Backward compatibility: return the latest session (or the only session)
    pub fn session(&self) -> Option<&AuthSession> {
        self.latest_session_id
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .or_else(|| self.sessions.values().next())
    }

    /// Backward compatibility: set session (adds to the store)
    pub fn set_session(&mut self, session: AuthSession) {
        self.add_session(session);
    }

    /// Clear all sessions
    pub fn clear_sessions(&mut self) {
        self.sessions.clear();
        self.latest_session_id = None;
    }
}

// ABOUTME: In-memory TTL cache wrapping ActivityScraper for activity data
// ABOUTME: Uses moka concurrent cache with configurable TTL and max entries
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use moka::future::Cache;
use tracing::{debug, info};

use crate::config::CacheConfig;
use crate::error::{LoginResult, ScraperResult};
use crate::models::{Activity, ActivityParams, AuthSession};
use crate::types::ActivityScraper;

/// Cache key combining session identity and query parameters
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    session_id: String,
    params_hash: u64,
}

impl CacheKey {
    fn new(session: &AuthSession, params: &ActivityParams) -> Self {
        let mut hasher = std::hash::DefaultHasher::new();
        params.limit.hash(&mut hasher);
        if let Some(before) = params.before {
            before.timestamp().hash(&mut hasher);
        }
        if let Some(after) = params.after {
            after.timestamp().hash(&mut hasher);
        }
        params.sport_type.hash(&mut hasher);

        Self {
            session_id: session.session_id.clone(),
            params_hash: hasher.finish(),
        }
    }
}

/// Cached wrapper around any `ActivityScraper` implementation
pub struct CachedScraper<S> {
    inner: S,
    activity_cache: Cache<CacheKey, Vec<Activity>>,
    detail_cache: Cache<String, Activity>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl<S: ActivityScraper> CachedScraper<S> {
    /// Wrap a scraper with an in-memory TTL cache
    pub fn new(inner: S, config: &CacheConfig) -> Self {
        let ttl = Duration::from_secs(config.ttl_secs);

        Self {
            inner,
            activity_cache: Cache::builder()
                .max_capacity(config.max_entries)
                .time_to_live(ttl)
                .build(),
            detail_cache: Cache::builder()
                .max_capacity(config.max_entries)
                .time_to_live(ttl)
                .build(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Get cache hit/miss statistics
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            activity_entries: self.activity_cache.entry_count(),
            detail_entries: self.detail_cache.entry_count(),
        }
    }

    /// Clear all cached data
    pub fn clear(&self) {
        self.activity_cache.invalidate_all();
        self.detail_cache.invalidate_all();
        info!("Cache cleared");
    }

    /// Get a reference to the inner scraper
    pub const fn inner(&self) -> &S {
        &self.inner
    }
}

#[async_trait]
impl<S: ActivityScraper> ActivityScraper for CachedScraper<S> {
    async fn browser_login(&self) -> ScraperResult<AuthSession> {
        self.inner.browser_login().await
    }

    async fn credential_login(
        &self,
        email: &str,
        password: &str,
        method: &str,
    ) -> ScraperResult<LoginResult> {
        self.inner.credential_login(email, password, method).await
    }

    async fn submit_otp(&self, code: &str) -> ScraperResult<LoginResult> {
        self.inner.submit_otp(code).await
    }

    async fn select_two_factor(&self, option_id: &str) -> ScraperResult<LoginResult> {
        self.inner.select_two_factor(option_id).await
    }

    async fn is_authenticated(&self, session: &AuthSession) -> bool {
        self.inner.is_authenticated(session).await
    }

    async fn get_activities(
        &self,
        session: &AuthSession,
        params: &ActivityParams,
    ) -> ScraperResult<Vec<Activity>> {
        let key = CacheKey::new(session, params);

        if let Some(cached) = self.activity_cache.get(&key).await {
            self.hits.fetch_add(1, Ordering::Relaxed);
            debug!(key = ?key.params_hash, "Cache hit for activities");
            return Ok(cached);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        debug!(key = ?key.params_hash, "Cache miss for activities");

        let activities = self.inner.get_activities(session, params).await?;
        self.activity_cache.insert(key, activities.clone()).await;

        Ok(activities)
    }

    async fn get_activity(
        &self,
        session: &AuthSession,
        activity_id: &str,
    ) -> ScraperResult<Activity> {
        let key = format!("{}:{activity_id}", session.session_id);

        if let Some(cached) = self.detail_cache.get(&key).await {
            self.hits.fetch_add(1, Ordering::Relaxed);
            debug!(activity_id, "Cache hit for activity detail");
            return Ok(cached);
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        debug!(activity_id, "Cache miss for activity detail");

        let activity = self.inner.get_activity(session, activity_id).await?;
        self.detail_cache.insert(key, activity.clone()).await;

        Ok(activity)
    }
}

/// Cache hit/miss statistics
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Number of cached activity list entries
    pub activity_entries: u64,
    /// Number of cached activity detail entries
    pub detail_entries: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_deterministic() {
        let session = AuthSession {
            session_id: "test".to_owned(),
            cookies: vec![],
            created_at: chrono::Utc::now(),
            expires_at: None,
        };
        let params = ActivityParams {
            limit: Some(10),
            ..Default::default()
        };

        let k1 = CacheKey::new(&session, &params);
        let k2 = CacheKey::new(&session, &params);
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_varies_with_params() {
        let session = AuthSession {
            session_id: "test".to_owned(),
            cookies: vec![],
            created_at: chrono::Utc::now(),
            expires_at: None,
        };

        let k1 = CacheKey::new(
            &session,
            &ActivityParams {
                limit: Some(10),
                ..Default::default()
            },
        );
        let k2 = CacheKey::new(
            &session,
            &ActivityParams {
                limit: Some(20),
                ..Default::default()
            },
        );
        assert_ne!(k1, k2);
    }
}

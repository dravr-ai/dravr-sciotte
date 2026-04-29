// ABOUTME: TTL-bounded slot for in-flight 2FA/OTP browser sessions
// ABOUTME: Evicts abandoned credential logins so held Chrome processes are freed
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::time::Duration;

use tokio::time::Instant;

/// In-progress credential login held between `credential_login` and the
/// follow-up `submit_otp` / `select_two_factor` call.
///
/// Each entry records the `Instant` it was parked. Entries older than the
/// configured TTL are evicted on access (and on every store) — dropping the
/// inner value (typically a `(Browser, Page)` pair) so chromiumoxide's
/// `kill_on_drop` reaps the held Chrome process. Without this bound,
/// abandoned 2FA flows would pin Chrome instances for the lifetime of the
/// scraper.
pub(crate) struct PendingLogin<T> {
    inner: T,
    created_at: Instant,
}

impl<T> PendingLogin<T> {
    /// Park `inner` with the current timestamp.
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            created_at: Instant::now(),
        }
    }

    /// Consume the slot, returning the inner value if it was parked less
    /// than `ttl` ago. Otherwise drops the inner value and returns `None`.
    pub(crate) fn into_inner_if_fresh(self, ttl: Duration) -> Option<T> {
        if self.created_at.elapsed() >= ttl {
            None
        } else {
            Some(self.inner)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::PendingLogin;

    #[test]
    fn fresh_slot_returns_inner() {
        let slot = PendingLogin::new(42i32);
        assert_eq!(slot.into_inner_if_fresh(Duration::from_secs(5)), Some(42));
    }

    #[test]
    fn expired_slot_returns_none_and_drops_inner() {
        let slot = PendingLogin::new("payload".to_owned());
        sleep(Duration::from_millis(20));
        assert_eq!(slot.into_inner_if_fresh(Duration::from_millis(5)), None);
    }

    #[test]
    fn zero_ttl_always_evicts() {
        let slot = PendingLogin::new(());
        assert_eq!(slot.into_inner_if_fresh(Duration::ZERO), None);
    }
}

// ABOUTME: Process-wide signal that a chromiumoxide browser is being torn down
// ABOUTME: Lets the platform's tracing layer suppress expected post-close WS errors
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! Browser-teardown signal published to the rest of the process.
//!
//! ## Why this exists
//!
//! `chromiumoxide` drives Chrome over a DevTools-Protocol WebSocket. When
//! we close the browser, Chrome exits without sending the polite WS Close
//! frame, so chromiumoxide's handler task observes a `Ws(Protocol(
//! ResetWithoutClosingHandshake))` and emits an `error!` log. Nothing is
//! actually broken — the scrape completed before the close — but
//! `dravr-tronc` forwards every `error!` line to the Slack alert channel,
//! drowning real errors in lifecycle noise.
//!
//! Globally downgrading `chromiumoxide::handler` errors would also hide
//! genuine WS faults (Chrome crash mid-scrape, network reset on a live
//! session) and is rejected for that reason. Instead, sciotte advertises
//! a short *"teardown is in progress"* window via this module; the
//! platform's tracing init installs a `Filter` layer that suppresses
//! `chromiumoxide::handler` ERROR events **only** while the window is
//! open. Outside that window, every chromiumoxide error still reaches
//! the Slack alert path with full fidelity.
//!
//! ## Lifetime
//!
//! - Each [`TeardownGuard::new`] increments [`TEARDOWN_DEPTH`]
//! - Dropping the guard schedules a tokio task that, after
//!   [`TEARDOWN_GRACE`], decrements the counter
//! - The grace window covers the gap between sciotte's `browser.close().await`
//!   returning and the chromiumoxide handler task observing the WS reset
//!   (empirically a few hundred milliseconds; the constant is generous
//!   to absorb GC / scheduling jitter without bleeding into normal ops)
//!
//! Concurrent teardowns are supported — the counter is `AtomicU32` so two
//! browsers torn down in parallel both contribute to "depth > 0" until
//! both grace windows expire.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// Grace window held open after a guard is dropped so the chromiumoxide
/// handler task can emit its post-close WS-reset error inside the
/// suppression window. 500 ms is long enough to absorb scheduling jitter
/// on Cloud Run cold starts; short enough that real errors landing
/// immediately after a teardown still appear noisy in logs (and thus
/// Slack) within ~half a second of "back to normal".
const TEARDOWN_GRACE: Duration = Duration::from_millis(500);

/// Process-wide counter of in-flight browser teardowns. Public **for
/// reading only** so the platform's tracing filter can ask
/// [`is_in_progress`] without taking a dependency on this module's
/// internals. Treat as opaque from outside.
static TEARDOWN_DEPTH: AtomicU32 = AtomicU32::new(0);

/// Returns `true` while one or more chromiumoxide browsers are inside
/// their teardown grace window.
///
/// The platform's tracing layer calls this on every event whose target
/// is `chromiumoxide::handler`. When `true`, ERROR-level events from
/// that target are skipped; outside the window every event flows
/// through the normal subscriber chain.
#[must_use]
pub fn is_in_progress() -> bool {
    TEARDOWN_DEPTH.load(Ordering::Relaxed) > 0
}

/// RAII guard that opens a teardown grace window for the duration of a
/// browser-close call.
///
/// Construct **before** calling `browser.close().await`; let it drop at
/// the end of the close path. The grace window stays open for
/// [`TEARDOWN_GRACE`] **after** the guard is dropped so the
/// chromiumoxide handler task — which emits its WS-reset error
/// asynchronously, after our `close()` future has already resolved —
/// still falls inside the suppression window.
///
/// Must be created from inside a tokio runtime; the grace-window
/// release is implemented with [`tokio::spawn`].
pub struct TeardownGuard {
    _private: (),
}

impl TeardownGuard {
    /// Open a new teardown window. Returns immediately.
    #[must_use]
    pub fn new() -> Self {
        TEARDOWN_DEPTH.fetch_add(1, Ordering::Relaxed);
        Self { _private: () }
    }
}

impl Default for TeardownGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TeardownGuard {
    fn drop(&mut self) {
        // Defer the decrement so chromiumoxide's handler task — which
        // sees the WS reset *after* our close().await resolves — still
        // sees TEARDOWN_DEPTH > 0 and gets its error suppressed.
        tokio::spawn(async {
            tokio::time::sleep(TEARDOWN_GRACE).await;
            TEARDOWN_DEPTH.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

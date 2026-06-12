// ABOUTME: Library root re-exporting server modules
// ABOUTME: Exposes router, handlers, auth, streaming, and state for composability
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::str_to_string
    )
)]

pub mod auth;
pub mod error_response;
pub mod health;
pub mod router;
pub mod state;
pub mod streaming;

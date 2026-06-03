// ABOUTME: Minimal vision-LLM abstraction the vision scraper depends on
// ABOUTME: Keeps sciotte free of any concrete LLM crate; the consumer implements this trait
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::error::Error;

use async_trait::async_trait;

/// Error returned by a [`VisionModel`] implementation.
///
/// Boxed so sciotte stays agnostic to the consumer's error type (an embacle
/// provider error, an HTTP error, etc.).
pub type VisionModelError = Box<dyn Error + Send + Sync>;

/// A vision-capable LLM reduced to the single operation the vision scraper needs:
/// analyze a page screenshot against a prompt and return the model's text reply.
///
/// sciotte defines this trait so it does **not** depend on any concrete LLM
/// crate (e.g. embacle). The consumer implements it — typically by wrapping its
/// own LLM provider — and hands it to
/// [`crate::scraper::ChromeScraper::with_llm`].
#[async_trait]
pub trait VisionModel: Send + Sync {
    /// Analyze a base64-encoded PNG screenshot against `prompt`; return the
    /// model's text response.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying model call fails.
    async fn analyze_screenshot(
        &self,
        prompt: &str,
        screenshot_png_b64: &str,
    ) -> Result<String, VisionModelError>;
}

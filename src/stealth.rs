// ABOUTME: CDP-injected JS to hide automation tells from bot-detection scripts
// ABOUTME: Applied via Page.addScriptToEvaluateOnNewDocument so it runs before any page JS
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::Page;

use crate::error::{ScraperError, ScraperResult};

/// Stealth payload covering Google Sign-In's automation detection vectors
/// plus the API response capture hook used by provider `js_extract`.
///
/// Google's "Couldn't sign you in. This browser or app may not be secure"
/// page checks several automation tells beyond `navigator.webdriver`:
/// `navigator.plugins`, `navigator.languages`, `window.chrome`, the
/// permissions API shape, and the WebGL renderer string. Cloudflare's
/// silent JS challenge weights `webdriver` heavily but tolerates the
/// rest. We override every signal Google's loader probes so headless
/// Chrome passes both gates.
///
/// The API capture hook intercepts every `fetch()` and `XMLHttpRequest` the
/// page makes and stores responses by URL in `window.__sciotteApiCaptures`.
/// Provider `js_extract` snippets read this map instead of replaying API
/// calls — replay loses Garmin's request fingerprint (Origin, Referer,
/// per-request anti-replay tokens) and gets 403'd. Reading from the page's
/// own successful fetches has no such problem.
const STEALTH_SCRIPT: &str = r"
// JS-level navigator/WebGL/plugin spoofs intentionally disabled for the
// baseline Cloudflare-fingerprint test. Rationale:
//   - `navigator.webdriver` is already removed by Chrome's
//     `--disable-blink-features=AutomationControlled` flag (emitted by
//     chromiumoxide's `.hide()`), so the JS override is redundant.
//   - The WebGL renderer spoof was hardcoded to 'Apple Inc.' / 'Apple M1'
//     regardless of platform. On Linux Chromium that's an obvious
//     platform contradiction with the Linux User-Agent; Cloudflare's
//     fingerprint analysis 427s requests on this mismatch before any
//     login page renders.
//   - Plugin / languages / chrome.runtime spoofs all also override
//     properties via `Object.defineProperty`, which leaves a detectable
//     trace in the function's `.toString()` — modern detection scripts
//     read this and flag automation.
// The XHR/fetch capture hook below is kept — it's a Garmin /app/
// data-extraction utility, not an anti-detection layer.

(function() {
    if (window.__sciotteApiCaptures) return;
    window.__sciotteApiCaptures = {};

    var origFetch = window.fetch;
    window.fetch = function(input, init) {
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        var p = origFetch.apply(this, arguments);
        if (/activity-service|workout-service|gc-api|training_activities/.test(url)) {
            p.then(function(r) {
                try {
                    var clone = r.clone();
                    clone.text().then(function(t) {
                        window.__sciotteApiCaptures[url] = {
                            status: r.status,
                            body: t
                        };
                    }).catch(function() {});
                } catch (e) {}
                return r;
            }).catch(function() {});
        }
        return p;
    };

    var origOpen = XMLHttpRequest.prototype.open;
    var origSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function(method, url) {
        this.__sciotteUrl = url;
        return origOpen.apply(this, arguments);
    };
    XMLHttpRequest.prototype.send = function() {
        var self = this;
        var url = this.__sciotteUrl || '';
        if (/activity-service|workout-service|gc-api|training_activities/.test(url)) {
            this.addEventListener('load', function() {
                try {
                    window.__sciotteApiCaptures[url] = {
                        status: self.status,
                        body: self.responseText
                    };
                } catch (e) {}
            });
        }
        return origSend.apply(this, arguments);
    };
})();
";

/// Inject the stealth payload into a page.
///
/// Must be called after `new_page` and before any navigation that should
/// appear non-automated. Runs on every frame creation thereafter, including
/// subsequent `page.goto(...)` calls.
pub async fn apply_minimal_stealth(page: &Page) -> ScraperResult<()> {
    page.execute(AddScriptToEvaluateOnNewDocumentParams::new(
        STEALTH_SCRIPT.to_owned(),
    ))
    .await
    .map_err(|e| ScraperError::Browser {
        reason: format!("Failed to inject stealth script: {e}"),
    })?;
    Ok(())
}

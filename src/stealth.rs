// ABOUTME: CDP-injected JS to hide automation tells from bot-detection scripts
// ABOUTME: Applied via Page.addScriptToEvaluateOnNewDocument so it runs before any page JS
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use chromiumoxide::cdp::browser_protocol::page::AddScriptToEvaluateOnNewDocumentParams;
use chromiumoxide::Page;

use crate::error::{ScraperError, ScraperResult};

/// Minimal stealth payload: hides `navigator.webdriver` and installs an API
/// response capture hook.
///
/// The `webdriver` override defeats Cloudflare's silent JS challenge (the
/// `cdn-cgi/challenge-platform/h/g/jsd/...` endpoint reads it as one of its
/// highest-weighted bot signals). Chrome's `--disable-blink-features=AutomationControlled`
/// hides it on older Chrome but leaks under newer versions; this script
/// overrides the property directly so it survives Chrome upgrades.
///
/// The API capture hook intercepts every `fetch()` and `XMLHttpRequest` the
/// page makes and stores responses by URL in `window.__sciotteApiCaptures`.
/// Provider `js_extract` snippets read this map instead of replaying API
/// calls — replay loses Garmin's request fingerprint (Origin, Referer,
/// per-request anti-replay tokens) and gets 403'd. Reading from the page's
/// own successful fetches has no such problem.
const STEALTH_SCRIPT: &str = r"
Object.defineProperty(navigator, 'webdriver', {
    get: () => undefined,
    configurable: true
});

(function() {
    if (window.__sciotteApiCaptures) return;
    window.__sciotteApiCaptures = {};

    var origFetch = window.fetch;
    window.fetch = function(input, init) {
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        var p = origFetch.apply(this, arguments);
        if (/activity-service|workout-service|gc-api/.test(url)) {
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
        if (/activity-service|workout-service|gc-api/.test(url)) {
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

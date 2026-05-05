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
Object.defineProperty(navigator, 'webdriver', {
    get: () => undefined,
    configurable: true
});

(function() {
    // navigator.plugins — headless Chrome reports an empty PluginArray;
    // real Chrome has at least 3 (PDF Viewer, Native Client, Chromium PDF).
    // Google's loader rejects sign-ins from browsers reporting 0 plugins.
    var fakePlugins = [
        { name: 'PDF Viewer', filename: 'internal-pdf-viewer', description: 'Portable Document Format' },
        { name: 'Chrome PDF Viewer', filename: 'internal-pdf-viewer', description: '' },
        { name: 'Chromium PDF Viewer', filename: 'internal-pdf-viewer', description: '' }
    ];
    Object.defineProperty(navigator, 'plugins', {
        get: function() {
            var arr = fakePlugins.slice();
            arr.item = function(i) { return arr[i] || null; };
            arr.namedItem = function(n) { return arr.find(function(p) { return p.name === n; }) || null; };
            arr.refresh = function() {};
            return arr;
        },
        configurable: true
    });

    // navigator.languages — headless reports [] on some launches;
    // production users almost always have at least one language.
    if (!navigator.languages || navigator.languages.length === 0) {
        Object.defineProperty(navigator, 'languages', {
            get: function() { return ['en-US', 'en']; },
            configurable: true
        });
    }

    // window.chrome — present in real Chrome with a runtime object;
    // missing in headless Chrome by default.
    if (!window.chrome) {
        window.chrome = {
            runtime: {},
            loadTimes: function() {},
            csi: function() {},
            app: {}
        };
    } else if (!window.chrome.runtime) {
        window.chrome.runtime = {};
    }

    // Permissions API — headless returns 'granted' for notifications when
    // notifications are 'denied' at the document level, which real browsers
    // never do. Googles checks for this exact mismatch.
    if (window.navigator && navigator.permissions && navigator.permissions.query) {
        var origQuery = navigator.permissions.query.bind(navigator.permissions);
        navigator.permissions.query = function(parameters) {
            if (parameters && parameters.name === 'notifications') {
                return Promise.resolve({ state: Notification.permission, onchange: null });
            }
            return origQuery(parameters);
        };
    }

    // WebGL renderer string — headless leaks 'SwiftShader' / 'Mesa OffScreen';
    // real Chrome on macOS reports 'Apple Inc.' / 'Apple M…'. Spoof to a
    // generic Apple GPU string so fingerprints don't scream 'headless'.
    try {
        var origGetParam = WebGLRenderingContext.prototype.getParameter;
        WebGLRenderingContext.prototype.getParameter = function(parameter) {
            // UNMASKED_VENDOR_WEBGL=37445, UNMASKED_RENDERER_WEBGL=37446
            if (parameter === 37445) return 'Apple Inc.';
            if (parameter === 37446) return 'Apple M1';
            return origGetParam.call(this, parameter);
        };
    } catch (e) {}

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

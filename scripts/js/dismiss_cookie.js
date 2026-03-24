(function() {
    // Cookiebot
    var btn = document.querySelector('#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll')
        || document.querySelector('[data-cookiefirst-action="accept"]')
        || document.querySelector('button[id*="accept"], button[class*="accept"]');
    if (btn) { btn.click(); return 'dismissed'; }
    // Text-based fallback: find any button with Accept All / Tout accepter
    var allButtons = document.querySelectorAll('button, a, [role=button]');
    for (var i = 0; i < allButtons.length; i++) {
        var text = allButtons[i].textContent.trim();
        if (text === 'Accept All' || text === 'Tout accepter' || text === 'Accept all'
            || text === 'Accepter tout' || text === 'Accept All Cookies') {
            allButtons[i].click();
            return 'dismissed_text';
        }
    }
    // Iframe fallback
    var iframes = document.querySelectorAll('iframe');
    for (var j = 0; j < iframes.length; j++) {
        try {
            var doc = iframes[j].contentDocument;
            if (doc) {
                var b = doc.querySelector('#CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll')
                    || doc.querySelector('button[id*="accept"]');
                if (b) { b.click(); return 'dismissed_iframe'; }
            }
        } catch(e) {}
    }
    return 'not_found';
})()

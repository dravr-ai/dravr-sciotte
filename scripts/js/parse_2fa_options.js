(function() {
    var options = [];
    var seen = {};
    var els = document.querySelectorAll('[data-challengetype]');
    for (var i = 0; i < els.length; i++) {
        var el = els[i];
        var ct = el.getAttribute('data-challengetype');
        var rect = el.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) continue;
        var text = el.textContent.trim();
        if (!text || text.length < 5) continue;
        var lower = text.toLowerCase();
        if (lower.includes('passkey') || lower.includes('security key')) continue;
        if (lower.includes('enter your password') || lower.includes('mot de passe')) continue;
        if (lower === 'help' || lower === 'aide') continue;
        if (lower.includes('another way') || lower.includes('autrement')) continue;
        var id = ct || 'unknown';
        if (lower.includes('authenticator') || lower.includes('verification code') || lower.includes('code de validation')) id = 'otp';
        else if (lower.includes('tap') || lower.includes('yes on your') || lower.includes('appuyez')) id = 'app';
        else if (lower.includes('text message') || lower.includes('sms') || lower.includes('texto')) id = 'sms';
        else if (lower.includes('backup') || lower.includes('secours')) id = 'backup';
        if (seen[id]) continue;
        seen[id] = true;
        options.push({id: id, label: text.substring(0, 120), x: rect.x + rect.width / 2, y: rect.y + rect.height / 2});
    }
    if (options.length === 0) {
        var debug = Array.from(els).map(function(e) {
            return {ct: e.getAttribute('data-challengetype'), text: e.textContent.trim().substring(0, 80)};
        });
        return 'debug:' + JSON.stringify(debug);
    }
    return JSON.stringify(options);
})()

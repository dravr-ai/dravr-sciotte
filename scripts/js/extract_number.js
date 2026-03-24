(function() {
    // Find all 2-3 digit numbers on the page with their font sizes
    var candidates = [];
    var all = document.querySelectorAll('*');
    for (var i = 0; i < all.length; i++) {
        var el = all[i];
        var text = el.textContent.trim();
        if (!/^\d{2,3}$/.test(text)) continue;
        var style = window.getComputedStyle(el);
        var fontSize = parseFloat(style.fontSize) || 0;
        var rect = el.getBoundingClientRect();
        if (rect.width <= 0 || rect.height <= 0) continue;
        candidates.push({text: text, size: fontSize, tag: el.tagName, w: rect.width, h: rect.height});
    }
    if (candidates.length === 0) return JSON.stringify({number: null, debug: "no candidates"});
    // Sort by font size descending, pick the largest
    candidates.sort(function(a, b) { return b.size - a.size; });
    return JSON.stringify({number: candidates[0].text, debug: JSON.stringify(candidates.slice(0, 5))});
})()

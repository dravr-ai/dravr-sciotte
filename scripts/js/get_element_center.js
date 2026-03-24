(function(selectorStr) {
    var selectors = selectorStr.split(",").map(function(s) { return s.trim(); });
    var el = null;
    for (var i = 0; i < selectors.length; i++) {
        el = document.querySelector(selectors[i]);
        if (el) break;
    }
    if (!el) return null;
    var r = el.getBoundingClientRect();
    return JSON.stringify({x: r.x + r.width / 2, y: r.y + r.height / 2});
})

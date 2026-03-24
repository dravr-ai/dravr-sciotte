(function(selectorStr) {
    var selectors = selectorStr.split(",").map(function(s) { return s.trim(); });
    for (var i = 0; i < selectors.length; i++) {
        var el = document.querySelector(selectors[i]);
        if (el) {
            var r = el.getBoundingClientRect();
            if (r.width > 0 && r.height > 0) return "found";
        }
    }
    return "not_found";
})

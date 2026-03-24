(function(selectorStr) {
    var parts = selectorStr.split(",").map(function(s) { return s.trim(); });
    for (var i = 0; i < parts.length; i++) {
        var sel = parts[i];
        if (sel.indexOf("text:") === 0) {
            var text = sel.substring(5);
            var buttons = document.querySelectorAll("button, a, [role=button]");
            for (var j = 0; j < buttons.length; j++) {
                if (buttons[j].textContent.trim().indexOf(text) !== -1) {
                    buttons[j].click();
                    return "clicked";
                }
            }
        } else {
            var el = document.querySelector(sel);
            if (el) { el.click(); return "clicked"; }
        }
    }
    return "not_found";
})

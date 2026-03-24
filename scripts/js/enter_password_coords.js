(function() {
    var all = document.querySelectorAll('[data-challengetype], [jsname] li, div[role=link], li, div, span');
    for (var i = 0; i < all.length; i++) {
        var el = all[i];
        var rect = el.getBoundingClientRect();
        if (rect.width > 0 && rect.height > 0 && rect.height < 100) {
            var text = el.textContent.trim();
            if (text === 'Enter your password' || text === 'Saisir votre mot de passe') {
                return JSON.stringify({x: rect.x + rect.width / 2, y: rect.y + rect.height / 2});
            }
        }
    }
    var debug = Array.from(document.querySelectorAll('[data-challengetype], li, div[role=link]')).map(function(e) {
        return {tag: e.tagName, text: e.textContent.trim().substring(0, 50), ct: e.getAttribute('data-challengetype')};
    });
    return 'not_found:' + JSON.stringify(debug);
})()

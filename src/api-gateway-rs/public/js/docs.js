  // Shared by /docs and /tutorials.
  (function() {
    var TUTORIALS = 'https://agentraas.io/tutorials';
    var onTutorials = !!document.getElementById('tut-start');

    // Tutorials moved off /docs (2026-10-26): old /docs#tut-... links land on them.
    var hash = location.hash;
    if (!onTutorials && /^#tut(orials$|-)/.test(hash) && !document.getElementById(hash.slice(1))) {
      location.replace(TUTORIALS + hash);
      return;
    }

    // Highlights the sidebar link for whichever section is currently in view.
    var sections = document.querySelectorAll('.docs-content > section[id]');
    var links = document.querySelectorAll('.docs-sidebar a');
    function setActive(id) {
      links.forEach(function(a) { a.classList.toggle('active', a.getAttribute('href') === '#' + id); });
    }
    var observer = new IntersectionObserver(function(entries) {
      entries.forEach(function(entry) {
        if (entry.isIntersecting) setActive(entry.target.id);
      });
    }, { rootMargin: '-80px 0px -70% 0px' });
    sections.forEach(function(s) { observer.observe(s); });

    // "Unclear or wrong?" under every section: a prefilled GitHub Discussion.
    sections.forEach(function(s) {
      var h = s.querySelector('h1, h2');
      if (!h) return;
      var page = location.origin + location.pathname + '#' + s.id;
      var p = document.createElement('p');
      p.className = 'doc-feedback';
      var a = document.createElement('a');
      a.href = 'https://github.com/agentraas/agentraas/discussions/new?category=q-a' +
        '&title=' + encodeURIComponent('Docs: ' + h.textContent.trim()) +
        '&body=' + encodeURIComponent('About ' + page + '\n\nWhat was unclear, wrong or missing:\n');
      a.target = '_blank';
      a.rel = 'noopener';
      a.textContent = 'Something unclear or wrong here? Ask on GitHub';
      p.appendChild(a);
      s.appendChild(p);
    });

    // Search: section titles, sub-headings and tutorial cases on this page,
    // including text inside closed case boxes (Ctrl+F skips those).
    var sidebar = document.querySelector('.docs-sidebar');
    if (!sidebar) return;
    var entries = [];
    sections.forEach(function(s) {
      var h = s.querySelector('h1, h2');
      var title = h ? h.textContent.trim() : s.id;
      entries.push({ el: s, title: title, where: '', text: s.textContent });
      s.querySelectorAll('h3, details.case > summary').forEach(function(sub) {
        var box = sub.closest('details') || sub;
        // a case's summary ends in its status chip; the title is the text before it
        entries.push({ el: box, title: sub.firstChild.textContent.trim(), where: title, text: box.textContent + ' ' + title });
      });
    });

    var wrap = document.createElement('div');
    wrap.className = 'doc-search';
    var input = document.createElement('input');
    input.type = 'search';
    input.placeholder = onTutorials ? 'Search the tutorials' : 'Search the docs';
    input.setAttribute('aria-label', input.placeholder);
    var list = document.createElement('div');
    list.className = 'doc-search-results';
    list.setAttribute('role', 'listbox');
    wrap.appendChild(input);
    wrap.appendChild(list);
    sidebar.insertBefore(wrap, sidebar.firstChild);

    function go(entry) {
      if (entry.el.tagName === 'DETAILS') entry.el.open = true;
      entry.el.scrollIntoView({ block: 'start' });
      window.scrollBy(0, -80);
      list.textContent = '';
    }

    function snippet(text, word) {
      var flat = text.replace(/\s+/g, ' ');
      var i = flat.toLowerCase().indexOf(word);
      if (i < 0) return '';
      var start = Math.max(0, i - 40);
      return (start ? '...' : '') + flat.slice(start, i + 80).trim() + '...';
    }

    function run() {
      var q = input.value.trim().toLowerCase();
      list.textContent = '';
      if (q.length < 2) return;
      var words = q.split(/\s+/);
      var hits = entries.map(function(e) {
        var t = e.title.toLowerCase(), body = e.text.toLowerCase();
        if (!words.every(function(w) { return body.indexOf(w) >= 0; })) return null;
        var score = words.reduce(function(n, w) { return n + (t.indexOf(w) >= 0 ? 10 : 1); }, 0);
        return { e: e, score: score };
      }).filter(Boolean).sort(function(a, b) { return b.score - a.score; }).slice(0, 8);
      hits.forEach(function(h) {
        var b = document.createElement('button');
        b.type = 'button';
        var t = document.createElement('b');
        t.textContent = h.e.title;
        var s = document.createElement('span');
        s.textContent = (h.e.where ? h.e.where + ': ' : '') + snippet(h.e.text, words[0]);
        b.appendChild(t);
        b.appendChild(s);
        b.addEventListener('click', function() { go(h.e); });
        list.appendChild(b);
      });
      var other = document.createElement('a');
      other.href = (onTutorials ? '/docs' : TUTORIALS) + '?q=' + encodeURIComponent(input.value.trim());
      other.textContent = hits.length ? (onTutorials ? 'Search the docs too' : 'Search the tutorials too') :
        'Nothing here. ' + (onTutorials ? 'Search the docs' : 'Search the tutorials');
      list.appendChild(other);
    }
    input.addEventListener('input', run);
    input.addEventListener('keydown', function(e) {
      if (e.key === 'Enter') { var first = list.querySelector('button'); if (first) first.click(); }
      if (e.key === 'Escape') { input.value = ''; list.textContent = ''; }
    });
    var q = new URLSearchParams(location.search).get('q');
    if (q) { input.value = q; run(); }

    // The lines added above, and the fonts, move sections after the browser
    // has jumped to the #anchor: jump again once the page has settled.
    function toAnchor() {
      var t = location.hash && document.getElementById(location.hash.slice(1));
      if (t) t.scrollIntoView();
    }
    toAnchor();
    window.addEventListener('load', toAnchor);
  })();

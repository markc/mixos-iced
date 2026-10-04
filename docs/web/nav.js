/* MixOS docs navigation — progressive enhancement over prerendered pages.
 *
 * Every URL on this site is a complete static HTML page written by
 * build/gen-site.mix; nothing is rendered in the browser and there is no
 * hash routing. This script only makes same-site navigation smooth: a click
 * on a page link fetches that page's real HTML and swaps in <main> and the
 * "On this page" panel, so the DCS sidebars, carousels and Appearance state
 * stay put. Without JS every link is an ordinary page load and the site
 * works the same.
 *
 * Copyright © 2026 Mark Constable <mc@mixos.dev> (MIT OR Apache-2.0)
 */
(function () {
    'use strict';

    /* Template/asset version stamped by the generator. A fetched page from a
       different build gets a full load instead of a swap, so new HTML never
       runs against an open tab's old CSS/JS. */
    function buildOf(doc) {
        var m = doc.querySelector('meta[name="mixos-build"]');
        return m ? m.getAttribute('content') : '';
    }
    var BUILD = buildOf(document);
    var currentPath = location.pathname;
    var seq = 0;

    /* Page URLs are directory-style: "/", "/try/", "/mix/syntax/". Anything
       with a file extension (.md, .txt, .xml, fonts) is a real file — leave
       it to the browser. */
    function isPage(url) {
        return url.origin === location.origin && /\/$/.test(url.pathname);
    }

    function panelIndexOf(el) {
        var panel = el.closest('.panel');
        return panel ? Array.prototype.indexOf.call(panel.parentNode.children, panel) : -1;
    }

    /* Mark the sidebar link(s) for this page and slide the left carousel to
       the panel holding it (preferring the panel already showing), so the
       sidebar always shows where you are. */
    function setActive(path) {
        var matches = [];
        document.querySelectorAll('.sidebar a[href]').forEach(function (a) {
            var u;
            try { u = new URL(a.getAttribute('href'), location.href); } catch (e) { return; }
            var on = isPage(u) && !u.hash && u.pathname === path;
            a.classList.toggle('active', on);
            if (on) a.setAttribute('aria-current', 'page');
            else a.removeAttribute('aria-current');
            if (on && a.closest('.sidebar-left')) matches.push(a);
        });
        if (!matches.length) return;
        var activeDot = document.querySelector('.carousel-dot[data-sidebar="left"].active');
        var curIdx = activeDot ? parseInt(activeDot.dataset.panel, 10) : 0;
        var pick = null;
        matches.forEach(function (a) { if (!pick && panelIndexOf(a) === curIdx) pick = a; });
        if (!pick) pick = matches[0];
        var group = pick.closest('.sidebar-group');
        if (group) group.classList.remove('collapsed');
        var idx = panelIndexOf(pick);
        if (idx >= 0 && idx !== curIdx) {
            if (window.Base && Base.setPanel) Base.setPanel('left', idx);
            else {
                var dot = document.querySelector('.carousel-dot[data-sidebar="left"][data-panel="' + idx + '"]');
                if (dot) dot.click();
            }
        }
    }

    function scrollToHash(hash) {
        var id = '';
        try { id = hash ? decodeURIComponent(hash.slice(1)) : ''; } catch (e) { id = ''; }
        var el = id && document.getElementById(id);
        if (el) el.scrollIntoView({ block: 'start' });
        else window.scrollTo(0, 0);
    }

    /* Per-page head entries travel with the content, so copy-link, bookmarks
       and agents reading the live DOM see the right canonical/markdown URL. */
    var HEAD_SWAP = [
        'meta[name="description"]',
        'meta[name="robots"]',
        'link[rel="canonical"]',
        'link[rel="alternate"][type="text/markdown"]'
    ];
    function swapHead(doc) {
        document.title = doc.title;
        HEAD_SWAP.forEach(function (sel) {
            var cur = document.head.querySelector(sel);
            var next = doc.head.querySelector(sel);
            if (cur && next) cur.replaceWith(document.importNode(next, true));
            else if (cur) cur.remove();
            else if (next) document.head.appendChild(document.importNode(next, true));
        });
    }

    function load(url, push, restoreY) {
        var my = ++seq;
        fetch(url.pathname, { headers: { 'Accept': 'text/html' } })
            .then(function (r) {
                var type = r.headers.get('content-type') || '';
                if (!r.ok || type.indexOf('text/html') === -1) throw new Error('not a page');
                return r.text();
            })
            .then(function (text) {
                if (my !== seq) return;
                var doc = new DOMParser().parseFromString(text, 'text/html');
                var main = doc.getElementById('content');
                var toc = doc.getElementById('toc-nav');
                if (buildOf(doc) !== BUILD || !main || !toc) { location.assign(url.href); return; }
                if (push) {
                    history.replaceState({ y: window.scrollY }, '');
                    history.pushState({ y: 0 }, '', url.pathname + url.search + url.hash);
                }
                currentPath = url.pathname;
                swapHead(doc);
                document.getElementById('content').replaceWith(document.importNode(main, true));
                document.getElementById('toc-nav').innerHTML = toc.innerHTML;
                setActive(currentPath);
                /* site.js only initialises what was in the DOM at load. */
                if (window.Site) {
                    if (Site.initScrollReveal) Site.initScrollReveal();
                    if (Site.initFooterYear) Site.initFooterYear();
                    if (Site.initDeck) Site.initDeck();
                }
                var m = document.getElementById('content');
                if (m) m.focus({ preventScroll: true });
                if (typeof restoreY === 'number') window.scrollTo(0, restoreY);
                else scrollToHash(url.hash);
            })
            .catch(function () {
                if (my === seq) location.assign(url.href);
            });
    }

    document.addEventListener('click', function (e) {
        /* Modified clicks (new tab/window, download) keep browser behaviour —
           every href is a real URL, so they just work. */
        if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
        var a = e.target.closest('a[href]');
        if (!a || a.hasAttribute('download')) return;
        var target = a.getAttribute('target');
        if (target && target !== '_self') return;
        var url;
        try { url = new URL(a.getAttribute('href'), location.href); } catch (err) { return; }
        if (!isPage(url)) return;
        if (url.pathname === location.pathname) {
            if (url.hash) return;           /* in-page anchor: native scroll */
            e.preventDefault();
            window.scrollTo(0, 0);
            return;
        }
        e.preventDefault();
        load(url, true);
    });

    if ('scrollRestoration' in history) history.scrollRestoration = 'manual';

    window.addEventListener('popstate', function (e) {
        var y = e.state && typeof e.state.y === 'number' ? e.state.y : undefined;
        if (location.pathname === currentPath) {
            /* Same page, different anchor (native hash navigation). */
            if (typeof y === 'number') window.scrollTo(0, y);
            else scrollToHash(location.hash);
            return;
        }
        load(new URL(location.href), false, y);
    });

    function init() {
        /* After base.js has restored the carousel from localStorage. */
        setTimeout(function () { setActive(location.pathname); }, 0);
        /* First visit: the FOUC head script paints scheme-mono by default, but
           base.js marks the 'default' (Ocean) button when nothing is stored.
           Persist mono so the Appearance panel agrees with the paint. */
        var st;
        try { st = JSON.parse(localStorage.getItem('base-state') || '{}'); } catch (e) { st = {}; }
        if (!st.scheme && window.Base && Base.setScheme) Base.setScheme('mono');
    }
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
    else init();
})();

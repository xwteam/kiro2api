/* kiro2api Admin UI v2 — app.js  (loads LAST)
   Orchestrator: login gate, section router, header wiring (theme + language +
   logout), mobile sidebar toggle, and per-section lazy init/refresh.

   Section registry: each sec-*.js sets
       K.sections.<name> = { init?, onShow?, onHide? }
   where <name> is one of the canonical section names below and the matching
   DOM node is <section id="sec-<name>">. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};

  var SECTION_KEY = 'kiro2api_section';
  var VALID = ['dashboard', 'accounts', 'models', 'apikeys', 'modeltest', 'usage', 'logs', 'settings'];
  var DEFAULT_SECTION = 'dashboard';

  var currentSection = null;
  var shellInited = false;

  // ---------- section router ----------
  function showSection(name) {
    if (VALID.indexOf(name) < 0) name = DEFAULT_SECTION;
    var prev = currentSection;
    if (prev === name) { // still fire onShow to refresh
      K.sections[name] && K.sections[name].onShow && K.sections[name].onShow();
      return;
    }

    document.querySelectorAll('.section').forEach(function (s) { s.classList.remove('active'); });
    var el = document.getElementById('sec-' + name);
    if (el) el.classList.add('active');

    document.querySelectorAll('.nav-item').forEach(function (n) {
      n.classList.toggle('active', n.getAttribute('data-section') === name);
    });

    try { localStorage.setItem(SECTION_KEY, name); } catch (e) {}
    currentSection = name;

    var sec = K.sections[name];
    if (sec) {
      if (!sec._inited && typeof sec.init === 'function') { try { sec.init(); } catch (e) { console.error(e); } sec._inited = true; }
      if (typeof sec.onShow === 'function') { try { sec.onShow(); } catch (e) { console.error(e); } }
    }
    if (prev && prev !== name) {
      var p = K.sections[prev];
      if (p && typeof p.onHide === 'function') { try { p.onHide(); } catch (e) { console.error(e); } }
    }
    closeSidebar();
  }
  K.showSection = showSection;

  // ---------- header: theme ----------
  function wireTheme() {
    var btn = document.getElementById('themeToggleBtn');
    if (!btn) return;
    function paint() {
      var dark = K.theme.get() === 'dark';
      var sun = btn.querySelector('.icon-sun');
      var moon = btn.querySelector('.icon-moon');
      if (sun) sun.style.display = dark ? 'none' : '';
      if (moon) moon.style.display = dark ? '' : 'none';
    }
    btn.addEventListener('click', function () { K.theme.toggle(); });
    window.addEventListener('themechange', paint);
    paint();
  }

  // ---------- header: language ----------
  function wireLang() {
    var btn = document.getElementById('langBtn');
    var menu = document.getElementById('langMenu');
    if (!btn || !menu) return;
    var labelEl = btn.querySelector('.lang-label');

    function paintLabel() {
      var cur = K.i18n.getLang();
      if (labelEl) labelEl.textContent = K.i18n.LABELS[cur] || cur;
      menu.querySelectorAll('button').forEach(function (b) {
        b.classList.toggle('active', b.getAttribute('data-lang') === cur);
      });
    }

    // Build menu items from the supported list (native labels).
    menu.innerHTML = '';
    K.i18n.SUPPORTED.forEach(function (code) {
      var b = document.createElement('button');
      b.setAttribute('data-lang', code);
      b.textContent = K.i18n.LABELS[code] || code;
      b.addEventListener('click', function () {
        K.i18n.setLang(code);
        menu.classList.remove('open');
      });
      menu.appendChild(b);
    });

    btn.addEventListener('click', function (e) {
      e.stopPropagation();
      menu.classList.toggle('open');
    });
    document.addEventListener('click', function () { menu.classList.remove('open'); });
    menu.addEventListener('click', function (e) { e.stopPropagation(); });
    window.addEventListener('langchange', paintLabel);
    paintLabel();
  }

  // ---------- header: logout ----------
  function wireLogout() {
    var btn = document.getElementById('logoutBtn');
    if (!btn) return;
    btn.addEventListener('click', function () {
      K.api.clearSession();               // clears admin + master key
      location.reload();
    });
  }

  // ---------- header: running status badge ----------
  // Lightweight liveness: ping /health on load + every 20s, toggle running/offline.
  // Exposed setter (K._setServerStatus) lets the restart flow drive the badge.
  function wireStatus() {
    var badge = document.getElementById('serverStatus');
    if (!badge) return;
    var textEl = badge.querySelector('.status-text');
    function setState(state) { // 'running' | 'offline' | 'restarting'
      badge.classList.toggle('is-offline', state === 'offline');
      badge.classList.toggle('is-restarting', state === 'restarting');
      var key = state === 'offline' ? 'header.statusOffline'
        : (state === 'restarting' ? 'header.statusRestarting' : 'header.statusRunning');
      if (textEl) {
        textEl.setAttribute('data-i18n', key);
        textEl.textContent = K.i18n ? K.i18n.t(key) : textEl.textContent;
      }
    }
    K._setServerStatus = setState;
    function ping() {
      if (badge.classList.contains('is-restarting')) return; // restart flow owns the badge
      fetch('/health', { cache: 'no-store' })
        .then(function (r) { setState(r && r.ok ? 'running' : 'offline'); })
        .catch(function () { setState('offline'); });
    }
    ping();
    setInterval(ping, 20000);
  }

  // ---------- header: restart ----------
  // Confirm → POST /api/admin/restart?confirm=true → process exits → container
  // restart policy brings it back → poll /health until up → reload.
  function wireRestart() {
    var btn = document.getElementById('restartBtn');
    if (!btn) return;
    btn.addEventListener('click', function () {
      confirmModal('header.confirmRestart').then(function (ok) {
        if (!ok) return;
        btn.disabled = true;
        btn.classList.add('is-restarting');
        if (K._setServerStatus) K._setServerStatus('restarting');
        K.api.request('/restart', { method: 'POST', query: { confirm: 'true' } })
          .then(function () {
            K.api.toast(K.i18n.t('header.restarting'), 'info');
            waitForBack();
          })
          .catch(function (e) {
            btn.disabled = false;
            btn.classList.remove('is-restarting');
            if (K._setServerStatus) K._setServerStatus('running');
            K.api.toast((e && e.message) || K.i18n.t('common.error'), 'error');
          });
      });
    });

    function waitForBack() {
      // Wait past the server's 0.5s delayed exit before polling for it to return,
      // so we don't see the still-alive old process and reload prematurely.
      setTimeout(function () {
        var tries = 0;
        var timer = setInterval(function () {
          tries++;
          fetch('/health', { cache: 'no-store' })
            .then(function (r) {
              if (r && r.ok) {
                clearInterval(timer);
                K.api.toast(K.i18n.t('header.restarted'), 'success');
                setTimeout(function () { location.reload(); }, 600);
              }
            })
            .catch(function () { /* still down — keep waiting */ });
          if (tries > 60) { // ~2 min ceiling
            clearInterval(timer);
            btn.disabled = false;
            btn.classList.remove('is-restarting');
            if (K._setServerStatus) K._setServerStatus('offline');
            K.api.toast(K.i18n.t('header.restartTimeout'), 'error');
          }
        }, 2000);
      }, 2000);
    }
  }

  // ---------- lightweight confirm modal (mounts into #modalRoot) ----------
  function confirmModal(msgKey) {
    return new Promise(function (resolve) {
      var t = function (k) { return K.i18n ? K.i18n.t(k) : k; };
      var root = document.getElementById('modalRoot') || document.body;
      var overlay = document.createElement('div'); overlay.className = 'modal-overlay active';
      var modal = document.createElement('div'); modal.className = 'modal';
      var head = document.createElement('div'); head.className = 'modal-header';
      var h3 = document.createElement('h3'); h3.textContent = t('common.confirm'); head.appendChild(h3);
      var x = document.createElement('button'); x.className = 'modal-close'; x.type = 'button'; x.textContent = '✕'; head.appendChild(x);
      modal.appendChild(head);
      var body = document.createElement('div'); body.className = 'modal-body';
      var p = document.createElement('p'); p.textContent = t(msgKey); body.appendChild(p); modal.appendChild(body);
      var foot = document.createElement('div'); foot.className = 'modal-footer';
      var cancel = document.createElement('button'); cancel.className = 'btn btn-outline'; cancel.type = 'button'; cancel.textContent = t('common.cancel');
      var ok = document.createElement('button'); ok.className = 'btn btn-primary'; ok.type = 'button'; ok.textContent = t('common.confirm');
      foot.appendChild(cancel); foot.appendChild(ok); modal.appendChild(foot);
      overlay.appendChild(modal); root.appendChild(overlay);
      var resolved = false;
      function done(v) { if (resolved) return; resolved = true; overlay.remove(); resolve(v); }
      x.addEventListener('click', function () { done(false); });
      cancel.addEventListener('click', function () { done(false); });
      ok.addEventListener('click', function () { done(true); });
      overlay.addEventListener('click', function (e) { if (e.target === overlay) done(false); });
    });
  }

  // ---------- sidebar nav + mobile toggle ----------
  function wireNav() {
    document.querySelectorAll('.nav-item').forEach(function (n) {
      n.addEventListener('click', function () { showSection(n.getAttribute('data-section')); });
    });
    var toggle = document.getElementById('sidebarToggle');
    var backdrop = document.getElementById('sidebarBackdrop');
    if (toggle) toggle.addEventListener('click', function () {
      var sb = document.getElementById('sidebar');
      var open = sb.classList.toggle('open');
      if (backdrop) backdrop.classList.toggle('show', open);
    });
    if (backdrop) backdrop.addEventListener('click', closeSidebar);
  }
  function closeSidebar() {
    var sb = document.getElementById('sidebar');
    var backdrop = document.getElementById('sidebarBackdrop');
    if (sb) sb.classList.remove('open');
    if (backdrop) backdrop.classList.remove('show');
  }

  // ---------- live language switch ----------
  // When the language changes we must (a) re-fill every static [data-i18n]
  // element in the whole document, and (b) RE-RENDER the currently visible
  // section so any strings it composed at render time via t('key') (select
  // options, dynamic labels, freshly-built rows, etc.) are rebuilt in the new
  // language. Individual sections may ALSO listen for 'langchange' to retranslate
  // pieces themselves, but that is not enough for sections (e.g. settings) that
  // only build their content inside onShow/init — those never re-run on a plain
  // language switch, so their content stays baked in the old language until a
  // manual refresh. Driving onShow() from here guarantees EVERY section updates
  // live, uniformly, regardless of whether it wired its own listener.
  function wireLiveLangSwitch() {
    window.addEventListener('langchange', function () {
      // (a) re-apply static translations across the document (idempotent).
      try { K.i18n.apply(document); } catch (e) { console.error(e); }

      // (b) re-render the visible section. Only when a section is actually
      // shown and initialized — langchange can fire from the login overlay
      // before any section exists, and re-rendering an un-inited section would
      // skip its one-time init() and render into a half-built shell.
      if (!currentSection) return;
      var sec = K.sections[currentSection];
      if (!sec) return;
      // If init() has not run yet, run it once (mirrors the router) so the
      // shell exists before onShow() paints dynamic content into it.
      if (!sec._inited && typeof sec.init === 'function') {
        try { sec.init(); } catch (e) { console.error(e); }
        sec._inited = true;
      }
      if (typeof sec.onShow === 'function') {
        try { sec.onShow(); } catch (e) { console.error(e); }
      }
    });
  }

  // ---------- shell init (after login) ----------
  function initShell() {
    if (shellInited) return;
    shellInited = true;
    wireTheme();
    wireLang();
    wireStatus();
    wireRestart();
    wireLogout();
    wireNav();
    wireLiveLangSwitch();
    K.i18n.apply(document);
  }

  function restoredSection() {
    var s;
    try { s = localStorage.getItem(SECTION_KEY); } catch (e) {}
    return (s && VALID.indexOf(s) >= 0) ? s : DEFAULT_SECTION;
  }

  function enterApp() {
    var overlay = document.getElementById('loginOverlay');
    if (overlay) overlay.classList.remove('active');
    initShell();
    showSection(restoredSection());
  }

  // ---------- login gate ----------
  function wireLogin() {
    var overlay = document.getElementById('loginOverlay');
    var input = document.getElementById('loginKeyInput');
    var submit = document.getElementById('loginSubmitBtn');
    var errEl = document.getElementById('loginError');

    function showError(show) { if (errEl) errEl.classList.toggle('show', !!show); }

    async function attempt(key) {
      if (!key) { showError(true); return; }
      showError(false);
      if (submit) submit.disabled = true;
      K.api.setAdminKey(key);
      try {
        await K.api.get('/credentials');   // validation probe
        enterApp();
      } catch (e) {
        K.api.setAdminKey('');
        showError(true);
        if (input) { input.focus(); input.select(); }
      } finally {
        if (submit) submit.disabled = false;
      }
    }

    if (submit) submit.addEventListener('click', function () { attempt(input ? input.value.trim() : ''); });
    if (input) input.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { e.preventDefault(); attempt(input.value.trim()); }
    });

    // If a stored key exists, validate silently; otherwise show the overlay.
    if (K.api.hasAdminKey()) {
      K.api.get('/credentials').then(enterApp).catch(function () {
        K.api.setAdminKey('');
        if (overlay) overlay.classList.add('active');
        if (input) input.focus();
      });
    } else {
      if (overlay) overlay.classList.add('active');
      if (input) input.focus();
    }
  }

  // If any admin call anywhere returns 401/403, drop back to the login gate.
  K.api.onUnauthorized = function () {
    // stop any live section work (e.g. logs SSE / timers)
    if (currentSection) {
      var sec = K.sections[currentSection];
      if (sec && typeof sec.onHide === 'function') { try { sec.onHide(); } catch (e) {} }
    }
    currentSection = null;
    var overlay = document.getElementById('loginOverlay');
    if (overlay) overlay.classList.add('active');
    var input = document.getElementById('loginKeyInput');
    if (input) { input.value = ''; input.focus(); }
  };

  // ---------- go ----------
  document.addEventListener('DOMContentLoaded', function () {
    // Apply i18n to the login overlay immediately (shell not yet inited).
    K.i18n.apply(document);
    wireLogin();
  });
})();

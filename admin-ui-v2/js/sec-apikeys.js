/* kiro2api Admin UI v2 — sec-apikeys.js
   API Key management section (#sec-apikeys).

   Visual structure is a vanilla-JS clean-room replica of the reference panel's
   ApiKeysPanel (React): a service-connection card, five status stat cards,
   a sort + search toolbar, and card-based key rows grouped into "绑定账号"
   (bound) and "全局策略" (global). Each key card shows serial (#NNN), name,
   status badge, bound-credential chips, masked key, created date, a
   limit/expiry line, and a usage line (requests / RPM / in-out tokens / cost).

   Data:
     GET    /api/admin/api-keys                 -> list
     GET    /api/admin/api-keys/usage           -> per-key usage summaries
     GET    /api/admin/api-keys/:id/usage        -> one summary (detail modal)
     GET    /api/admin/api-keys/:id/usage/records -> paged records (detail modal)
     GET    /api/admin/credentials              -> bindable accounts
     POST   /api/admin/api-keys                 -> create (returns full key)
     PUT    /api/admin/api-keys/:id             -> update
     DELETE /api/admin/api-keys/:id             -> delete
     DELETE /api/admin/api-keys/:id/usage        -> reset usage

   The create dialog mirrors the reference panel exactly: 编号 (serial) field with
   conflict check, a 限制方式 mode toggle (按日期 / 按额度), quick-duration
   chips + custom number + hours/days unit, or a metering-unit toggle
   (美元估算 / 真实 Credits) with quick-amount chips + custom, and a bound-
   accounts multi-select (下拉). On create the returned full key is shown once
   in a modal; the copy button copies ONLY the key string.

   Public entry: registers K.sections.apikeys = { init, onShow, onHide }.
   Every visible string uses a namespaced data-i18n key; dynamic values are
   set via textContent (XSS-safe). Rebuilds on the `langchange` event. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};
  var api = K.api;

  // ---- ensure our stylesheet is present (we own css/sections-manage.css) ----
  (function ensureCss() {
    if (document.querySelector('link[data-manage-css]')) return;
    var l = document.createElement('link');
    l.rel = 'stylesheet';
    l.href = 'css/sections-manage.css';
    l.setAttribute('data-manage-css', '1');
    document.head.appendChild(l);
  })();

  // ---------- small helpers ----------
  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }

  // Coerce an API response into an array. CRITICAL: check Array.isArray FIRST.
  // Never read `.keys` off a possibly-array value as an "array-holding" property —
  // on an Array that is Array.prototype.keys (a function), not our data.
  // `fields` lists object properties that may hold the array (e.g. 'apiKeys','data').
  function asList(resp, fields) {
    if (Array.isArray(resp)) return resp;
    if (resp && typeof resp === 'object') {
      for (var i = 0; i < fields.length; i++) {
        var v = resp[fields[i]];
        if (Array.isArray(v)) return v;
      }
    }
    return [];
  }
  function getKeyList(kd) { return asList(kd, ['apiKeys', 'data']); }
  function el(tag, cls, txt) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (txt != null) n.textContent = txt;
    return n;
  }
  // element whose textContent is filled by K.i18n.apply() via data-i18n
  function elI18n(tag, cls, key) {
    var n = el(tag, cls);
    n.setAttribute('data-i18n', key);
    n.textContent = t(key); // best-effort until apply() runs
    return n;
  }
  function fmtDateTime(s) {
    if (!s) return t('common.never');
    var d = new Date(s);
    return isNaN(d.getTime()) ? String(s) : d.toLocaleString();
  }
  function fmtInt(n) { n = Number(n); return isFinite(n) ? Math.round(n).toLocaleString() : '0'; }
  function fmtCost(n) { n = Number(n); return isFinite(n) ? n.toFixed(2) : '0.00'; }
  function fmtCost4(n) { n = Number(n); return isFinite(n) ? n.toFixed(4) : '0.0000'; }
  function fmtCredits(n) { n = Number(n); return isFinite(n) ? n.toFixed(2) : '0.00'; }
  function fmtCredits4(n) { n = Number(n); return isFinite(n) ? n.toFixed(4) : '0.0000'; }
  // detail dialog: locale-grouped token count (matches reference formatTokens)
  function formatTokens(n) { n = Number(n); return isFinite(n) ? Math.round(n).toLocaleString('zh-CN') : '0'; }
  // detail dialog: $X.XXXX (matches reference formatCost)
  function formatCost(n) { return '$' + fmtCost4(n); }
  var CREDIT_RATE = 0.72; // 1 credit ≈ $0.72 (matches reference divisor)

  // model → color class (matches reference getModelColor: opus/sonnet/haiku)
  var MODEL_COLORS = { opus: 'model-opus', sonnet: 'model-sonnet', haiku: 'model-haiku' };
  function getModelColor(model) {
    var lower = String(model || '').toLowerCase();
    for (var key in MODEL_COLORS) {
      if (MODEL_COLORS.hasOwnProperty(key) && lower.indexOf(key) >= 0) return MODEL_COLORS[key];
    }
    return 'model-default';
  }
  function padId(id) { var s = String(id); return s.length >= 3 ? s : ('000' + s).slice(-3); }
  function serial(id) { return '#' + padId(id); }

  function copy(text) {
    try {
      navigator.clipboard.writeText(text).then(function () { api.toast(t('common.copied'), 'success'); });
    } catch (e) {
      var ta = document.createElement('textarea'); ta.value = text; document.body.appendChild(ta);
      ta.select(); try { document.execCommand('copy'); api.toast(t('common.copied'), 'success'); } catch (_) {}
      ta.remove();
    }
  }
  function mask(k) {
    if (!k) return '••••';
    if (k.length <= 12) return k.slice(0, 3) + '…' + k.slice(-2);
    return k.slice(0, 7) + '…' + k.slice(-4);
  }

  // the reference panel duration helpers ------------------------------------------
  function toDays(value, unit) { return unit === 'hours' ? Number(value) / 24 : Number(value); }
  function formatDuration(days) {
    days = Number(days);
    if (days < 1) {
      var hours = Math.round(days * 24 * 100) / 100;
      return t('keyForm.durHours', { n: hours });
    }
    return t('keyForm.durDays', { n: days });
  }
  var QUICK_DURATIONS = [
    { labelKey: 'keyForm.quick1h', value: 1, unit: 'hours' },
    { labelKey: 'keyForm.quick3h', value: 3, unit: 'hours' },
    { labelKey: 'keyForm.quick6h', value: 6, unit: 'hours' },
    { labelKey: 'keyForm.quick12h', value: 12, unit: 'hours' },
    { labelKey: 'keyForm.quick1d', value: 1, unit: 'days' },
    { labelKey: 'keyForm.quick3d', value: 3, unit: 'days' },
    { labelKey: 'keyForm.quick7d', value: 7, unit: 'days' }
  ];

  // ---------- lucide icons (inline SVG, matching the reference panel's icon set) ----------
  var LUCIDE = {
    key: '<path d="m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4"/><path d="m21 2-9.6 9.6"/><circle cx="7.5" cy="15.5" r="5.5"/>',
    copy: '<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>',
    check: '<path d="M20 6 9 17l-5-5"/>',
    clock: '<circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/>',
    dollar: '<line x1="12" x2="12" y1="2" y2="22"/><path d="M17 5H9.5a3.5 3.5 0 0 0 0 7h5a3.5 3.5 0 0 1 0 7H6"/>',
    barChart: '<line x1="12" x2="12" y1="20" y2="10"/><line x1="18" x2="18" y1="20" y2="4"/><line x1="6" x2="6" y1="20" y2="16"/>',
    rotate: '<path d="M3 2v6h6"/><path d="M3 8a9 9 0 1 0 3-6.7L3 8"/>',
    fileText: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/>',
    pencil: '<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>',
    trash: '<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>',
    link: '<path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/>',
    globe: '<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>',
    search: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
    plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',
    sort: '<path d="M11 5h10"/><path d="M11 9h7"/><path d="M11 13h4"/><path d="m3 17 3 3 3-3"/><path d="M6 18V4"/>'
  };
  function svgIcon(name, cls) {
    var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    svg.setAttribute('aria-hidden', 'true');
    if (cls) svg.setAttribute('class', cls);
    svg.innerHTML = LUCIDE[name] || '';
    return svg;
  }
  function iconBtn(iconName, titleKey, extraCls, onClick) {
    var b = el('button', 'acct-icon-btn' + (extraCls ? ' ' + extraCls : ''));
    b.type = 'button';
    b.setAttribute('data-i18n-title', titleKey);
    b.title = t(titleKey);
    b.appendChild(svgIcon(iconName));
    b.addEventListener('click', function () { onClick(b); });
    return b;
  }
  function switchToggle(checked, titleKey, onToggle) {
    var wrap = el('button', 'acct-switch' + (checked ? ' is-on' : ''));
    wrap.type = 'button';
    wrap.setAttribute('role', 'switch');
    wrap.setAttribute('aria-checked', checked ? 'true' : 'false');
    wrap.setAttribute('data-i18n-title', titleKey);
    wrap.title = t(titleKey);
    wrap.appendChild(el('span', 'acct-switch-thumb'));
    wrap.addEventListener('click', function () { onToggle(wrap); });
    return wrap;
  }

  // ---------- generic modal (mounts into #modalRoot) ----------
  function openModal(opts) {
    var root = document.getElementById('modalRoot') || document.body;
    var overlay = el('div', 'modal-overlay active');
    var modal = el('div', 'modal' + (opts.size === 'lg' ? ' modal-lg' : ''));

    var head = el('div', 'modal-header');
    // Title + optional subtitle are grouped in a left-hand column so the
    // subtitle sits directly UNDER the title and ABOVE the header divider.
    var headText = el('div', 'modal-head-text');
    headText.appendChild(el('h3', null, opts.title || ''));
    if (opts.descKey) headText.appendChild(elI18n('p', 'modal-desc', opts.descKey));
    head.appendChild(headText);
    var closeBtn = el('button', 'modal-close'); closeBtn.type = 'button';
    closeBtn.setAttribute('aria-label', t('common.close'));
    closeBtn.textContent = '✕';
    head.appendChild(closeBtn);
    modal.appendChild(head);

    var body = el('div', 'modal-body');
    if (opts.bodyEl) body.appendChild(opts.bodyEl);
    else if (opts.bodyHtml != null) body.innerHTML = opts.bodyHtml;
    modal.appendChild(body);

    if (opts.footerEl) modal.appendChild(opts.footerEl);

    overlay.appendChild(modal);
    root.appendChild(overlay);
    if (K.i18n) K.i18n.apply(modal);

    function close() {
      overlay.remove();
      if (typeof opts.onClose === 'function') opts.onClose();
    }
    closeBtn.addEventListener('click', close);
    overlay.addEventListener('click', function (e) { if (e.target === overlay) close(); });

    return { overlay: overlay, modal: modal, body: body, close: close };
  }

  function confirmModal(msgKey, msgVars, danger) {
    return new Promise(function (resolve) {
      var footer = el('div', 'modal-footer');
      var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
      var ok = el('button', 'btn ' + (danger === false ? 'btn-primary' : 'btn-danger'), t('common.confirm'));
      ok.type = 'button';
      footer.appendChild(cancel); footer.appendChild(ok);
      var body = el('div');
      body.appendChild(el('p', null, t(msgKey, msgVars)));
      var resolved = false;
      var m = openModal({
        title: t('common.confirm'), bodyEl: body, footerEl: footer,
        onClose: function () { if (!resolved) { resolved = true; resolve(false); } }
      });
      cancel.addEventListener('click', function () { m.close(); });
      ok.addEventListener('click', function () { resolved = true; resolve(true); m.close(); });
    });
  }

  // segmented control: array of [key, label i18n key, isActive] with onSelect(key)
  function segmented(options, current, onSelect) {
    var wrap = el('div', 'seg-group');
    options.forEach(function (o) {
      var b = elI18n('button', 'seg-btn' + (o.key === current ? ' is-active' : ''), o.labelKey);
      b.type = 'button';
      if (o.iconName) b.insertBefore(svgIcon(o.iconName, 'seg-ico'), b.firstChild);
      b.addEventListener('click', function () { onSelect(o.key, wrap); });
      wrap.appendChild(b);
    });
    return wrap;
  }
  function segSetActive(wrap, index) {
    var kids = wrap.querySelectorAll('.seg-btn');
    for (var i = 0; i < kids.length; i++) kids[i].classList.toggle('is-active', i === index);
  }

  // ---------- state ----------
  var container = null;
  var keys = [];
  var usageByKey = {};   // apiKeyId -> summary
  var rpmByKey = {};     // apiKeyId -> rpm
  var accountsCache = null;
  var balanceByCred = {}; // credId -> balance (best-effort)
  var sortBy = 'newest'; // 'newest' | 'cost-desc' | 'cost-asc'
  var searchQuery = '';
  var serverInfo = {};
  var langBound = false;

  // ---------- section shell ----------
  function buildShell() {
    container = document.getElementById('sec-apikeys');
    if (!container) return;
    container.innerHTML = '';

    // service connection info card
    var svcCard = el('div', 'card svc-card'); svcCard.id = 'keySvcCard';
    container.appendChild(svcCard);

    // status stat cards
    var statGrid = el('div', 'key-stat-grid'); statGrid.id = 'keyStatGrid';
    container.appendChild(statGrid);

    // header + toolbar
    var header = el('div', 'section-header');
    header.appendChild(elI18n('h2', null, 'key.title'));

    var actions = el('div', 'section-actions');

    var sortWrap = el('div', 'key-sort');
    sortWrap.appendChild(svgIcon('sort', 'key-sort-ico'));
    sortWrap.appendChild(sortBtn('newest', 'key.sortNewest'));
    sortWrap.appendChild(sortBtn('cost-desc', 'key.sortCostDesc'));
    sortWrap.appendChild(sortBtn('cost-asc', 'key.sortCostAsc'));
    actions.appendChild(sortWrap);

    var addBtn = el('button', 'btn btn-primary'); addBtn.type = 'button';
    addBtn.appendChild(svgIcon('plus', 'btn-ico'));
    addBtn.appendChild(elI18n('span', null, 'key.add'));
    addBtn.addEventListener('click', function () { openKeyForm(null); });
    actions.appendChild(addBtn);

    var purgeBtn = el('button', 'btn btn-outline'); purgeBtn.type = 'button'; purgeBtn.id = 'keyPurgeBtn';
    purgeBtn.style.display = 'none';
    purgeBtn.appendChild(svgIcon('trash', 'btn-ico'));
    purgeBtn.appendChild(el('span', null, ''));
    purgeBtn.addEventListener('click', function () { openPurgeDialog(); });
    actions.appendChild(purgeBtn);

    var reloadBtn = elI18n('button', 'btn btn-outline', 'common.refresh'); reloadBtn.type = 'button';
    reloadBtn.addEventListener('click', function () { loadKeys(); });
    actions.appendChild(reloadBtn);

    header.appendChild(actions);
    container.appendChild(header);

    // search box
    var searchG = el('div', 'key-search-wrap');
    searchG.appendChild(svgIcon('search', 'key-search-ico'));
    var searchIn = el('input', 'form-control key-search'); searchIn.type = 'text'; searchIn.id = 'keySearch';
    searchIn.setAttribute('data-i18n-ph', 'key.searchPh');
    searchIn.setAttribute('placeholder', t('key.searchPh'));
    searchIn.value = searchQuery;
    searchIn.addEventListener('input', function () { searchQuery = searchIn.value; renderList(); });
    searchG.appendChild(searchIn);
    container.appendChild(searchG);

    // list mount
    var list = el('div', 'key-list'); list.id = 'keyList';
    list.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    container.appendChild(list);

    if (K.i18n) K.i18n.apply(container);

    if (!langBound) {
      langBound = true;
      window.addEventListener('langchange', function () {
        if (container && document.getElementById('keyList')) { renderSvcCard(); renderStatGrid(); renderList(); }
      });
    }
  }
  function sortBtn(key, labelKey) {
    var b = elI18n('button', 'btn btn-sm ' + (sortBy === key ? 'btn-primary' : 'btn-outline'), labelKey);
    b.type = 'button'; b.setAttribute('data-sort', key);
    b.addEventListener('click', function () {
      sortBy = key;
      var wrap = b.parentNode;
      wrap.querySelectorAll('[data-sort]').forEach(function (x) {
        var on = x.getAttribute('data-sort') === key;
        x.classList.toggle('btn-primary', on);
        x.classList.toggle('btn-outline', !on);
      });
      renderList();
    });
    return b;
  }

  // ---------- data load ----------
  function loadKeys() {
    var list = document.getElementById('keyList');
    if (list) { list.innerHTML = ''; list.appendChild(elI18n('div', 'empty-state', 'common.loading')); if (K.i18n) K.i18n.apply(list); }
    return Promise.allSettled([
      api.get('/api-keys'),
      api.get('/api-keys/usage'),
      api.get('/server-info'),
      api.get('/rpm')
    ]).then(function (res) {
      var kd = res[0].status === 'fulfilled' ? res[0].value : {};
      // Normalize: the backend returns a plain ARRAY. We MUST check Array.isArray
      // FIRST and never fall back to `kd.keys` — on an array `.keys` is the
      // built-in Array.prototype.keys METHOD (a truthy function), which would make
      // `keys` a function and later blow up `.forEach`. See getKeyList().
      keys = getKeyList(kd);

      usageByKey = {};
      if (res[1].status === 'fulfilled') {
        var ud = res[1].value;
        var ulist = asList(ud, ['usage', 'data']);
        ulist.forEach(function (u) {
          var id = u.apiKeyId != null ? u.apiKeyId : (u.keyId != null ? u.keyId : u.id);
          usageByKey[id] = u;
        });
      }

      serverInfo = res[2].status === 'fulfilled' ? (res[2].value || {}) : {};

      rpmByKey = {};
      if (res[3].status === 'fulfilled') {
        var rd = res[3].value || {};
        var byKey = rd.byApiKey || rd.byKey || {};
        Object.keys(byKey).forEach(function (k) { rpmByKey[k] = byKey[k]; });
      }

      renderSvcCard();
      renderStatGrid();
      renderList();

      // best-effort accounts + balances for chip labels (non-blocking)
      loadAccountsForBind();
    }).catch(function (e) {
      api.toast(e.message || t('common.error'), 'error');
      var l = document.getElementById('keyList');
      if (l) { l.innerHTML = ''; l.appendChild(elI18n('div', 'empty-state', 'common.error')); if (K.i18n) K.i18n.apply(l); }
    });
  }

  function loadAccountsForBind() {
    if (accountsCache) return Promise.resolve(accountsCache);
    return api.get('/credentials').then(function (d) {
      accountsCache = asList(d, ['credentials', 'data']);
      // refresh chips now that we have labels
      if (document.getElementById('keyList')) renderList();
      return accountsCache;
    }).catch(function () { accountsCache = []; return accountsCache; });
  }

  function credLabel(id) {
    if (accountsCache) {
      for (var i = 0; i < accountsCache.length; i++) {
        if (String(accountsCache[i].id) === String(id)) {
          return accountsCache[i].email || accountsCache[i].nickname || ('#' + id);
        }
      }
    }
    return '#' + id;
  }

  // ---------- key status (mirrors the reference panel getKeyStatus) ----------
  function getKeyStatus(k) {
    if (k.enabled === false) return 'disabled';
    if (k.expiresAt && new Date(k.expiresAt) <= new Date()) return 'expired';
    if (k.durationDays != null && !k.activatedAt) return 'pending';
    return 'active';
  }
  function statusMeta(status) {
    switch (status) {
      case 'active': return ['badge-success', 'key.statusActive'];
      case 'pending': return ['badge-muted', 'key.statusPending'];
      case 'expired': return ['badge-warning', 'key.statusExpired'];
      default: return ['badge-danger', 'key.statusDisabled'];
    }
  }
  function invalidKeys() {
    return keys.filter(function (k) { var s = getKeyStatus(k); return s === 'disabled' || s === 'expired'; });
  }

  // ---------- service connection card ----------
  function renderSvcCard() {
    var card = document.getElementById('keySvcCard');
    if (!card) return;
    card.innerHTML = '';
    var head = el('div', 'svc-card-head');
    head.appendChild(svgIcon('key', 'svc-ico'));
    head.appendChild(elI18n('span', null, 'key.svcTitle'));
    card.appendChild(head);

    var origin = window.location.origin;
    card.appendChild(svcRow('key.baseUrl', origin, origin, true));

    var masterKey = serverInfo.masterApiKey || serverInfo.masterKey || '';
    card.appendChild(svcRow('key.masterKey', masterKey ? mask(masterKey) : t('common.loading'), masterKey, false, !masterKey));

    if (K.i18n) K.i18n.apply(card);
  }
  function svcRow(labelKey, valueText, copyText, mono, disabled) {
    var row = el('div', 'svc-row');
    var left = el('div', 'svc-row-left');
    left.appendChild(elI18n('div', 'svc-row-label', labelKey));
    left.appendChild(el('code', 'svc-row-val' + (mono ? ' is-url' : ''), valueText));
    row.appendChild(left);
    var b = el('button', 'acct-icon-btn'); b.type = 'button';
    b.setAttribute('data-i18n-title', 'common.copy'); b.title = t('common.copy');
    b.appendChild(svgIcon('copy'));
    if (disabled) b.disabled = true;
    b.addEventListener('click', function () { if (copyText) copy(copyText); });
    row.appendChild(b);
    return row;
  }

  // ---------- status stat cards ----------
  function renderStatGrid() {
    var grid = document.getElementById('keyStatGrid');
    if (!grid) return;
    grid.innerHTML = '';
    var all = keys;
    var active = 0, pending = 0, disabled = 0, expired = 0;
    all.forEach(function (k) {
      var s = getKeyStatus(k);
      if (s === 'active') active++;
      else if (s === 'pending') pending++;
      else if (s === 'disabled') disabled++;
      else if (s === 'expired') expired++;
    });
    grid.appendChild(statCard('key.statTotal', all.length, 'neutral'));
    grid.appendChild(statCard('key.statActive', active, 'ok'));
    grid.appendChild(statCard('key.statPending', pending, 'muted'));
    grid.appendChild(statCard('key.statDisabled', disabled, 'bad'));
    grid.appendChild(statCard('key.statExpired', expired, 'warn'));
    if (K.i18n) K.i18n.apply(grid);

    // toggle purge button
    var pb = document.getElementById('keyPurgeBtn');
    if (pb) {
      var inv = invalidKeys().length;
      pb.style.display = inv > 0 ? '' : 'none';
      var span = pb.querySelector('span');
      if (span) span.textContent = t('key.purgeN', { n: inv });
    }
  }
  function statCard(labelKey, value, kind) {
    var c = el('div', 'card key-stat ' + (kind || 'neutral'));
    c.appendChild(elI18n('div', 'key-stat-label', labelKey));
    c.appendChild(el('div', 'key-stat-num', String(value)));
    return c;
  }

  // ---------- render list (bound + global groups) ----------
  function filterKeys() {
    var q = searchQuery.trim().toLowerCase();
    if (!q) return keys.slice();
    return keys.filter(function (k) {
      var sser = padId(k.id);
      return sser.indexOf(q) >= 0 || String(k.id).indexOf(q) >= 0 || String(k.name || '').toLowerCase().indexOf(q) >= 0;
    });
  }
  function usageCost(k) { var u = usageByKey[k.id]; return (u && Number(u.totalCost)) || 0; }
  function sortFn(a, b) {
    if (sortBy === 'cost-desc') return usageCost(b) - usageCost(a);
    if (sortBy === 'cost-asc') return usageCost(a) - usageCost(b);
    return new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime();
  }

  function renderList() {
    var list = document.getElementById('keyList');
    if (!list) return;
    list.innerHTML = '';

    if (!keys.length) {
      list.appendChild(elI18n('div', 'empty-state', 'key.emptyNone'));
      if (K.i18n) K.i18n.apply(list);
      return;
    }
    var filtered = filterKeys();
    if (!filtered.length) {
      list.appendChild(elI18n('div', 'empty-state', 'key.emptySearch'));
      if (K.i18n) K.i18n.apply(list);
      return;
    }

    var bound = filtered.filter(function (k) { return k.boundCredentialIds && k.boundCredentialIds.length > 0; }).sort(sortFn);
    var global = filtered.filter(function (k) { return !k.boundCredentialIds || k.boundCredentialIds.length === 0; }).sort(sortFn);

    if (bound.length) {
      var gb = el('div', 'key-group');
      var hb = el('div', 'key-group-head is-bound');
      hb.appendChild(svgIcon('link', 'key-group-ico'));
      hb.appendChild(elI18n('span', null, 'key.groupBound'));
      hb.appendChild(el('span', 'key-group-count', '(' + bound.length + ')'));
      gb.appendChild(hb);
      var gbl = el('div', 'key-cards');
      bound.forEach(function (k) { gbl.appendChild(buildKeyCard(k, true)); });
      gb.appendChild(gbl);
      list.appendChild(gb);
    }
    if (global.length) {
      var gg = el('div', 'key-group');
      var hg = el('div', 'key-group-head');
      hg.appendChild(svgIcon('globe', 'key-group-ico'));
      hg.appendChild(elI18n('span', null, 'key.groupGlobal'));
      hg.appendChild(el('span', 'key-group-count', '(' + global.length + ')'));
      gg.appendChild(hg);
      var ggl = el('div', 'key-cards');
      global.forEach(function (k) { ggl.appendChild(buildKeyCard(k, false)); });
      gg.appendChild(ggl);
      list.appendChild(gg);
    }

    if (K.i18n) K.i18n.apply(list);
  }

  function buildKeyCard(k, isBound) {
    var status = getKeyStatus(k);
    var u = usageByKey[k.id] || {};
    var card = el('div', 'key-card');
    if (status === 'disabled' || status === 'expired') card.classList.add('is-dim');
    if (isBound) card.classList.add('is-bound');

    var main = el('div', 'key-card-main');
    var info = el('div', 'key-card-info');

    // line 1: serial + name + status badge + bound chips
    var line1 = el('div', 'key-line1');
    line1.appendChild(el('code', 'key-serial', serial(k.id)));
    line1.appendChild(el('span', 'key-name', k.name || serial(k.id)));
    var sm = statusMeta(status);
    var badge = el('span', 'badge ' + sm[0]);
    badge.appendChild(elI18n('span', null, sm[1]));
    line1.appendChild(badge);
    if (isBound && k.boundCredentialIds && k.boundCredentialIds.length) {
      var chip = el('span', 'key-bound-chip');
      chip.appendChild(svgIcon('link', 'chip-ico'));
      k.boundCredentialIds.forEach(function (id, i) {
        if (i > 0) chip.appendChild(el('span', 'chip-sep', '·'));
        chip.appendChild(el('span', 'chip-label', credLabel(id)));
      });
      line1.appendChild(chip);
    }
    info.appendChild(line1);

    // line 2: masked key + created + limit/expiry
    var line2 = el('div', 'key-line2');
    line2.appendChild(el('code', 'key-mono', mask(k.key || k.apiKey || '')));
    var created = el('span', null);
    created.appendChild(elI18n('span', 'meta-k', 'key.metaCreated'));
    created.appendChild(document.createTextNode(' ' + fmtDateTime(k.createdAt)));
    line2.appendChild(created);
    line2.appendChild(buildLimitExpiry(k, u));
    info.appendChild(line2);

    // line 3: usage stats
    var line3 = el('div', 'key-line3');
    var reqStat = el('span', 'key-usg-item');
    reqStat.appendChild(svgIcon('barChart', 'usg-ico'));
    reqStat.appendChild(el('span', null, t('key.nRequests', { n: fmtInt(u.totalRequests) })));
    line3.appendChild(reqStat);
    line3.appendChild(el('span', 'key-usg-rpm', 'RPM ' + fmtInt(rpmByKey[String(k.id)] || 0)));
    var io = el('span', 'key-usg-io');
    io.textContent = t('key.inOut', { in: fmtInt(u.totalInputTokens), out: fmtInt(u.totalOutputTokens) });
    line3.appendChild(io);
    line3.appendChild(el('span', 'key-usg-cost', '$' + fmtCost(u.totalCost)));
    if (u.totalRequests > 0) {
      line3.appendChild(iconBtn('rotate', 'key.resetUsage', 'is-mini', function () { resetUsage(k); }));
    }
    info.appendChild(line3);

    main.appendChild(info);

    // right: actions cluster
    var act = el('div', 'key-card-actions');
    act.appendChild(iconBtn('fileText', 'key.viewLogs', null, function () { showDetail(k); }));
    act.appendChild(iconBtn('copy', 'key.copyInfo', null, function () {
      copy(t('key.copyBlock', { name: k.name || serial(k.id), url: window.location.origin, key: k.key || k.apiKey || '' }));
    }));
    act.appendChild(switchToggle(k.enabled !== false, k.enabled !== false ? 'acc.disable' : 'acc.enable',
      function () { toggleEnabled(k); }));
    act.appendChild(iconBtn('pencil', 'common.edit', null, function () { openKeyForm(k); }));
    act.appendChild(iconBtn('trash', 'common.delete', 'is-danger', function () { deleteKey(k); }));
    main.appendChild(act);

    card.appendChild(main);
    return card;
  }

  function buildLimitExpiry(k, u) {
    var span = el('span', 'key-limit');
    if (k.spendingLimit != null) {
      span.appendChild(svgIcon('dollar', 'meta-ico'));
      var used, limit;
      if (k.limitUnit === 'credits') {
        used = fmtCredits(u.totalCredits); limit = fmtCredits(k.spendingLimit);
        span.appendChild(el('span', null, t('key.quotaCredits', { used: used, limit: limit })));
      } else {
        used = fmtCost(u.totalCost); limit = fmtCost(k.spendingLimit);
        span.appendChild(el('span', null, t('key.quotaUsd', { used: used, limit: limit })));
      }
      return span;
    }
    if (k.durationDays != null && !k.activatedAt) {
      span.appendChild(svgIcon('clock', 'meta-ico'));
      span.appendChild(el('span', null, t('key.validityPending', { dur: formatDuration(k.durationDays) })));
      return span;
    }
    if (k.durationDays != null && k.expiresAt) {
      span.appendChild(svgIcon('clock', 'meta-ico'));
      span.appendChild(el('span', null, t('key.expiresWithDur', { date: fmtDateTime(k.expiresAt), dur: formatDuration(k.durationDays) })));
      return span;
    }
    if (k.expiresAt) {
      span.appendChild(svgIcon('clock', 'meta-ico'));
      span.appendChild(el('span', null, t('key.expiresAt', { date: fmtDateTime(k.expiresAt) })));
      return span;
    }
    return span;
  }

  // ---------- actions ----------
  function toggleEnabled(k) {
    api.put('/api-keys/' + k.id, { enabled: k.enabled === false })
      .then(function () { api.toast(t('key.saved'), 'success'); loadKeys(); })
      .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
  }
  function deleteKey(k) {
    confirmModal('key.confirmDeleteName', { name: k.name || serial(k.id) }).then(function (ok) {
      if (!ok) return;
      api.del('/api-keys/' + k.id)
        .then(function () { api.toast(t('key.deleted'), 'success'); loadKeys(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
    });
  }
  function resetUsage(k) {
    confirmModal('key.confirmResetName', { name: k.name || serial(k.id) }).then(function (ok) {
      if (!ok) return;
      api.del('/api-keys/' + k.id + '/usage')
        .then(function () { api.toast(t('key.usageReset'), 'success'); loadKeys(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
    });
  }

  // ---------- purge invalid ----------
  function openPurgeDialog() {
    var inv = invalidKeys();
    if (!inv.length) return;
    var body = el('div');
    body.appendChild(el('p', 'modal-desc', t('key.purgeDesc', { n: inv.length })));
    var listBox = el('div', 'purge-list');
    inv.forEach(function (k) {
      var row = el('div', 'purge-item');
      var left = el('span');
      left.appendChild(el('code', 'key-serial', padId(k.id)));
      left.appendChild(document.createTextNode(' ' + (k.name || '')));
      row.appendChild(left);
      var s = getKeyStatus(k);
      var badge = el('span', 'badge ' + (s === 'disabled' ? 'badge-danger' : 'badge-warning'));
      badge.appendChild(elI18n('span', null, s === 'disabled' ? 'key.statusDisabled' : 'key.statusExpired'));
      row.appendChild(badge);
      listBox.appendChild(row);
    });
    body.appendChild(listBox);

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var ok = el('button', 'btn btn-danger', t('key.purgeConfirm', { n: inv.length })); ok.type = 'button';
    footer.appendChild(cancel); footer.appendChild(ok);

    var m = openModal({ title: t('key.purgeTitle'), bodyEl: body, footerEl: footer });
    cancel.addEventListener('click', function () { m.close(); });
    ok.addEventListener('click', function () {
      ok.disabled = true; ok.textContent = t('key.purging');
      var done = 0;
      var chain = Promise.resolve();
      inv.forEach(function (k) {
        chain = chain.then(function () {
          return api.del('/api-keys/' + k.id).then(function () { done++; }).catch(function () {});
        });
      });
      chain.then(function () {
        m.close();
        api.toast(t('key.purgedN', { n: done }), 'success');
        loadKeys();
      });
    });
  }

  // ---------- per-key detail (usage summary + by-model + request log) ----------
  // 1:1 layout of the reference ApiKeyDetailPage: five summary cards, by-model
  // cards (model name color-coded), and a paged 请求日志 table.
  var DETAIL_PAGE_SIZE = 50;
  function showDetail(k) {
    var body = el('div', 'key-detail');

    // --- summary cards (filled once from /usage) ---
    var strip = el('div', 'usage-strip');
    body.appendChild(strip);

    // --- by-model section (filled once from /usage) ---
    var modelSection = el('div'); modelSection.id = 'keyDetailModels';
    body.appendChild(modelSection);

    // --- request-log header (count + refresh) ---
    var logHead = el('div', 'key-log-head');
    var logTitle = el('h3', 'key-sub-h');
    logTitle.appendChild(elI18n('span', null, 'key.reqLogs'));
    var logCount = el('span', 'key-log-count'); logTitle.appendChild(logCount);
    logHead.appendChild(logTitle);
    var refreshBtn = iconBtn('rotate', 'common.refresh', 'is-mini', function () { loadRecords(page); });
    logHead.appendChild(refreshBtn);
    body.appendChild(logHead);

    // --- records table mount + pager ---
    var tableMount = el('div'); tableMount.id = 'keyDetailTable';
    tableMount.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    body.appendChild(tableMount);
    var pager = el('div', 'accounts-pager'); pager.style.display = 'none';
    body.appendChild(pager);

    openModal({ title: t('key.viewLogs') + ' — ' + serial(k.id) + ' ' + (k.name || ''), bodyEl: body, size: 'lg' });

    var page = 1;
    var totalPages = 1;

    // summary (once)
    api.get('/api-keys/' + k.id + '/usage').then(function (sum) {
      renderSummary(sum || {});
    }).catch(function () { renderSummary({}); });

    loadRecords(1);

    function renderSummary(sum) {
      strip.innerHTML = '';
      var totalCost = Number(sum.totalCost) || 0;
      strip.appendChild(usItem(fmtInt(sum.totalRequests), 'usg.totalRequests'));
      strip.appendChild(usItem(formatTokens(sum.totalInputTokens || sum.inputTokens || 0), 'usg.inputTokens'));
      strip.appendChild(usItem(formatTokens(sum.totalOutputTokens || sum.outputTokens || 0), 'usg.outputTokens'));
      strip.appendChild(usItem(formatCost(totalCost), 'usg.totalCost', 'us-cost'));
      // 总 Credits: totalCredits if present else totalCost/rate; optional "省 X"
      var credits = sum.totalCredits != null ? Number(sum.totalCredits) : totalCost / CREDIT_RATE;
      var saved = Number(sum.totalCreditsSaved) || 0;
      strip.appendChild(usItem(fmtCredits4(credits), 'usg.totalCredits', 'us-credits',
        saved > 0 ? t('usg.creditsSaved', { n: fmtCredits4(saved) }) : null));

      // by-model cards
      modelSection.innerHTML = '';
      var byModel = sum.byModel || sum.models || [];
      if (byModel.length) {
        modelSection.appendChild(elI18n('h3', 'key-sub-h', 'key.byModel'));
        var mg = el('div', 'key-model-grid');
        byModel.forEach(function (m) {
          var c = el('div', 'card key-model-card');
          c.appendChild(el('div', 'key-model-name ' + getModelColor(m.model), m.model || '—'));
          var meta = el('div', 'key-model-meta');
          meta.appendChild(el('span', null, t('key.nRequests', { n: fmtInt(m.requests) })));
          meta.appendChild(el('span', null, t('key.inShort', { n: formatTokens(m.inputTokens) })));
          meta.appendChild(el('span', null, t('key.outShort', { n: formatTokens(m.outputTokens) })));
          meta.appendChild(el('span', 'key-usg-cost', formatCost(m.cost)));
          c.appendChild(meta);
          mg.appendChild(c);
        });
        modelSection.appendChild(mg);
      }
      if (K.i18n) K.i18n.apply(body);
    }

    function loadRecords(p) {
      page = p;
      tableMount.innerHTML = '';
      tableMount.appendChild(elI18n('div', 'empty-state', 'common.loading'));
      if (K.i18n) K.i18n.apply(tableMount);
      api.get('/api-keys/' + k.id + '/usage/records', { page: p, page_size: DETAIL_PAGE_SIZE }).then(function (rd) {
        rd = rd || {};
        var records = rd.records || rd.data || [];
        var total = rd.total != null ? rd.total : records.length;
        totalPages = rd.totalPages != null ? rd.totalPages
          : Math.max(1, Math.ceil(total / DETAIL_PAGE_SIZE));
        logCount.textContent = t('usg.recordsCount', { n: fmtInt(total) });
        tableMount.innerHTML = '';
        tableMount.appendChild(buildRecordsTable(records));
        renderPager();
        if (K.i18n) K.i18n.apply(body);
      }).catch(function () {
        tableMount.innerHTML = '';
        tableMount.appendChild(elI18n('div', 'empty-state', 'common.error'));
        if (K.i18n) K.i18n.apply(tableMount);
      });
    }

    function renderPager() {
      pager.innerHTML = '';
      if (totalPages <= 1) { pager.style.display = 'none'; return; }
      pager.style.display = '';
      var prev = elI18n('button', 'btn btn-outline btn-sm', 'common.prev'); prev.type = 'button';
      prev.disabled = page <= 1;
      prev.addEventListener('click', function () { if (page > 1) loadRecords(page - 1); });
      var ind = el('span', 'accounts-page-indicator', t('usg.pageOf', { cur: page, total: totalPages }));
      var next = elI18n('button', 'btn btn-outline btn-sm', 'common.next'); next.type = 'button';
      next.disabled = page >= totalPages;
      next.addEventListener('click', function () { if (page < totalPages) loadRecords(page + 1); });
      pager.appendChild(prev); pager.appendChild(ind); pager.appendChild(next);
    }
  }
  function usItem(valueText, labelKey, numCls, subText) {
    var it = el('div', 'us-item');
    it.appendChild(el('div', 'us-n' + (numCls ? ' ' + numCls : ''), String(valueText)));
    it.appendChild(elI18n('div', 'us-k', labelKey));
    if (subText) it.appendChild(el('div', 'us-sub', subText));
    return it;
  }
  function buildRecordsTable(records) {
    var wrap = el('div', 'table-container');
    if (!records.length) { wrap.appendChild(elI18n('div', 'empty-state', 'usg.noRecords')); return wrap; }
    var table = el('table', 'data-table');
    var thead = el('thead'); var htr = el('tr');
    ['usg.recTime', 'usg.recClientIp', 'usg.recCredential', 'usg.recModel', 'usg.recTokens', 'usg.recCost', 'usg.recKiroCredits'].forEach(function (kk) {
      htr.appendChild(elI18n('th', null, kk));
    });
    thead.appendChild(htr); table.appendChild(thead);
    var tb = el('tbody');
    records.forEach(function (r) {
      var tr = el('tr');
      // time
      tr.appendChild(el('td', 'rec-time', fmtDateTime(r.createdAt || r.timestamp || r.time)));
      // client IP
      tr.appendChild(el('td', 'rec-ip', r.clientIp || r.ip || '—'));
      // account (credential)
      tr.appendChild(el('td', 'rec-acct', String(r.credentialLabel || r.email || r.credentialId || '—')));
      // model (color-coded)
      tr.appendChild(el('td', 'rec-model ' + getModelColor(r.model), r.model || '—'));
      // Token 用量: 输入 / 输出 / 缓存读取 / 输入总计
      var inTotal = Number(r.inputTokens != null ? r.inputTokens : r.input) || 0;
      var cacheRead = Number(r.cacheReadInputTokens) || 0;
      var out = Number(r.outputTokens != null ? r.outputTokens : r.output) || 0;
      var tokTd = el('td', 'rec-tokens');
      var tokBox = el('div', 'rec-tok-box');
      tokBox.appendChild(tokLine('usg.tokInput', formatTokens(Math.max(0, inTotal - cacheRead))));
      tokBox.appendChild(tokLine('usg.tokOutput', formatTokens(out)));
      tokBox.appendChild(tokLine('usg.tokCacheRead', formatTokens(cacheRead), 'rec-tok-cache'));
      tokBox.appendChild(tokLine('usg.tokInputTotal', formatTokens(inTotal), 'rec-tok-total'));
      tokTd.appendChild(tokBox);
      tr.appendChild(tokTd);
      // cost
      var cost = Number(r.estimatedCost != null ? r.estimatedCost : r.cost) || 0;
      tr.appendChild(el('td', 'rec-cost', formatCost(cost)));
      // Kiro Credits: creditsUsed (✓) else cost/rate; optional (省 X)
      var creditsTd = el('td', 'rec-credits');
      if (r.creditsUsed != null) {
        creditsTd.appendChild(document.createTextNode(fmtCredits4(r.creditsUsed)));
        creditsTd.appendChild(el('span', 'rec-credit-ok', ' ✓'));
      } else {
        creditsTd.appendChild(document.createTextNode(fmtCredits4(cost / CREDIT_RATE)));
      }
      if (r.creditsSaved != null && Number(r.creditsSaved) > 0) {
        creditsTd.appendChild(el('span', 'rec-credit-saved', ' ' + t('usg.savedParen', { n: fmtCredits4(r.creditsSaved) })));
      }
      tr.appendChild(creditsTd);
      tb.appendChild(tr);
    });
    table.appendChild(tb); wrap.appendChild(table);
    return wrap;
  }
  function tokLine(labelKey, valueText, extraCls) {
    var line = el('div', 'rec-tok-line' + (extraCls ? ' ' + extraCls : ''));
    line.appendChild(elI18n('span', 'rec-tok-k', labelKey));
    line.appendChild(el('span', 'rec-tok-v', valueText));
    return line;
  }

  // ---------- create / edit dialog (the reference panel replica) ----------
  function openKeyForm(k) {
    var editing = !!k;

    // ---- form state ----
    var mode; // 'date' | 'quota'
    var duration;      // number or null (null = never expires)
    var durationUnit;  // 'days' | 'hours'
    var spendingLimit;
    var limitUnit;     // 'usd' | 'credits'
    var boundSelected = (k && (k.boundCredentialIds || k.boundCredentials)) ? (k.boundCredentialIds || k.boundCredentials).slice() : [];

    if (editing) {
      if (k.spendingLimit != null) {
        mode = 'quota'; spendingLimit = Number(k.spendingLimit); limitUnit = k.limitUnit || 'usd'; duration = 1; durationUnit = 'days';
      } else {
        mode = 'date'; spendingLimit = 50; limitUnit = 'usd';
        if (k.durationDays != null && k.durationDays < 1) { duration = Math.round(k.durationDays * 24 * 100) / 100; durationUnit = 'hours'; }
        else { duration = k.durationDays != null ? k.durationDays : 1; durationUnit = 'days'; }
      }
    } else {
      mode = 'quota'; duration = 1; durationUnit = 'days'; spendingLimit = 100; limitUnit = 'usd';
    }

    var form = el('div', 'key-form');

    // ---- name / serial ----
    var nameG = el('div', 'form-group');
    nameG.appendChild(elI18n('label', 'form-label', editing ? 'keyForm.nameLabel' : 'keyForm.serialLabel'));
    var nameIn = el('input', 'form-control'); nameIn.type = 'text';
    if (editing) nameIn.value = k.name || '';
    else { nameIn.setAttribute('data-i18n-ph', 'keyForm.serialPh'); nameIn.setAttribute('placeholder', t('keyForm.serialPh')); nameIn.value = generateUniqueSerial(); }
    nameG.appendChild(nameIn);
    var conflictNote = elI18n('div', 'form-error', 'keyForm.serialConflict'); conflictNote.style.display = 'none';
    nameG.appendChild(conflictNote);
    form.appendChild(nameG);

    function existingNumbers() {
      var s = {};
      keys.forEach(function (kk) {
        if (editing && kk.id === k.id) return;
        var trimmed = String(kk.name || '').trim();
        if (/^\d+$/.test(trimmed)) s[parseInt(trimmed, 10)] = true;
      });
      return s;
    }
    function generateUniqueSerial() {
      var ex = existingNumbers();
      for (var i = 0; i < 100; i++) {
        var num = Math.floor(Math.random() * 9999) + 1;
        if (!ex[num]) return ('000' + num).slice(-4);
      }
      var max = 0; Object.keys(ex).forEach(function (n) { if (Number(n) > max) max = Number(n); });
      return ('000' + (max + 1)).slice(-4);
    }
    function checkConflict() {
      var trimmed = nameIn.value.trim();
      var conflict = /^\d+$/.test(trimmed) && existingNumbers()[parseInt(trimmed, 10)];
      conflictNote.style.display = conflict ? '' : 'none';
      return conflict;
    }
    nameIn.addEventListener('input', function () { checkConflict(); updateSaveState(); });

    // ---- mode toggle: 按日期 / 按额度 ----
    var modeG = el('div', 'form-group');
    modeG.appendChild(elI18n('label', 'form-label', 'keyForm.limitMode'));
    var modeSeg = segmented([
      { key: 'date', labelKey: 'keyForm.modeDate', iconName: 'clock' },
      { key: 'quota', labelKey: 'keyForm.modeQuota', iconName: 'dollar' }
    ], mode, function (key, wrap) { mode = key; segSetActive(wrap, key === 'date' ? 0 : 1); renderModeBox(); });
    modeG.appendChild(modeSeg);
    form.appendChild(modeG);

    // ---- mode-specific box ----
    var modeBox = el('div'); modeBox.id = 'keyModeBox';
    form.appendChild(modeBox);

    function renderModeBox() {
      modeBox.innerHTML = '';
      if (mode === 'date') renderDateMode(); else renderQuotaMode();
      if (K.i18n) K.i18n.apply(modeBox);
    }

    function renderDateMode() {
      var g = el('div', 'form-group');
      g.appendChild(elI18n('label', 'form-label', 'keyForm.validity'));
      var chips = el('div', 'chip-row');
      QUICK_DURATIONS.forEach(function (opt) {
        var active = duration === opt.value && durationUnit === opt.unit;
        var b = elI18n('button', 'chip-btn' + (active ? ' is-active' : ''), opt.labelKey); b.type = 'button';
        b.addEventListener('click', function () { duration = opt.value; durationUnit = opt.unit; renderDateMode2(g); });
        chips.appendChild(b);
      });
      var never = elI18n('button', 'chip-btn' + (duration === null ? ' is-active' : ''), 'keyForm.neverExpire'); never.type = 'button';
      never.addEventListener('click', function () { duration = null; renderDateMode2(g); });
      chips.appendChild(never);
      g.appendChild(chips);

      var customWrap = el('div', 'dur-custom'); customWrap.id = 'durCustom';
      g.appendChild(customWrap);

      var hint = el('div', 'form-hint'); hint.id = 'durHint';
      g.appendChild(hint);

      modeBox.appendChild(g);
      renderDurCustom(customWrap);
      renderDurHint(hint);
    }
    // re-render the date-mode group in place after chip clicks
    function renderDateMode2() {
      modeBox.innerHTML = '';
      renderDateMode();
      if (K.i18n) K.i18n.apply(modeBox);
    }
    function renderDurCustom(wrap) {
      wrap.innerHTML = '';
      if (duration === null) return;
      var num = el('input', 'form-control dur-num'); num.type = 'number'; num.min = '1'; num.value = duration;
      num.addEventListener('input', function () { duration = Math.max(1, Number(num.value) || 1); renderDurHint(document.getElementById('durHint')); });
      wrap.appendChild(num);
      var unitSeg = segmented([
        { key: 'hours', labelKey: 'keyForm.unitHours' },
        { key: 'days', labelKey: 'keyForm.unitDays' }
      ], durationUnit, function (key, w) { durationUnit = key; segSetActive(w, key === 'hours' ? 0 : 1); renderDurHint(document.getElementById('durHint')); });
      wrap.appendChild(unitSeg);
      if (K.i18n) K.i18n.apply(wrap);
    }
    function renderDurHint(hint) {
      if (!hint) return;
      hint.innerHTML = '';
      hint.appendChild(svgIcon('clock', 'hint-ico'));
      if (duration === null) hint.appendChild(elI18n('span', null, 'keyForm.neverExpire'));
      else hint.appendChild(el('span', null, t('keyForm.dateHint', { dur: formatDuration(toDays(duration, durationUnit)) })));
    }

    function renderQuotaMode() {
      var g = el('div', 'form-group');
      g.appendChild(elI18n('label', 'form-label', 'keyForm.meteringUnit'));
      var unitSeg = segmented([
        { key: 'usd', labelKey: 'keyForm.unitUsdEst' },
        { key: 'credits', labelKey: 'keyForm.unitCreditsReal' }
      ], limitUnit, function (key, w) { limitUnit = key; segSetActive(w, key === 'usd' ? 0 : 1); renderQuotaMode2(); });
      g.appendChild(unitSeg);

      var lbl = el('label', 'form-label form-label-mt');
      lbl.textContent = limitUnit === 'credits' ? t('keyForm.limitCapCredits') : t('keyForm.limitCapUsd');
      g.appendChild(lbl);

      var chips = el('div', 'chip-row');
      var presets = limitUnit === 'credits' ? [1000, 5000, 10000] : [100, 500, 1000];
      presets.forEach(function (amount) {
        var b = el('button', 'chip-btn' + (spendingLimit === amount ? ' is-active' : '')); b.type = 'button';
        b.textContent = limitUnit === 'credits' ? String(amount) : '$' + amount;
        b.addEventListener('click', function () { spendingLimit = amount; renderQuotaMode2(); });
        chips.appendChild(b);
      });
      g.appendChild(chips);

      var customRow = el('div', 'dur-custom');
      customRow.appendChild(el('span', 'quota-prefix', limitUnit === 'credits' ? t('keyForm.customLabel') : t('keyForm.customLabelUsd')));
      var num = el('input', 'form-control dur-num'); num.type = 'text'; num.inputMode = 'numeric'; num.value = spendingLimit || '';
      num.addEventListener('input', function () {
        var v = num.value.replace(/\D/g, '');
        spendingLimit = v === '' ? 0 : Number(v);
        renderQuotaHint(document.getElementById('quotaHint'));
      });
      customRow.appendChild(num);
      g.appendChild(customRow);

      var hint = el('div', 'form-hint'); hint.id = 'quotaHint';
      g.appendChild(hint);
      modeBox.appendChild(g);
      renderQuotaHint(hint);
    }
    function renderQuotaMode2() {
      modeBox.innerHTML = '';
      renderQuotaMode();
      if (K.i18n) K.i18n.apply(modeBox);
    }
    function renderQuotaHint(hint) {
      if (!hint) return;
      hint.innerHTML = '';
      hint.appendChild(svgIcon('dollar', 'hint-ico'));
      var amt = limitUnit === 'credits' ? t('keyForm.amtCredits', { n: spendingLimit }) : t('keyForm.amtUsd', { n: spendingLimit });
      hint.appendChild(el('span', null, t('keyForm.quotaHint', { amt: amt })));
    }

    // ---- bound accounts multi-select ----
    var boundG = el('div', 'form-group');
    boundG.appendChild(elI18n('label', 'form-label', 'keyForm.boundCreds'));
    boundG.appendChild(elI18n('div', 'form-hint', 'keyForm.boundHintGlobal'));
    var msMount = el('div'); msMount.id = 'keyBoundMs';
    msMount.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    boundG.appendChild(msMount);
    form.appendChild(boundG);

    loadAccountsForBind().then(function (accts) {
      msMount.innerHTML = '';
      msMount.appendChild(buildCredMultiSelect(accts, boundSelected));
    });

    // ---- footer ----
    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var save = el('button', 'btn btn-primary', t(editing ? 'common.save' : 'keyForm.create')); save.type = 'button';
    footer.appendChild(cancel); footer.appendChild(save);

    function updateSaveState() {
      save.disabled = !nameIn.value.trim() || checkConflict();
    }

    var m = openModal({
      title: t(editing ? 'keyForm.editTitle' : 'keyForm.addTitle'),
      descKey: editing ? 'keyForm.editDesc' : 'keyForm.addDesc',
      bodyEl: form, footerEl: footer, size: 'lg'
    });
    renderModeBox();
    updateSaveState();

    cancel.addEventListener('click', function () { m.close(); });
    save.addEventListener('click', function () {
      var name = nameIn.value.trim();
      if (!name || checkConflict()) { updateSaveState(); return; }

      var payload;
      if (editing) {
        payload = { name: name };
        if (mode === 'date') {
          if (duration !== null) {
            payload.durationDays = toDays(duration, durationUnit);
            if (getKeyStatus(k) !== 'active') payload.expiresAt = null;
          } else {
            payload.durationDays = null; payload.expiresAt = null;
          }
          payload.spendingLimit = null;
        } else {
          payload.spendingLimit = spendingLimit; payload.limitUnit = limitUnit;
          payload.expiresAt = null; payload.durationDays = null;
        }
        payload.boundCredentialIds = boundSelected.length ? boundSelected : null;
      } else {
        payload = { name: name };
        if (mode === 'date') {
          if (duration !== null) payload.durationDays = toDays(duration, durationUnit);
        } else {
          payload.spendingLimit = spendingLimit; payload.limitUnit = limitUnit;
        }
        payload.boundCredentialIds = boundSelected.length ? boundSelected : null;
      }

      save.disabled = true;
      var p = editing ? api.put('/api-keys/' + k.id, payload) : api.post('/api-keys', payload);
      p.then(function (res) {
        api.toast(editing ? t('key.saved') : t('key.created'), 'success');
        m.close();
        if (!editing) {
          var newKey = res && (res.key || res.apiKey || (res.data && (res.data.key || res.data.apiKey)));
          if (newKey) showCreatedKey(newKey);
        }
        loadKeys();
      }).catch(function (e) {
        // 失败提示必须用「保存失败」——老写法在编辑分支里复用了成功文案 t('key.saved'),
        // PUT 失败也会弹「API 密钥已保存: <错误>」,而失败路径又不会 loadKeys(),
        // 卡片还显示旧值,看起来跟"保存成功但没改动"一模一样。改额度上限时这等于
        // 让运维以为新的花费上限已经生效,实际还是老的 → 真金白银超支。
        api.toast((editing ? t('keyForm.saveFailed') : t('keyForm.createFailed')) + ': ' + (e.message || t('common.error')), 'error');
        save.disabled = false;
      });
    });
  }

  // ---- credential multi-select (dropdown with checkboxes + search) ----
  function buildCredMultiSelect(accts, selected) {
    var wrap = el('div', 'cred-ms');
    if (!accts || !accts.length) { wrap.appendChild(elI18n('div', 'empty-state', 'common.empty')); return wrap; }

    var open = false;
    var query = '';

    var trigger = el('button', 'cred-ms-trigger'); trigger.type = 'button';
    var tags = el('div', 'cred-ms-tags');
    var chevron = el('span', 'cred-ms-chev', '▾');
    trigger.appendChild(tags); trigger.appendChild(chevron);
    wrap.appendChild(trigger);

    var panel = el('div', 'cred-ms-panel'); panel.style.display = 'none';
    var searchWrap = el('div', 'cred-ms-search');
    searchWrap.appendChild(svgIcon('search', 'key-search-ico'));
    var searchIn = el('input'); searchIn.type = 'text';
    searchIn.setAttribute('data-i18n-ph', 'keyForm.credSearchPh');
    searchIn.setAttribute('placeholder', t('keyForm.credSearchPh'));
    searchWrap.appendChild(searchIn);
    panel.appendChild(searchWrap);
    var optList = el('div', 'cred-ms-options');
    panel.appendChild(optList);
    wrap.appendChild(panel);

    function renderTags() {
      tags.innerHTML = '';
      if (!selected.length) { tags.appendChild(elI18n('span', 'cred-ms-ph', 'keyForm.boundNone')); return; }
      selected.forEach(function (id) {
        var tag = el('span', 'cred-ms-tag');
        tag.appendChild(el('span', null, credLabel(id)));
        var x = el('span', 'cred-ms-x', '✕');
        x.addEventListener('click', function (e) { e.stopPropagation(); toggle(id); });
        tag.appendChild(x);
        tags.appendChild(tag);
      });
    }
    function toggle(id) {
      var idx = selected.map(String).indexOf(String(id));
      if (idx >= 0) selected.splice(idx, 1); else selected.push(id);
      renderTags(); renderOptions();
    }
    function renderOptions() {
      optList.innerHTML = '';
      var q = query.trim().toLowerCase();
      var filtered = accts.filter(function (a) {
        if (!q) return true;
        return String(a.id).indexOf(q) >= 0 || String(a.email || '').toLowerCase().indexOf(q) >= 0;
      });
      if (!filtered.length) { optList.appendChild(elI18n('div', 'cred-ms-empty', 'keyForm.credNoMatch')); return; }
      filtered.forEach(function (a) {
        var isSel = selected.map(String).indexOf(String(a.id)) >= 0;
        var opt = el('button', 'cred-ms-opt' + (isSel ? ' is-sel' : '')); opt.type = 'button';
        var box = el('span', 'cred-ms-check');
        if (isSel) box.appendChild(svgIcon('check', 'check-ico'));
        opt.appendChild(box);
        var meta = el('span', 'cred-ms-meta');
        var top = el('span', 'cred-ms-top');
        top.appendChild(el('span', 'cred-ms-name', a.email || t('keyForm.acctN', { n: a.id })));
        top.appendChild(el('span', 'cred-ms-id', '#' + a.id));
        if (a.disabled) top.appendChild(elI18n('span', 'cred-ms-off', 'common.disabled'));
        meta.appendChild(top);
        opt.appendChild(meta);
        opt.addEventListener('click', function () { toggle(a.id); });
        optList.appendChild(opt);
      });
    }
    searchIn.addEventListener('input', function () { query = searchIn.value; renderOptions(); });
    trigger.addEventListener('click', function (e) {
      e.stopPropagation();
      open = !open;
      panel.style.display = open ? '' : 'none';
      chevron.classList.toggle('is-open', open);
      if (open) { query = ''; searchIn.value = ''; renderOptions(); searchIn.focus(); }
    });
    document.addEventListener('click', function docClose(ev) {
      if (!wrap.contains(ev.target)) { open = false; panel.style.display = 'none'; chevron.classList.remove('is-open'); }
    });

    renderTags();
    renderOptions();
    return wrap;
  }

  // ---------- created-key showcase (copy ONLY the key) ----------
  function showCreatedKey(keyStr) {
    var body = el('div');
    body.appendChild(elI18n('p', 'modal-desc', 'key.createdOnce'));
    var box = el('div', 'created-key-box');
    box.appendChild(elI18n('div', 'form-label', 'key.colKey'));
    box.appendChild(el('div', 'cka-key', keyStr));
    var copyBtn = el('button', 'btn btn-primary btn-sm'); copyBtn.type = 'button';
    copyBtn.appendChild(svgIcon('copy', 'btn-ico'));
    copyBtn.appendChild(elI18n('span', null, 'key.copyKeyOnly'));
    copyBtn.addEventListener('click', function () { copy(keyStr); }); // copies ONLY the key
    box.appendChild(copyBtn);
    body.appendChild(box);

    var footer = el('div', 'modal-footer');
    var done = elI18n('button', 'btn btn-outline', 'common.close'); done.type = 'button';
    footer.appendChild(done);
    var m = openModal({ title: t('key.createdTitle'), bodyEl: body, footerEl: footer });
    done.addEventListener('click', function () { m.close(); });
  }

  // ---------- section registration ----------
  K.sections.apikeys = {
    init: function () { buildShell(); },
    onShow: function () { if (!container) buildShell(); loadKeys(); },
    onHide: function () {}
  };
})();

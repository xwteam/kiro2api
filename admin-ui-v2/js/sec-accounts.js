/* kiro2api Admin UI v2 — sec-accounts.js
   Accounts management section (#sec-accounts).

   Renders a PAGINATED LIST (one account per row, 10 per page) from
   GET /api/admin/credentials and wires all per-account actions
   (enable/disable, priority/weight, reset strikes, balance, usage,
   failure/throttle logs, model refresh, delete) plus the multi-flow
   "Add Account" dropdown (Builder-ID device login, IAM Identity Center SSO,
   SSO token import, and bulk import). Model-refresh surfaces the backend's
   per-account errors[] with reasons.

   Layout mirrors the old React credential-card: a full-width row per account
   with #id, nickname/email, health + auth badges, a stats line (weight /
   success / failure / throttle / RPM / remaining) and an action cluster.
   Client-side pagination: 10 rows/page + 上一页/下一页 + 第 X / Y 页（共 N 个）.

   Public entry: registers K.sections.accounts = { init, onShow, onHide }.
   Dynamic values are set via textContent; static labels via data-i18n so
   K.i18n.apply(sectionEl) fills them — NO hardcoded visible strings. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};
  var api = K.api;

  var PAGE_SIZE = 10;

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
  function fmtDate(s) {
    if (!s) return t('common.never');
    var d = new Date(s);
    return isNaN(d.getTime()) ? String(s) : d.toLocaleString();
  }
  // Format a timestamp that may arrive as a UNIX epoch NUMBER (or numeric string).
  // The balance endpoint returns nextResetAt as unix SECONDS; `new Date(seconds)`
  // treats the value as MILLISECONDS and renders a 1970 date. Detect an
  // epoch-seconds magnitude (< 1e12) and scale to ms; a millis-magnitude number
  // (>= 1e12) is used as-is. Non-numeric values fall through to fmtDate (ISO etc).
  function fmtEpoch(s) {
    if (s == null || s === '') return t('common.never');
    var n = Number(s);
    if (isFinite(n) && n > 0) {
      var ms = n < 1e12 ? n * 1000 : n;
      var d = new Date(ms);
      return isNaN(d.getTime()) ? String(s) : d.toLocaleString();
    }
    return fmtDate(s);
  }
  function fmtInt(n) { n = Number(n); return isFinite(n) ? Math.round(n).toLocaleString() : '0'; }
  function fmtCost(n) { n = Number(n); return isFinite(n) ? n.toFixed(2) : '0.00'; }
  function fmtCredits(n) { n = Number(n); return isFinite(n) ? n.toFixed(1) : '0.0'; }

  // ---------- lucide icons (inline SVG, matching the reference panel's icon set) ----------
  // Paths lifted verbatim from lucide-react so the icon set matches 1:1.
  var LUCIDE = {
    wallet: '<path d="M19 7V4a1 1 0 0 0-1-1H5a2 2 0 0 0 0 4h15a1 1 0 0 1 1 1v4h-3a2 2 0 0 0 0 4h3a1 1 0 0 0 1-1v-2a1 1 0 0 0-1-1"/><path d="M3 5v14a2 2 0 0 0 2 2h15a1 1 0 0 0 1-1v-4"/>',
    fileText: '<path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z"/><path d="M14 2v4a2 2 0 0 0 2 2h4"/><path d="M10 9H8"/><path d="M16 13H8"/><path d="M16 17H8"/>',
    pencil: '<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>',
    refreshCw: '<path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"/><path d="M21 3v5h-5"/><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"/><path d="M8 16H3v5"/>',
    trash: '<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>'
  };
  function svgIcon(name) {
    var svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    svg.setAttribute('aria-hidden', 'true');
    svg.innerHTML = LUCIDE[name] || '';
    return svg;
  }
  // icon action button: <button class="acct-icon-btn ..." title=...><svg/></button>
  function iconBtn(iconName, titleKey, extraCls, onClick) {
    var b = el('button', 'acct-icon-btn' + (extraCls ? ' ' + extraCls : ''));
    b.type = 'button';
    b.setAttribute('data-i18n-title', titleKey);
    b.title = t(titleKey);
    b.appendChild(svgIcon(iconName));
    b.addEventListener('click', function () { onClick(b); });
    return b;
  }
  // CSS switch (icon toggle) for enable/disable — checked = enabled (not disabled)
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
  // opts: { title, bodyEl|bodyHtml, footerEl, size ('lg'), onClose }
  function openModal(opts) {
    var root = document.getElementById('modalRoot') || document.body;
    var overlay = el('div', 'modal-overlay active');
    var modal = el('div', 'modal' + (opts.size === 'lg' ? ' modal-lg' : ''));

    var head = el('div', 'modal-header');
    head.appendChild(el('h3', null, opts.title || ''));
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

  function confirmModal(msgKey) {
    return new Promise(function (resolve) {
      var footer = el('div', 'modal-footer');
      var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
      var ok = elI18n('button', 'btn btn-danger', 'common.confirm'); ok.type = 'button';
      footer.appendChild(cancel); footer.appendChild(ok);
      var body = el('div'); body.appendChild(elI18n('p', null, msgKey));
      var resolved = false;
      var m = openModal({ title: t('common.confirm'), bodyEl: body, footerEl: footer, onClose: function () { if (!resolved) { resolved = true; resolve(false); } } });
      cancel.addEventListener('click', function () { m.close(); });
      ok.addEventListener('click', function () { resolved = true; resolve(true); m.close(); });
    });
  }

  // ---------- state ----------
  var container = null;
  var accounts = [];
  var page = 1;
  var focusId = null; // set when navigated-to from dashboard
  var langBound = false;
  var selectedIds = Object.create(null); // id -> true (selection set for batch actions)
  var balanceMap = Object.create(null);  // id -> balance response (for global-credits sum + per-row remaining)
  // id -> 实时 RPM。GET /credentials 的 CredentialStatusItem **没有** rpm 字段
  // (src/admin/handler.rs account_to_item),每账号 RPM 只在 GET /rpm 的
  // byCredential 里。老写法直接读 acc.rpm,于是满载时每行也恒显示 0,
  // 运维排查流量倾斜会误判成"所有账号都闲着"。
  var rpmByCred = Object.create(null);
  var dailyToday = null;                  // today's usage/daily row (credits + cost)
  var queryingInfo = false;               // guards the "query info" fan-out
  var autoQueryToken = 0;                  // bumps to cancel an in-flight auto-query when the list reloads

  function selectedCount() { return Object.keys(selectedIds).length; }
  function clearSelection() { selectedIds = Object.create(null); }

  // add-menu item: <button><span/><span.sub/></button> that closes the menu on click
  function addMenuItem(row, menu) {
    var b = el('button', null); b.type = 'button';
    b.appendChild(elI18n('span', null, row[0]));
    b.appendChild(elI18n('span', 'sub', row[1]));
    b.addEventListener('click', function () { menu.classList.remove('open'); row[2](); });
    return b;
  }

  // ---------- section shell ----------
  function buildShell() {
    container = document.getElementById('sec-accounts');
    if (!container) return;
    container.innerHTML = '';

    var header = el('div', 'section-header');
    header.appendChild(elI18n('h2', null, 'acc.title'));

    var actions = el('div', 'section-actions');

    // Add-account split dropdown — mirrors the reference "添加账号" DropdownMenu:
    // a "登录 / 授权" group (Builder-ID / IAM Identity Center / SSO Token) then a
    // "手动" group (粘贴 Refresh Token / 批量导入 JSON).
    var addWrap = el('div', 'add-menu-wrap');
    var addBtn = el('button', 'btn btn-primary'); addBtn.type = 'button';
    addBtn.appendChild(document.createTextNode('＋ '));
    addBtn.appendChild(elI18n('span', null, 'acc.add'));
    addBtn.appendChild(document.createTextNode(' ▾'));
    var addMenu = el('div', 'add-menu');
    // group: 登录 / 授权 — mirrors the reference "添加账号" dropdown login group:
    // Builder-ID / IAM Identity Center / SSO Token / Web Cookie / 本地缓存.
    addMenu.appendChild(elI18n('div', 'add-menu-label', 'acc.groupLogin'));
    [
      ['login2.tabBuilder', 'login2.startBuilder', function () { openLoginModal('builderid'); }],
      ['login2.tabIamSso', 'login2.startIamSso', function () { openLoginModal('iam-sso'); }],
      ['login2.tabSsoToken', 'login2.importTokens', function () { openLoginModal('sso-token'); }],
      ['webcookie.title', 'webcookie.sub', function () { openWebCookieModal(); }],
      ['localcache.title', 'localcache.sub', function () { openLocalCacheModal(); }]
    ].forEach(function (row) { addMenu.appendChild(addMenuItem(row, addMenu)); });
    // group: 手动
    addMenu.appendChild(elI18n('div', 'add-menu-label', 'acc.groupManual'));
    [
      ['accForm.pasteToken', 'accForm.addTitle', function () { openAddEditForm(null); }],
      ['acc.batchImportJson', 'imp.paste', function () { openImportModal(); }]
    ].forEach(function (row) { addMenu.appendChild(addMenuItem(row, addMenu)); });
    addBtn.addEventListener('click', function (e) {
      e.stopPropagation();
      addMenu.classList.toggle('open');
    });
    document.addEventListener('click', function () { addMenu.classList.remove('open'); });
    addMenu.addEventListener('click', function (e) { e.stopPropagation(); });
    addWrap.appendChild(addBtn); addWrap.appendChild(addMenu);

    // 查询信息 — fan-out per-account balance (populates 全局积分 + per-row 剩余)
    var queryInfoBtn = elI18n('button', 'btn btn-outline', 'acc.queryInfo'); queryInfoBtn.type = 'button';
    queryInfoBtn.id = 'accountsQueryInfoBtn';
    queryInfoBtn.addEventListener('click', function () { queryAllInfo(queryInfoBtn); });

    // 清除已禁用 — delete every disabled account
    var clearDisabledBtn = elI18n('button', 'btn btn-danger', 'acc.clearDisabled'); clearDisabledBtn.type = 'button';
    clearDisabledBtn.id = 'accountsClearDisabledBtn';
    clearDisabledBtn.addEventListener('click', function () { clearDisabled(clearDisabledBtn); });

    // Kiro Account Manager 导入 — standalone toolbar button (reference: 940-943)
    var kamImportBtn = elI18n('button', 'btn btn-outline', 'acc.kamImport'); kamImportBtn.type = 'button';
    kamImportBtn.addEventListener('click', function () { openImportModal(); });

    // 批量导入 — standalone toolbar button (reference: 944-947)
    var batchImportBtn = elI18n('button', 'btn btn-outline', 'acc.batchImport'); batchImportBtn.type = 'button';
    batchImportBtn.addEventListener('click', function () { openImportModal(); });

    // 刷新 — manual list refresh (reference keeps a refresh action; also re-runs
    // the auto-query). Kept per the requirement to preserve a manual refresh.
    var reloadBtn = elI18n('button', 'btn btn-outline', 'common.refresh'); reloadBtn.type = 'button';
    reloadBtn.addEventListener('click', function () { loadAccounts(); });

    // reference toolbar order: 查询信息 → 清除已禁用 → KAM 导入 → 批量导入 → 添加账号
    actions.appendChild(queryInfoBtn);
    actions.appendChild(clearDisabledBtn);
    actions.appendChild(kamImportBtn);
    actions.appendChild(batchImportBtn);
    actions.appendChild(reloadBtn);
    actions.appendChild(addWrap);
    header.appendChild(actions);
    container.appendChild(header);

    // ---- summary stat cards (账号总数 / 可用账号 / 全局积分 / 今日用量) ----
    var statGrid = el('div', 'accounts-stat-grid'); statGrid.id = 'accountsStatGrid';
    container.appendChild(statGrid);

    // ---- batch-actions bar (only visible when rows are selected) ----
    var batchBar = el('div', 'accounts-batchbar'); batchBar.id = 'accountsBatchBar'; batchBar.style.display = 'none';
    container.appendChild(batchBar);

    var list = el('div', 'accounts-list'); list.id = 'accountsList';
    container.appendChild(list);

    var pager = el('div', 'accounts-pager'); pager.id = 'accountsPager'; pager.style.display = 'none';
    container.appendChild(pager);

    var empty = el('div', 'card'); empty.id = 'accountsEmpty'; empty.style.display = 'none';
    empty.appendChild(elI18n('div', 'empty-state', 'common.empty'));
    container.appendChild(empty);

    if (K.i18n) K.i18n.apply(container);

    // re-render on language change so freshly built rows re-translate
    if (!langBound) {
      langBound = true;
      window.addEventListener('langchange', function () {
        if (container && document.getElementById('accountsList')) { renderList(); renderStats(); updateBatchBar(); }
      });
    }
  }

  // ---------- data load ----------
  function loadAccounts() {
    var list = document.getElementById('accountsList');
    if (list) { list.innerHTML = ''; list.appendChild(elI18n('div', 'empty-state', 'common.loading')); if (K.i18n) K.i18n.apply(list); }
    return api.get('/credentials').then(function (data) {
      accounts = (data && (data.credentials || data.data)) || [];
      // drop selection/balance entries for accounts that no longer exist
      var live = Object.create(null);
      accounts.forEach(function (a) { live[String(a.id)] = true; });
      Object.keys(selectedIds).forEach(function (id) { if (!live[id]) delete selectedIds[id]; });
      Object.keys(balanceMap).forEach(function (id) { if (!live[id]) delete balanceMap[id]; });
      page = 1;
      renderList();
      renderStats();
      updateBatchBar();
      // 每账号 RPM — best-effort,单独一发 /rpm(列表接口不带这个字段)
      loadRpm();
      // today's usage/daily row — best-effort, refreshes the 今日用量 card
      api.get('/usage/daily').then(function (d) {
        var rows = (d && (d.daily || d.days || d.data)) || (Array.isArray(d) ? d : []);
        dailyToday = pickTodayRow(rows);
        renderStats();
      }).catch(function () { /* silent: card falls back to — */ });
      // AUTO-QUERY: mirror the reference dashboard, which auto-fetches every
      // enabled account's balance (剩余 + 全局积分) right after the list loads —
      // the user should not have to click 查询信息. Runs serially (one request
      // at a time) so we don't burst the upstream and risk rate-limits/suspension.
      autoQueryBalances();
    }).catch(function (e) {
      api.toast(e.message || t('common.error'), 'error');
      if (list) { list.innerHTML = ''; list.appendChild(elI18n('div', 'empty-state', 'common.error')); if (K.i18n) K.i18n.apply(list); }
    });
  }

  // 每账号实时 RPM(近 60s):GET /rpm → { global, byCredential, byApiKey }。
  // best-effort:失败就保留上一份快照(首次失败时各行回落 0),不打扰用户 —— 列表
  // 本身已经渲染完了,RPM 只是其中一格。只改在屏行的数字、不整表重绘,免得打断
  // 正在勾选/正在内联改优先级的操作。
  function loadRpm() {
    return api.get('/rpm').then(function (rd) {
      rd = rd || {};
      var by = rd.byCredential || rd.by_credential || {};
      var next = Object.create(null);
      Object.keys(by).forEach(function (k) { next[k] = by[k]; });
      rpmByCred = next;
      paintRpmCells();
    }).catch(function () { /* silent: 各行回落 0 */ });
  }

  // 把最新 RPM 刷进已渲染的行(每行的 RPM 数字格带 data-rpm-cell)。
  function paintRpmCells() {
    var rows = document.querySelectorAll('.account-row[data-acct-id]');
    if (!rows || !rows.length) return;
    Array.prototype.forEach.call(rows, function (row) {
      var id = row.getAttribute('data-acct-id');
      var cell = row.querySelector('.stat-n[data-rpm-cell]');
      if (id != null && cell) cell.textContent = fmtInt(rpmOf(id));
    });
  }

  // ---------- render: paginated list ----------
  function healthClass(h) {
    switch (h) {
      case 'healthy': return 'healthy';
      case 'warning': return 'warning';
      case 'degraded': return 'warning';
      case 'unhealthy': return 'unhealthy';
      default: return 'muted';
    }
  }
  function healthBadge(acc) {
    var h = acc.healthStatus || (acc.disabled ? 'disabled' : 'healthy');
    var map = {
      healthy: ['badge-success', 'dash.healthy'],
      warning: ['badge-warning', 'dash.warning'],
      degraded: ['badge-warning', 'acc.status.degraded'],
      unhealthy: ['badge-danger', 'dash.unhealthy'],
      disabled: ['badge-muted', 'common.disabled']
    };
    var m = map[h] || map.healthy;
    var b = el('span', 'badge ' + m[0]);
    b.appendChild(dot(healthClass(h)));
    b.appendChild(elI18n('span', null, m[1]));
    return b;
  }
  function dot(cls) { return el('span', 'status-dot ' + cls); }

  function authBadge(acc) {
    var isIdc = acc.authMethod === 'idc' || acc.authMethod === 'iam' || acc.authMethod === 'iam-sso';
    var b = el('span', 'badge ' + (isIdc ? 'badge-info' : 'badge-muted'));
    b.appendChild(elI18n('span', null, isIdc ? 'acc.authIdc' : 'acc.authSocial'));
    return b;
  }

  function totalPages() { return Math.max(1, Math.ceil(accounts.length / PAGE_SIZE)); }

  function renderList() {
    var list = document.getElementById('accountsList');
    var pager = document.getElementById('accountsPager');
    var empty = document.getElementById('accountsEmpty');
    if (!list) return;
    list.innerHTML = '';

    if (!accounts.length) {
      list.style.display = 'none';
      if (pager) pager.style.display = 'none';
      if (empty) empty.style.display = '';
      return;
    }
    list.style.display = '';
    if (empty) empty.style.display = 'none';

    var pages = totalPages();
    if (page > pages) page = pages;
    if (page < 1) page = 1;
    var start = (page - 1) * PAGE_SIZE;
    var slice = accounts.slice(start, start + PAGE_SIZE);

    slice.forEach(function (acc) { list.appendChild(buildRow(acc)); });

    renderPager(pager, pages);
    if (K.i18n) K.i18n.apply(list);

    if (focusId != null) {
      // jump to the page that holds the focused account
      var idx = accounts.findIndex(function (a) { return String(a.id) === String(focusId); });
      if (idx >= 0) {
        var wantPage = Math.floor(idx / PAGE_SIZE) + 1;
        if (wantPage !== page) { page = wantPage; renderList(); return; }
        var node = list.querySelector('[data-acct-id="' + cssEscape(String(focusId)) + '"]');
        if (node) { node.scrollIntoView({ behavior: 'smooth', block: 'center' }); node.classList.add('is-current'); }
      }
      focusId = null;
    }
  }
  function cssEscape(s) { return s.replace(/["\\]/g, '\\$&'); }

  function renderPager(pager, pages) {
    if (!pager) return;
    pager.innerHTML = '';
    if (accounts.length <= PAGE_SIZE) { pager.style.display = 'none'; return; }
    pager.style.display = '';

    var prev = elI18n('button', 'btn btn-sm btn-outline', 'common.prev'); prev.type = 'button';
    prev.disabled = page <= 1;
    prev.addEventListener('click', function () { if (page > 1) { page--; renderList(); } });

    var next = elI18n('button', 'btn btn-sm btn-outline', 'common.next'); next.type = 'button';
    next.disabled = page >= pages;
    next.addEventListener('click', function () { if (page < pages) { page++; renderList(); } });

    // "第 X / Y 页（共 N 个）" — interpolated via i18n key.
    // No data-i18n attr here: apply() can't interpolate the {page}/{pages}/{total}
    // tokens, and this pager is rebuilt on every langchange (renderList re-runs),
    // so we set the fully-interpolated string directly.
    var ind = el('span', 'accounts-page-indicator');
    ind.textContent = t('accounts.pageIndicator', { page: page, pages: pages, total: accounts.length });

    pager.appendChild(prev);
    pager.appendChild(ind);
    pager.appendChild(next);
  }

  function buildRow(acc) {
    var row = el('div', 'account-row');
    row.setAttribute('data-acct-id', acc.id);
    if (acc.isCurrent) row.classList.add('is-current');
    if (acc.disabled) row.classList.add('is-disabled');
    if (selectedIds[String(acc.id)]) row.classList.add('is-selected');

    // ----- selection checkbox (drives the batch-actions bar) -----
    var sel = el('label', 'account-row-select');
    var cb = el('input'); cb.type = 'checkbox'; cb.checked = !!selectedIds[String(acc.id)];
    cb.setAttribute('data-i18n-title', 'acc.select');
    cb.title = t('acc.select');
    cb.addEventListener('change', function () {
      if (cb.checked) { selectedIds[String(acc.id)] = true; row.classList.add('is-selected'); }
      else { delete selectedIds[String(acc.id)]; row.classList.remove('is-selected'); }
      updateBatchBar();
    });
    sel.appendChild(cb);
    row.appendChild(sel);

    // ----- left: info -----
    var info = el('div', 'account-row-info');

    // line 1: #id + name + badges
    var idLine = el('div', 'account-row-idline');
    idLine.appendChild(el('code', 'account-row-id', '#' + (acc.id != null ? padId(acc.id) : '?')));
    idLine.appendChild(el('span', 'account-row-name', acc.nickname || acc.email || ('#' + acc.id)));
    idLine.appendChild(healthBadge(acc));
    idLine.appendChild(authBadge(acc));
    if (acc.isCurrent) {
      var cp = el('span', 'current-pill'); cp.appendChild(dot('healthy'));
      cp.appendChild(elI18n('span', null, 'acc.colCurrent'));
      idLine.appendChild(cp);
    }
    if (acc.disabled) {
      idLine.appendChild(elI18n('span', 'badge badge-danger', 'common.disabled'));
    }
    info.appendChild(idLine);

    // line 2: email + last used
    var metaLine = el('div', 'account-row-meta');
    if (acc.email) metaLine.appendChild(el('span', null, acc.email));
    var lu = el('span', null);
    lu.appendChild(elI18n('span', 'meta-k', 'accounts.meta.lastUsed'));
    lu.appendChild(document.createTextNode(' ' + fmtDate(acc.lastUsedAt)));
    metaLine.appendChild(lu);
    var exp = el('span', null);
    exp.appendChild(elI18n('span', 'meta-k', 'acc.colExpires'));
    exp.appendChild(document.createTextNode(' ' + fmtDate(acc.expiresAt)));
    metaLine.appendChild(exp);
    info.appendChild(metaLine);

    // line 3: stats (priority[inline-edit] / failure / success / throttle / rpm / remaining)
    // Matches the reference credential-card: 优先级 first, click-to-edit inline
    // (POST /credentials/{id}/priority), then the counters.
    var statLine = el('div', 'account-row-stats');
    statLine.appendChild(priorityStat(acc));
    statLine.appendChild(stat('acc.colSuccess', fmtInt(acc.successCount), 'ok'));
    var failStat = stat('acc.colFailure', fmtInt(acc.failureCount), Number(acc.failureCount) > 0 ? 'bad' : 'muted');
    failStat.classList.add('is-link');
    failStat.addEventListener('click', function () { showLogs(acc, 'failure'); });
    statLine.appendChild(failStat);
    var thrStat = stat('acc.colThrottle', fmtInt(acc.throttleCount), Number(acc.throttleCount) > 0 ? 'warn' : 'muted');
    thrStat.classList.add('is-link');
    thrStat.addEventListener('click', function () { showLogs(acc, 'throttle'); });
    statLine.appendChild(thrStat);
    // RPM 只能来自 /rpm 的 byCredential 快照(见 rpmByCred 的注释);拿不到时才回落 0。
    var rpmStat = stat('accounts.col.rpm', fmtInt(rpmOf(acc.id)), 'info');
    rpmStat.querySelector('.stat-n').dataset.rpmCell = '1';
    statLine.appendChild(rpmStat);
    var cachedBal = balanceMap[String(acc.id)];
    var balStat = stat('acc.colRemaining',
      (cachedBal && cachedBal.remaining != null) ? fmtCredits(cachedBal.remaining) : t('common.unset'),
      'muted');
    balStat.querySelector('.stat-n').dataset.balCell = '1';
    statLine.appendChild(balStat);
    info.appendChild(statLine);

    row.appendChild(info);

    // ----- right: actions (icon toggle switch + icon action buttons) -----
    // 1:1 with the reference panel credential-card action cluster (order + set):
    // Switch(enable/disable) → Wallet(balance) → FileText(usage) →
    // Pencil(edit) → RefreshCw(reset, disabled at 0 failures) → Trash2(delete,
    // disabled until the account is disabled). Balance and usage are SEPARATE
    // dialogs. No per-row model-refresh — the reference card has none; bulk
    // model-refresh wiring is preserved elsewhere.
    var actions = el('div', 'account-row-actions');

    // enable/disable = CSS icon toggle switch (checked === enabled)
    actions.appendChild(switchToggle(!acc.disabled,
      acc.disabled ? 'acc.enable' : 'acc.disable',
      function () { toggleDisabled(acc); }));

    actions.appendChild(iconBtn('wallet', 'acc.balance', null, function () {
      showBalance(acc, balStat.querySelector('.stat-n'));
    }));
    // 用量 — the FileText button opens the account's USAGE dialog (a distinct
    // popup from balance). Failure/throttle logs remain reachable via the
    // clickable 失败/限流 stats in the stat line above.
    actions.appendChild(iconBtn('fileText', 'acc.viewUsage', null, function () { showUsage(acc); }));
    actions.appendChild(iconBtn('pencil', 'common.edit', null, function () { openAddEditForm(acc); }));
    var resetBtn = iconBtn('refreshCw', 'acc.reset', null, function () { resetStrikes(acc); });
    if (!(Number(acc.failureCount) > 0)) resetBtn.disabled = true; // the reference panel disables reset at 0 failures
    actions.appendChild(resetBtn);
    var delBtn = iconBtn('trash', acc.disabled ? 'common.delete' : 'acc.deleteNeedDisable', 'is-danger', function () { deleteAccount(acc); });
    if (!acc.disabled) delBtn.disabled = true; // the reference panel requires disable-before-delete
    actions.appendChild(delBtn);
    row.appendChild(actions);

    return row;
  }

  function padId(id) { var s = String(id); return s.length >= 3 ? s : ('000' + s).slice(-3); }

  // 单账号 RPM:只认 /rpm 快照里的值(id 在 byCredential 里是字符串键)。
  function rpmOf(id) {
    var v = rpmByCred[String(id)];
    return v != null ? v : 0;
  }

  function stat(labelKey, valueText, kind) {
    var s = el('div', 'account-stat ' + (kind || 'neutral'));
    s.appendChild(el('span', 'stat-n', valueText));
    s.appendChild(elI18n('span', 'stat-k', labelKey));
    return s;
  }

  // Inline-editable 优先级 stat — mirrors the reference credential-card's
  // click-to-edit priority (数字越小越优先). Commits via POST
  // /credentials/{id}/priority; Enter saves, Escape cancels.
  function priorityStat(acc) {
    var current = acc.priority != null ? acc.priority : (acc.weight != null ? acc.weight : 0);
    var s = el('div', 'account-stat neutral is-link');
    var nSpan = el('span', 'stat-n', String(current));
    s.appendChild(nSpan);
    s.appendChild(elI18n('span', 'stat-k', 'acc.colPriority'));
    s.setAttribute('data-i18n-title', 'acc.editPriority');
    s.title = t('acc.editPriority');

    function commit(inp) {
      var v = parseInt(inp.value, 10);
      if (isNaN(v) || v < 0) { api.toast(t('acc.priorityInvalid'), 'error'); return; }
      api.post('/credentials/' + acc.id + '/priority', { priority: v })
        .then(function () {
          acc.priority = v;
          nSpan.textContent = String(v);
          api.toast(t('acc.saved'), 'success');
        })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); })
        .then(function () { restore(); });
    }
    function restore() {
      s.classList.add('is-link');
      s.innerHTML = '';
      s.appendChild(nSpan);
      s.appendChild(elI18n('span', 'stat-k', 'acc.colPriority'));
      if (K.i18n) K.i18n.apply(s);
    }
    s.addEventListener('click', function () {
      if (s.querySelector('input')) return;
      s.classList.remove('is-link');
      s.innerHTML = '';
      var inp = el('input', 'acct-inline-input'); inp.type = 'number'; inp.min = '0';
      inp.value = String(acc.priority != null ? acc.priority : (acc.weight != null ? acc.weight : 0));
      s.appendChild(inp);
      inp.focus(); inp.select();
      inp.addEventListener('keydown', function (e) {
        if (e.key === 'Enter') { e.preventDefault(); commit(inp); }
        else if (e.key === 'Escape') { e.preventDefault(); restore(); }
      });
      inp.addEventListener('blur', function () { restore(); });
    });
    return s;
  }

  // ---------- top section: summary stat cards ----------
  // Picks the row for "today" (CST/UTC+8 day key) out of the usage/daily list,
  // matching how the backend buckets daily rollups.
  function cstTodayKey() {
    var cst = new Date(Date.now() + 8 * 3600 * 1000);
    return cst.getUTCFullYear() + '-' +
      String(cst.getUTCMonth() + 1).padStart(2, '0') + '-' +
      String(cst.getUTCDate()).padStart(2, '0');
  }
  function pickTodayRow(rows) {
    if (!Array.isArray(rows)) return null;
    var key = cstTodayKey();
    for (var i = 0; i < rows.length; i++) {
      if (rows[i] && String(rows[i].date) === key) return rows[i];
    }
    return null;
  }

  function statCard(labelKey, valueText, kind, onClick) {
    var c = el('div', 'card acct-stat-card' + (kind ? ' ' + kind : '') + (onClick ? ' is-clickable' : ''));
    c.appendChild(elI18n('div', 'acct-stat-label', labelKey));
    c.appendChild(el('div', 'acct-stat-num', valueText));
    if (onClick) c.addEventListener('click', onClick);
    return c;
  }

  function renderStats() {
    var grid = document.getElementById('accountsStatGrid');
    if (!grid) return;
    grid.innerHTML = '';

    var total = accounts.length;
    var available = accounts.filter(function (a) { return !a.disabled; }).length;

    // 账号总数
    grid.appendChild(statCard('acc.statTotal', fmtInt(total), 'neutral'));
    // 可用账号
    grid.appendChild(statCard('acc.statAvailable', fmtInt(available), 'ok'));

    // 全局积分 — sum of fetched balances; shows "已查询 X/N" progress
    var queried = Object.keys(balanceMap).length;
    var sum = 0;
    Object.keys(balanceMap).forEach(function (id) {
      var b = balanceMap[id];
      if (b && typeof b.remaining === 'number') sum += b.remaining;
    });
    var creditsCard = statCard('acc.statCredits', queried > 0 ? fmtCredits(sum) : '—', 'credits');
    var sub = el('div', 'acct-stat-sub');
    sub.textContent = t('acc.creditsQueried', { queried: queried, total: total });
    creditsCard.appendChild(sub);
    if (queryingInfo && total > 0) {
      var meter = el('div', 'acct-stat-meter');
      var bar = el('span'); bar.style.width = Math.min(100, (queried / total) * 100) + '%';
      meter.appendChild(bar); creditsCard.appendChild(meter);
    }
    grid.appendChild(creditsCard);

    // 今日用量 — credits + cost, clicking jumps to the Usage Stats page
    var usageCard = statCard('acc.statTodayUsage',
      dailyToday ? (fmtCredits(dailyToday.totalCredits) + ' ' + t('usg.recCredits')) : '—',
      'neutral',
      function () { if (K.showSection) K.showSection('usage'); });
    if (dailyToday) {
      var cost = el('div', 'acct-stat-cost');
      cost.textContent = '$' + fmtCost(dailyToday.totalCost);
      usageCard.appendChild(cost);
    }
    grid.appendChild(usageCard);

    if (K.i18n) K.i18n.apply(grid);
  }

  // ---------- top section: batch-actions bar ----------
  function updateBatchBar() {
    var bar = document.getElementById('accountsBatchBar');
    if (!bar) return;
    var n = selectedCount();
    if (n === 0) { bar.style.display = 'none'; bar.innerHTML = ''; return; }
    bar.style.display = '';
    bar.innerHTML = '';

    var count = el('span', 'bb-count');
    count.textContent = t('acc.selectedN', { n: n });
    bar.appendChild(count);

    var deselect = elI18n('button', 'btn btn-sm btn-outline', 'acc.deselectAll'); deselect.type = 'button';
    deselect.addEventListener('click', function () { clearSelection(); renderList(); updateBatchBar(); });
    bar.appendChild(deselect);

    bar.appendChild(el('span', 'bb-spacer'));

    var verifyBtn = elI18n('button', 'btn btn-sm btn-outline', 'acc.batchVerify'); verifyBtn.type = 'button';
    verifyBtn.addEventListener('click', function () { batchVerify(verifyBtn); });
    bar.appendChild(verifyBtn);

    var resetBtn = elI18n('button', 'btn btn-sm btn-outline', 'acc.batchReset'); resetBtn.type = 'button';
    resetBtn.addEventListener('click', function () { batchReset(resetBtn); });
    bar.appendChild(resetBtn);

    var delBtn = elI18n('button', 'btn btn-sm btn-danger', 'acc.batchDelete'); delBtn.type = 'button';
    delBtn.addEventListener('click', function () { batchDelete(delBtn); });
    bar.appendChild(delBtn);

    if (K.i18n) K.i18n.apply(bar);
  }

  function selectedAccounts() {
    return accounts.filter(function (a) { return selectedIds[String(a.id)]; });
  }

  // ---------- top-section actions ----------
  // Core serial balance fan-out shared by the manual 查询信息 button and the
  // automatic on-load query. Fetches each enabled account's balance one request
  // at a time (never bursts the upstream), updating the per-row 剩余 cell and the
  // 全局积分 card as results arrive. `opts`:
  //   { btn, toast (bool), token (cancellation token — 手动/自动两条路都要带), onDone }
  // 两条硬性约束(都出过线上事故,别删):
  //   ① 串行:一次只发一发,绝不对上游成批爆发;
  //   ② 撞 401/403 立刻整轮停,详见下面 catch 里的注释。
  function runBalanceFanout(opts) {
    opts = opts || {};
    var enabled = accounts.filter(function (a) { return !a.disabled; });
    if (!enabled.length) {
      // 一个可查的账号都没有也要解禁按钮:调用方(queryAllInfo)点下去的那一刻就
      // 先把按钮禁掉了,这里不放开就永远灰着。
      if (opts.btn) opts.btn.disabled = false;
      if (opts.toast) api.toast(t('common.empty'), 'info');
      if (opts.onDone) opts.onDone();
      return;
    }
    queryingInfo = true;
    if (opts.btn) opts.btn.disabled = true;
    balanceMap = Object.create(null);
    renderStats();

    var i = 0, ok = 0, fail = 0;

    // 本轮是否已被作废:令牌被顶掉(列表重载 / 离开本页 / 又点了一次查询)。
    function cancelled() { return opts.token != null && opts.token !== autoQueryToken; }

    // 收工/中止的统一出口。**一定**要同时放掉 queryingInfo 并解禁按钮:
    // 老写法的两个取消分支只清了 queryingInfo,手动那一轮被顶掉时「查询信息」
    // 按钮会永远灰着。
    function stop() {
      queryingInfo = false;
      if (opts.btn) opts.btn.disabled = false;
    }

    function next() {
      // cancelled (list reloaded / left the page) — bail without touching UI state again
      if (cancelled()) { stop(); return; }
      if (i >= enabled.length) {
        stop();
        renderStats();
        if (opts.toast) api.toast(t('imp.result', { added: ok, total: enabled.length, failed: fail }), fail ? 'info' : 'success');
        if (opts.onDone) opts.onDone();
        return;
      }
      var acc = enabled[i++];
      var authFailed = false;
      api.get('/credentials/' + acc.id + '/balance').then(function (b) {
        if (b && b.remaining != null) { balanceMap[String(acc.id)] = b; ok++; }
        else fail++;
      }).catch(function (e) {
        fail++;
        // 401/403 = admin key 已经失效(改了 ADMIN key 重启容器、别处轮换了钥匙、
        // 会话过期……)。api.js 收到它已经清掉本地钥匙并弹出登录浮层了。
        //
        // 这时**绝不能**照旧 next() 把剩下的账号发完:池里有上千个,它们会在几秒内
        // 全部 401,而每一个 401 都会再触发一次 onUnauthorized —— 后者会清空登录
        // 输入框并重新抢占焦点。结果就是运维眼睁睁看着输入框被反复清空、一个字都
        // 打不进去,自己把自己锁在后台外面。所以撞到鉴权失败就整轮停。
        if (e && (e.status === 401 || e.status === 403)) authFailed = true;
      }).then(function () {
        // 顺带把令牌自增,让同时可能在飞的另一轮(手动/自动)也一起停下。
        if (authFailed) { autoQueryToken++; stop(); return; }
        if (cancelled()) { stop(); return; }
        // update the matching row's remaining cell if it is on-screen
        var node = document.querySelector('.account-row[data-acct-id="' + cssEscape(String(acc.id)) + '"] .stat-n[data-bal-cell]');
        var b = balanceMap[String(acc.id)];
        if (node && b && b.remaining != null) node.textContent = fmtCredits(b.remaining);
        renderStats();
        next();
      });
    }
    next();
  }

  // 查询信息 — manual refresh of all enabled-account balances (toasts a summary).
  //
  // 老写法开头是 `if (queryingInfo) return;`:进页面就会自动跑一轮扇出,上千个账号
  // 串行要好几分钟,这几分钟里点「查询信息」直接静默返回 —— 不弹提示、按钮也不禁用
  // (自动那轮没带 btn),看上去就是按钮坏了,运维只会一直点。现在改成**真的顶掉**
  // 在飞的那一轮:先自增令牌作废它,再等它自己收工(它要等手上那一发请求落地才会
  // 看到令牌变了),然后开新一轮。按钮从点击那一刻就禁用,用户立刻看得到反馈。
  function queryAllInfo(btn) {
    var token = ++autoQueryToken; // 作废在飞的自动/手动轮
    if (btn) btn.disabled = true;
    whenFanoutIdle(function () {
      // 等待期间又被顶掉(离开页面 / 列表重载 / 再点一次)→ 本轮作废,按钮交还。
      if (token !== autoQueryToken) { if (btn) btn.disabled = false; return; }
      runBalanceFanout({ btn: btn, toast: true, token: token });
    });
  }

  // 等到没有扇出在跑再执行 fn。被作废的那一轮最多再跑完手上这一发请求就会 stop(),
  // 所以这里只是短暂等待,不会真的卡到整轮结束。
  function whenFanoutIdle(fn) {
    if (!queryingInfo) { fn(); return; }
    setTimeout(function () { whenFanoutIdle(fn); }, 120);
  }

  // AUTO-QUERY on section load: silently populate balances so the user never has
  // to click 查询信息. Serial + cancellable (a fresh loadAccounts bumps the token).
  function autoQueryBalances() {
    if (queryingInfo) return; // a manual/auto pass is already running
    var token = ++autoQueryToken;
    runBalanceFanout({ token: token, toast: false });
  }

  // 清除已禁用 — delete every disabled account (with confirm)
  function clearDisabled(btn) {
    var disabled = accounts.filter(function (a) { return a.disabled; });
    if (!disabled.length) { api.toast(t('acc.noDisabled'), 'info'); return; }
    confirmModal('acc.confirmClearDisabled').then(function (yes) {
      if (!yes) return;
      if (btn) btn.disabled = true;
      var ok = 0, fail = 0;
      Promise.allSettled(disabled.map(function (a) {
        return api.del('/credentials/' + a.id).then(function () { ok++; }, function () { fail++; });
      })).then(function () {
        if (btn) btn.disabled = false;
        api.toast(t('imp.result', { added: ok, total: disabled.length, failed: fail }), fail ? 'info' : 'success');
        loadAccounts();
      });
    });
  }

  // 批量验活 — sequentially poke each selected account's balance (2s spacing to
  // avoid tripping anti-abuse), report success/fail counts.
  function batchVerify(btn) {
    var sel = selectedAccounts();
    if (!sel.length) return;
    if (btn) btn.disabled = true;
    var i = 0, ok = 0, fail = 0;
    function step() {
      if (i >= sel.length) {
        if (btn) btn.disabled = false;
        renderStats();
        api.toast(t('imp.result', { added: ok, total: sel.length, failed: fail }), fail ? 'info' : 'success');
        return;
      }
      var acc = sel[i++];
      api.get('/credentials/' + acc.id + '/balance').then(function (b) {
        if (b && b.remaining != null) { balanceMap[String(acc.id)] = b; ok++; } else fail++;
      }).catch(function () { fail++; }).then(function () {
        var node = document.querySelector('.account-row[data-acct-id="' + cssEscape(String(acc.id)) + '"] .stat-n[data-bal-cell]');
        var b = balanceMap[String(acc.id)];
        if (node && b && b.remaining != null) node.textContent = fmtCredits(b.remaining);
        if (i < sel.length) setTimeout(step, 2000); else step();
      });
    }
    step();
  }

  // 恢复异常 — reset strikes on every selected account that has failures
  function batchReset(btn) {
    var targets = selectedAccounts().filter(function (a) { return Number(a.failureCount) > 0; });
    if (!targets.length) { api.toast(t('acc.noFailures'), 'info'); return; }
    if (btn) btn.disabled = true;
    var ok = 0, fail = 0;
    Promise.allSettled(targets.map(function (a) {
      return api.post('/credentials/' + a.id + '/reset', {}).then(function () { ok++; }, function () { fail++; });
    })).then(function () {
      if (btn) btn.disabled = false;
      api.toast(t('imp.result', { added: ok, total: targets.length, failed: fail }), fail ? 'info' : 'success');
      loadAccounts();
    });
  }

  // 批量删除 — delete selected accounts (disabled-only, matching the reference:
  // non-disabled selections are skipped, never deleted).
  function batchDelete(btn) {
    var sel = selectedAccounts();
    var deletable = sel.filter(function (a) { return a.disabled; });
    if (!deletable.length) { api.toast(t('acc.batchDeleteNeedDisable'), 'error'); return; }
    confirmModal('acc.confirmBatchDelete').then(function (yes) {
      if (!yes) return;
      if (btn) btn.disabled = true;
      var ok = 0, fail = 0;
      Promise.allSettled(deletable.map(function (a) {
        return api.del('/credentials/' + a.id).then(function () { ok++; }, function () { fail++; });
      })).then(function () {
        if (btn) btn.disabled = false;
        api.toast(t('imp.result', { added: ok, total: deletable.length, failed: fail }), fail ? 'info' : 'success');
        loadAccounts();
      });
    });
  }

  // ---------- actions: toggle / reset / delete ----------
  function toggleDisabled(acc) {
    api.post('/credentials/' + acc.id + '/disabled', { disabled: !acc.disabled })
      .then(function () { api.toast(t('acc.saved'), 'success'); loadAccounts(); })
      .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
  }
  function resetStrikes(acc) {
    api.post('/credentials/' + acc.id + '/reset', {})
      .then(function () { api.toast(t('acc.resetDone'), 'success'); loadAccounts(); })
      .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
  }
  function deleteAccount(acc) {
    if (!acc.disabled) { api.toast(t('acc.deleteNeedDisable'), 'error'); return; } // the reference panel: disable first
    confirmModal('acc.confirmDelete').then(function (ok) {
      if (!ok) return;
      api.del('/credentials/' + acc.id)
        .then(function () { api.toast(t('acc.deleted'), 'success'); loadAccounts(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); });
    });
  }

  // ---------- balance (standalone dialog) ----------
  // The 余额 (wallet) button opens balance ONLY: subscription / current usage /
  // limit / remaining / used-% meter / next-reset. Usage is a SEPARATE dialog
  // (showUsage) — the two are intentionally distinct popups.
  function showBalance(acc, cellEl) {
    var body = el('div');
    body.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    openModal({ title: t('bal.title') + ' — ' + (acc.email || acc.nickname || ('#' + acc.id)), bodyEl: body, size: 'lg' });
    api.get('/credentials/' + acc.id + '/balance').then(function (b) {
      b = b || {};
      body.innerHTML = '';
      var pct = Number(b.usagePercentage != null ? b.usagePercentage : 0);
      var strip = el('div', 'usage-strip');
      strip.appendChild(usItem(b.subscriptionTitle || b.subscription || t('common.unset'), 'bal.subscription'));
      strip.appendChild(usItem(fmtCredits(b.currentUsage), 'bal.usage'));
      strip.appendChild(usItem(b.usageLimit != null ? fmtCredits(b.usageLimit) : t('common.unset'), 'bal.limit'));
      strip.appendChild(usItem(b.remaining != null ? fmtCredits(b.remaining) : t('common.unset'), 'bal.remaining'));
      body.appendChild(strip);

      var pctLabel = el('div', 'account-info-label');
      pctLabel.appendChild(elI18n('span', null, 'bal.percent'));
      pctLabel.appendChild(document.createTextNode(' — ' + pct.toFixed(1) + '%'));
      body.appendChild(pctLabel);
      var meter = el('div', 'meter' + (pct >= 90 ? ' hot' : pct >= 70 ? ' warm' : ''));
      var bar = el('span'); bar.style.width = Math.max(0, Math.min(100, pct)) + '%';
      meter.appendChild(bar); body.appendChild(meter);

      var resetRow = el('div', 'account-info-row');
      resetRow.appendChild(elI18n('span', 'account-info-label', 'bal.nextReset'));
      // nextResetAt is unix SECONDS — format via fmtEpoch (fmtDate would show 1970).
      resetRow.appendChild(el('span', 'account-info-value', fmtEpoch(b.nextResetAt)));
      body.appendChild(resetRow);

      // update the row's remaining cell + cache for the 全局积分 card
      if (cellEl && b.remaining != null) cellEl.textContent = fmtCredits(b.remaining);
      if (b.remaining != null) { balanceMap[String(acc.id)] = b; renderStats(); }

      if (K.i18n) K.i18n.apply(body);
    }).catch(function (e) {
      body.innerHTML = '';
      body.appendChild(el('div', 'empty-state', (e && e.message) || t('common.error')));
    });
  }

  // ---------- usage (standalone dialog) ----------
  // Account usage detail dialog. Layout:
  //   1) TOP SUMMARY CARDS (5) — 总请求数 / 本页 Tokens(入/出) / 本页费用 /
  //      本页 Credits(+省) / 今日 Credits(+省 +今日 N 请求)
  //   2) BY-MODEL BREAKDOWN — current page grouped by model (model colored,
  //      入/出 tokens, N 次, cost)
  //   3) RAW RECORDS TABLE — 时间 / IP / 账号 / 模型 / Token 用量 / 费用 /
  //      Kiro Credits, with 上一页/下一页 pagination
  //
  // Data:
  //   GET /credentials/{id}/usage/records?page=&page_size=50 →
  //     { records:[{ model, inputTokens, outputTokens, estimatedCost,
  //       creditsUsed?, creditsSaved?, cacheReadInputTokens?, createdAt,
  //       credentialLabel?, clientIp? }], total, page, pageSize, totalPages }
  //   GET /credentials/{id}/usage/today → { totalCredits, totalCreditsSaved?,
  //       totalRequests, ... }
  var USAGE_PAGE_SIZE = 50; // records-per-page for the usage detail dialog

  // model → color class (mirrors MODEL_COLORS / getModelColor in the tsx)
  function usageModelColor(model) {
    var lower = String(model || '').toLowerCase();
    if (lower.indexOf('opus') >= 0) return 'model-opus';
    if (lower.indexOf('sonnet') >= 0) return 'model-sonnet';
    if (lower.indexOf('haiku') >= 0) return 'model-haiku';
    return 'model-muted';
  }
  // formatTokens — tsx: n.toLocaleString('zh-CN')
  function formatTokens(n) { n = Number(n); return isFinite(n) ? n.toLocaleString('zh-CN') : '0'; }
  // formatCost — tsx: `$${cost.toFixed(4)}`
  function formatCost(n) { n = Number(n); return '$' + (isFinite(n) ? n : 0).toFixed(4); }
  // formatDate — tsx: toLocaleString('zh-CN', {y m d h m s})
  function formatUsageDate(s) {
    if (!s) return '—';
    var d = new Date(s);
    if (isNaN(d.getTime())) return String(s);
    return d.toLocaleString('zh-CN', {
      year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', second: '2-digit'
    });
  }

  function showUsage(acc) {
    var body = el('div');
    body.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    openModal({ title: t('acc.viewUsage') + ' — ' + (acc.email || acc.nickname || ('#' + acc.id)), bodyEl: body, size: 'lg' });

    var page = 1;
    var todaySummary = {};

    // fetch today's summary once (mirrors useCredentialTodaySummary)
    api.get('/credentials/' + acc.id + '/usage/today')
      .then(function (d) { todaySummary = d || {}; })
      .catch(function () { todaySummary = {}; })
      .then(function () { loadPage(); });

    function loadPage() {
      body.innerHTML = '';
      body.appendChild(elI18n('div', 'empty-state', 'common.loading'));
      api.get('/credentials/' + acc.id + '/usage/records', { page: page, page_size: USAGE_PAGE_SIZE })
        .then(function (recData) { render(recData || {}); })
        .catch(function (e) {
          body.innerHTML = '';
          body.appendChild(el('div', 'empty-state', (e && e.message) || t('common.error')));
        });
    }

    function render(recData) {
      body.innerHTML = '';
      var records = recData.records || [];
      var totalRequests = recData.total != null ? recData.total : 0;
      var totalPages = recData.totalPages != null ? recData.totalPages : 1;

      // ===== 1) TOP SUMMARY CARDS (5) =====
      var sumIn = 0, sumOut = 0, sumCost = 0, sumCredits = 0, sumSaved = 0;
      records.forEach(function (r) {
        sumIn += Number(r.inputTokens) || 0;
        sumOut += Number(r.outputTokens) || 0;
        sumCost += Number(r.estimatedCost) || 0;
        sumCredits += (r.creditsUsed != null ? Number(r.creditsUsed) : (Number(r.estimatedCost) || 0) / 0.72);
        sumSaved += Number(r.creditsSaved) || 0;
      });

      var strip = el('div', 'usage-strip');
      // 总请求数 (recordsData.total)
      strip.appendChild(usItem(fmtInt(totalRequests), 'usg.totalRequests'));
      // 本页 Tokens — 入 X / 出 Y (formatTokens)
      strip.appendChild(usItem('入 ' + formatTokens(sumIn) + ' / 出 ' + formatTokens(sumOut), 'usg.pageTokens'));
      // 本页费用 ($sum.toFixed(4))
      strip.appendChild(usItem(formatCost(sumCost), 'usg.pageCost', 'us-cost'));
      // 本页 Credits (sum creditsUsed ?? cost/0.72; + 省 X if saved>0)
      strip.appendChild(usItem(
        sumCredits.toFixed(4), 'usg.pageCredits', 'us-credits',
        sumSaved > 0 ? t('usg.saved', { n: sumSaved.toFixed(4) }) : null));
      // 今日 Credits (todaySummary.totalCredits.toFixed(4); + 省 X; + 今日 N 请求)
      var todayCredits = Number(todaySummary.totalCredits) || 0;
      var todaySaved = Number(todaySummary.totalCreditsSaved) || 0;
      var todayReq = Number(todaySummary.totalRequests) || 0;
      var todaySubs = [];
      if (todaySaved > 0) todaySubs.push(t('usg.saved', { n: todaySaved.toFixed(4) }));
      if (todayReq > 0) todaySubs.push(t('usg.todayReqN', { n: todayReq }));
      strip.appendChild(usItem(todayCredits.toFixed(4), 'usg.todayCredits', 'us-today', todaySubs));
      body.appendChild(strip);

      // ===== 2) BY-MODEL BREAKDOWN (current page) =====
      var byModel = {};
      records.forEach(function (r) {
        var m = byModel[r.model] || { requests: 0, inputTokens: 0, outputTokens: 0, cost: 0 };
        m.requests += 1;
        m.inputTokens += Number(r.inputTokens) || 0;
        m.outputTokens += Number(r.outputTokens) || 0;
        m.cost += Number(r.estimatedCost) || 0;
        byModel[r.model] = m;
      });
      var modelKeys = Object.keys(byModel);
      if (modelKeys.length) {
        body.appendChild(elI18n('div', 'mng-section-title', 'usg.byModelPage'));
        var grid = el('div', 'usage-bymodel-grid');
        modelKeys.forEach(function (model) {
          var m = byModel[model];
          var card = el('div', 'usage-bymodel-card');
          card.appendChild(el('div', 'ubm-model ' + usageModelColor(model), model));
          var meta = el('div', 'ubm-meta');
          meta.appendChild(el('span', null, t('usg.reqCount', { n: m.requests })));
          meta.appendChild(el('span', null, '入 ' + formatTokens(m.inputTokens)));
          meta.appendChild(el('span', null, '出 ' + formatTokens(m.outputTokens)));
          meta.appendChild(el('span', 'ubm-cost', formatCost(m.cost)));
          card.appendChild(meta);
          grid.appendChild(card);
        });
        body.appendChild(grid);
      }

      // ===== 3) RAW RECORDS TABLE + pagination =====
      var logHead = el('div', 'usage-log-head');
      var logTitle = el('div', 'mng-section-title');
      logTitle.appendChild(elI18n('span', null, 'usg.requestLog'));
      var cnt = el('span', 'usage-log-count');
      cnt.textContent = t('usg.recordCount', { total: totalRequests });
      logTitle.appendChild(cnt);
      logHead.appendChild(logTitle);
      body.appendChild(logHead);

      body.appendChild(buildRecordsTable(records));

      // pagination — only when totalPages > 1 (mirrors the tsx)
      if (totalPages > 1) {
        var pager = el('div', 'usage-pager');
        var prev = elI18n('button', 'btn btn-sm btn-outline', 'common.prev'); prev.type = 'button';
        prev.disabled = page <= 1;
        prev.addEventListener('click', function () { if (page > 1) { page--; loadPage(); } });
        var ind = el('span', 'usage-page-indicator');
        ind.textContent = t('usg.pageInd', { page: page, pages: totalPages });
        var next = elI18n('button', 'btn btn-sm btn-outline', 'common.next'); next.type = 'button';
        next.disabled = page >= totalPages;
        next.addEventListener('click', function () { if (page < totalPages) { page++; loadPage(); } });
        pager.appendChild(prev); pager.appendChild(ind); pager.appendChild(next);
        body.appendChild(pager);
      }

      if (K.i18n) K.i18n.apply(body);
    }
  }

  // us-item card: number + i18n label, optional colored value class + sub-line(s)
  function usItem(valueText, labelKey, valueCls, subs) {
    var it = el('div', 'us-item');
    it.appendChild(el('div', 'us-n' + (valueCls ? ' ' + valueCls : ''), String(valueText)));
    it.appendChild(elI18n('div', 'us-k', labelKey));
    if (subs != null) {
      var arr = Array.isArray(subs) ? subs : [subs];
      arr.forEach(function (s) { if (s) it.appendChild(el('div', 'us-sub', s)); });
    }
    return it;
  }

  // records table columns:
  // 时间 / IP / 账号 / 模型 / Token 用量(输入/输出/缓存/输入总计) / 费用 / Kiro Credits
  function buildRecordsTable(records) {
    var wrap = el('div', 'table-container');
    if (!records.length) { wrap.appendChild(elI18n('div', 'empty-state', 'usg.noRecords')); return wrap; }
    var table = el('table', 'data-table usage-records-table');
    var thead = el('thead'); var htr = el('tr');
    [
      ['usg.recTime', null], ['usg.recClientIp', null], ['usg.recCredential', null],
      ['usg.recModel', null], ['usg.recTokenUsage', null],
      ['usg.recCost', 'ta-right'], ['usg.recKiroCredits', 'ta-right']
    ].forEach(function (h) {
      htr.appendChild(elI18n('th', h[1], h[0]));
    });
    thead.appendChild(htr); table.appendChild(thead);
    var tb = el('tbody');
    // records are returned newest-first by the API
    records.forEach(function (r) {
      var tr = el('tr');
      // 时间
      tr.appendChild(el('td', 'ur-time', formatUsageDate(r.createdAt)));
      // IP
      tr.appendChild(el('td', 'ur-ip', r.clientIp || '—'));
      // 账号
      var acctTd = el('td', 'ur-acct', r.credentialLabel || '—');
      if (r.credentialLabel) acctTd.title = r.credentialLabel;
      tr.appendChild(acctTd);
      // 模型
      tr.appendChild(el('td', 'ur-model ' + usageModelColor(r.model), r.model || '—'));
      // Token 用量 (输入 / 输出 / 缓存读取 / 输入总计)
      var tokTd = el('td', 'ur-tokens');
      var cacheRead = Number(r.cacheReadInputTokens) || 0;
      var inTotal = Number(r.inputTokens) || 0;
      tokTd.appendChild(tokLine('usg.recInputTokens', formatTokens(Math.max(0, inTotal - cacheRead))));
      tokTd.appendChild(tokLine('usg.recOutputTokens', formatTokens(Number(r.outputTokens) || 0)));
      tokTd.appendChild(tokLine('usg.recCacheRead', formatTokens(cacheRead), 'tok-cache'));
      tokTd.appendChild(tokLine('usg.recInputSum', formatTokens(inTotal), 'tok-sum'));
      tr.appendChild(tokTd);
      // 费用
      tr.appendChild(el('td', 'ur-cost ta-right', formatCost(r.estimatedCost)));
      // Kiro Credits (creditsUsed ?? cost/0.72; ✓ if measured; 省 X if saved>0)
      var credTd = el('td', 'ur-credits ta-right');
      var credVal = (r.creditsUsed != null ? Number(r.creditsUsed) : (Number(r.estimatedCost) || 0) / 0.72).toFixed(4);
      credTd.appendChild(document.createTextNode(credVal));
      if (r.creditsUsed != null) {
        var chk = el('span', 'ur-cred-check', ' ✓');
        credTd.appendChild(chk);
      }
      if (r.creditsSaved != null && Number(r.creditsSaved) > 0) {
        credTd.appendChild(el('span', 'ur-cred-saved', ' (' + t('usg.saved', { n: Number(r.creditsSaved).toFixed(4) }) + ')'));
      }
      tr.appendChild(credTd);
      tb.appendChild(tr);
    });
    table.appendChild(tb); wrap.appendChild(table);
    return wrap;
  }
  // a "label：value" line inside the Token 用量 cell
  function tokLine(labelKey, valueText, extraCls) {
    var d = el('div', 'ur-tok-line' + (extraCls ? ' ' + extraCls : ''));
    d.appendChild(elI18n('span', 'ur-tok-k', labelKey));
    d.appendChild(el('span', 'ur-tok-sep', '：'));
    d.appendChild(el('span', 'ur-tok-v', valueText));
    return d;
  }

  // ---------- failure / throttle logs ----------
  function showLogs(acc, kind) {
    var path = kind === 'failure' ? 'failure-logs' : 'throttle-logs';
    var titleKey = kind === 'failure' ? 'acc.failureLogs' : 'acc.throttleLogs';
    var body = el('div');
    body.appendChild(elI18n('div', 'empty-state', 'common.loading'));
    openModal({ title: t(titleKey), bodyEl: body, size: 'lg' });
    api.get('/credentials/' + acc.id + '/' + path, { page: 1, page_size: 30 }).then(function (data) {
      body.innerHTML = '';
      var logs = data.logs || data.records || data.data || [];
      if (!logs.length) { body.appendChild(elI18n('div', 'empty-state', 'common.empty')); if (K.i18n) K.i18n.apply(body); return; }
      var wrap = el('div', 'table-container');
      var table = el('table', 'data-table');
      var thead = el('thead'); var htr = el('tr');
      htr.appendChild(elI18n('th', null, 'usg.recTime'));
      htr.appendChild(elI18n('th', null, 'common.status'));
      htr.appendChild(elI18n('th', null, 'log.detail'));
      thead.appendChild(htr); table.appendChild(thead);
      var tb = el('tbody');
      logs.forEach(function (lg) {
        var tr = el('tr');
        tr.appendChild(el('td', null, fmtDate(lg.timestamp || lg.createdAt || lg.time)));
        tr.appendChild(el('td', null, String(lg.statusCode || lg.status || lg.reason || '—')));
        // 详情 = 上游响应体。端点回的是 EventLogRecordView
        // {credentialId, requestType, statusCode, responseBody, createdAt}
        // (src/admin/handler.rs),压根没有 message/detail/error 这几个键 ——
        // 老写法逐个取空,于是每一行都显示「—」,这个弹窗唯一的用处(看清账号
        // 为什么在失败/被限流)彻底失效。responseBody 才是真因,放在最前面。
        var detail = lg.responseBody || lg.message || lg.detail || lg.error || '';
        var msg = el('td'); msg.appendChild(el('span', 'kv-mono', detail || '—'));
        tr.appendChild(msg);
        tb.appendChild(tr);
      });
      table.appendChild(tb); wrap.appendChild(table); body.appendChild(wrap);
      if (K.i18n) K.i18n.apply(body);
    }).catch(function (e) {
      body.innerHTML = '';
      body.appendChild(el('div', 'empty-state', e.message || t('common.error')));
    });
  }

  // ---------- shared field builder ----------
  // labelKey required? pass reqStar=true to append a red "*". phKey/hintKey are
  // optional i18n placeholder / helper-text keys (kept in sync on langchange via
  // data-i18n-ph / data-i18n on child nodes).
  function formField(labelKey, id, type, value, opts) {
    opts = opts || {};
    var g = el('div', 'form-group');
    var lab = elI18n('label', 'form-label', labelKey); lab.setAttribute('for', id);
    if (opts.reqStar) { var star = el('span', 'req-star', ' *'); lab.appendChild(star); }
    g.appendChild(lab);
    var input;
    if (type === 'textarea') { input = el('textarea', 'form-control'); }
    else { input = el('input', 'form-control'); input.type = type || 'text'; }
    input.id = id;
    if (value != null) input.value = value;
    if (opts.phKey) { input.setAttribute('data-i18n-ph', opts.phKey); input.setAttribute('placeholder', t(opts.phKey)); }
    g.appendChild(input);
    if (opts.hintKey) g.appendChild(elI18n('div', 'form-hint', opts.hintKey));
    return g;
  }
  function fval(id) { var e = document.getElementById(id); return e ? e.value.trim() : ''; }

  // ---------- add / edit form ----------
  // Two distinct dialogs mirroring the reference:
  //   editing → edit-credential-dialog.tsx: only-changed-fields, NO refreshToken,
  //             NO authMethod select; idc client fields shown only for idc accounts;
  //             wired to PUT /credentials/{id}.
  //   adding  → add-credential-dialog.tsx: refreshToken* + authMethod select +
  //             regions + (idc) clientId*/clientSecret* + profileArn + priority +
  //             machineId + proxy; wired to POST /credentials.
  function openAddEditForm(acc) {
    if (acc) return openEditForm(acc);
    return openAddForm();
  }

  // ----- Edit dialog: PUT /credentials/{id}, only-changed-fields semantics -----
  function openEditForm(acc) {
    var isIdc = acc.authMethod === 'idc' || acc.authMethod === 'iam' || acc.authMethod === 'iam-sso';
    var form = el('div');

    // "只填写需要修改的字段，留空的字段不会被更改。"
    form.appendChild(elI18n('p', 'mng-inline-note', 'edit.onlyChanged'));

    form.appendChild(formField('accForm.nickname', 'accNick', 'text', acc.nickname || '', { phKey: 'edit.nicknamePh' }));
    form.appendChild(formField('accForm.email', 'accEmail', 'text', acc.email || '', { phKey: 'edit.emailPh' }));
    form.appendChild(formField('accForm.weight', 'accWeight', 'number', acc.weight != null ? acc.weight : (acc.priority != null ? acc.priority : 1), { phKey: 'edit.weightPh', hintKey: 'edit.weightHint' }));

    // Region 配置 (auth + api)
    var regionRow = el('div', 'form-row');
    regionRow.appendChild(formField('accForm.authRegion', 'accAuthRegion', 'text', '', { phKey: 'accForm.authRegion' }));
    regionRow.appendChild(formField('accForm.apiRegion', 'accApiRegion', 'text', '', { phKey: 'accForm.apiRegion' }));
    var regionG = el('div', 'form-group');
    regionG.appendChild(elI18n('label', 'form-label', 'accForm.regionConfig'));
    regionG.appendChild(regionRow);
    regionG.appendChild(elI18n('div', 'form-hint', 'accForm.regionHint'));
    form.appendChild(regionG);

    // idc client fields (only for idc accounts)
    if (isIdc) {
      form.appendChild(formField('accForm.clientId', 'accClientId', 'text', '', { phKey: 'edit.keepBlank' }));
      form.appendChild(formField('accForm.clientSecret', 'accClientSecret', 'password', '', { phKey: 'edit.keepBlank' }));
    }

    form.appendChild(formField('accForm.profileArn', 'accProfileArn', 'text', '', { phKey: acc.hasProfileArn ? 'edit.profileArnConfigured' : 'edit.keepBlank', hintKey: 'accForm.profileArnHint' }));
    form.appendChild(formField('accForm.machineId', 'accMachineId', 'text', '', { phKey: 'edit.keepBlank' }));

    // 代理配置
    var proxyG = el('div', 'form-group');
    proxyG.appendChild(elI18n('label', 'form-label', 'accForm.proxyConfig'));
    var proxyUrl = el('input', 'form-control'); proxyUrl.type = 'text'; proxyUrl.id = 'accProxyUrl';
    proxyUrl.value = acc.proxyUrl || '';
    proxyUrl.setAttribute('data-i18n-ph', 'accForm.proxyUrlPh'); proxyUrl.setAttribute('placeholder', t('accForm.proxyUrlPh'));
    proxyG.appendChild(proxyUrl);
    var proxyRow = el('div', 'form-row');
    proxyRow.appendChild(formField('accForm.proxyUsername', 'accProxyUser', 'text', '', { phKey: 'accForm.proxyUsername' }));
    proxyRow.appendChild(formField('accForm.proxyPassword', 'accProxyPass', 'password', '', { phKey: 'accForm.proxyPassword' }));
    proxyG.appendChild(proxyRow);
    form.appendChild(proxyG);

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var save = elI18n('button', 'btn btn-primary', 'common.save'); save.type = 'button';
    footer.appendChild(cancel); footer.appendChild(save);

    var m = openModal({ title: t('accForm.editTitle') + ' #' + acc.id, bodyEl: form, footerEl: footer, size: 'lg' });
    cancel.addEventListener('click', function () { m.close(); });
    save.addEventListener('click', function () {
      // build a payload containing ONLY changed / newly-entered fields
      var payload = {};
      // 后端把空串当"不修改"(update_credential:`req.nickname.filter(|s| !s.is_empty())`,
      // 邮箱同理),清空昵称/邮箱在服务端根本不会发生。老写法照旧把 {"nickname":""}
      // 发出去,后端回 200,前端弹「账号已保存」,而身后那一行的旧昵称纹丝不动 ——
      // 成功提示当场被打脸。现在与本对话框自己的说明(「留空的字段不会被更改」)对齐:
      // 空值不进 payload,并明确告诉用户"清空"这件事做不到,别让他以为已经生效。
      var wantsClear = false;
      var nick = fval('accNick');
      if (nick !== (acc.nickname || '')) {
        if (nick) payload.nickname = nick; else if (acc.nickname) wantsClear = true;
      }
      var email = fval('accEmail');
      if (email !== (acc.email || '')) {
        if (email) payload.email = email; else if (acc.email) wantsClear = true;
      }
      var authRegion = fval('accAuthRegion'); if (authRegion) payload.authRegion = authRegion;
      var apiRegion = fval('accApiRegion'); if (apiRegion) payload.apiRegion = apiRegion;
      if (isIdc) {
        var cid = fval('accClientId'); if (cid) payload.clientId = cid;
        var csec = fval('accClientSecret'); if (csec) payload.clientSecret = csec;
      }
      var parn = fval('accProfileArn'); if (parn) payload.profileArn = parn;
      var mid = fval('accMachineId'); if (mid) payload.machineId = mid;
      var purl = fval('accProxyUrl'); if (purl !== (acc.proxyUrl || '')) payload.proxyUrl = purl;
      var puser = fval('accProxyUser'); if (puser) payload.proxyUsername = puser;
      var ppass = fval('accProxyPass'); if (ppass) payload.proxyPassword = ppass;
      var w = fval('accWeight');
      if (w !== '' && Number(w) !== (acc.weight != null ? acc.weight : (acc.priority != null ? acc.priority : 1))) {
        var nw = parseInt(w, 10);
        if (isNaN(nw) || nw < 0) { api.toast(t('edit.weightInvalid'), 'error'); return; }
        payload.weight = nw;
      }
      if (Object.keys(payload).length === 0) {
        // 只想清空、别的什么都没改 → 说清楚为什么不保存,而不是含糊的"没有需要更新的字段"。
        api.toast(wantsClear ? t('edit.cannotClear') : t('edit.noChanges'), wantsClear ? 'error' : 'info');
        return;
      }

      save.disabled = true;
      api.put('/credentials/' + acc.id, payload)
        .then(function () {
          api.toast(t('acc.saved'), 'success');
          // 别的字段确实存下了,但被清空的那个没有 —— 单独再提示一次,免得用户
          // 看见"已保存"就以为昵称/邮箱也一并清掉了。
          if (wantsClear) api.toast(t('edit.cannotClear'), 'info');
          m.close(); loadAccounts();
        })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); save.disabled = false; });
    });
  }

  // ----- Add dialog: POST /credentials -----
  function openAddForm() {
    var form = el('div');

    form.appendChild(formField('accForm.refreshToken', 'accToken', 'password', '', { reqStar: true, phKey: 'add.refreshTokenPh' }));
    form.appendChild(formField('accForm.email', 'accEmail', 'text', '', { phKey: 'add.emailPh' }));

    // auth method select
    var authG = el('div', 'form-group');
    authG.appendChild(elI18n('label', 'form-label', 'accForm.authMethod'));
    var authSel = el('select', 'form-control'); authSel.id = 'accAuth';
    [['social', 'add.methodSocial'], ['idc', 'add.methodIdc']].forEach(function (o) {
      var opt = elI18n('option', null, o[1]); opt.value = o[0]; authSel.appendChild(opt);
    });
    authSel.value = 'social';
    authG.appendChild(authSel);
    form.appendChild(authG);

    // Region 配置
    var regionRow = el('div', 'form-row');
    regionRow.appendChild(formField('accForm.authRegion', 'accAuthRegion', 'text', '', { phKey: 'accForm.authRegion' }));
    regionRow.appendChild(formField('accForm.apiRegion', 'accApiRegion', 'text', '', { phKey: 'accForm.apiRegion' }));
    var regionG = el('div', 'form-group');
    regionG.appendChild(elI18n('label', 'form-label', 'accForm.regionConfig'));
    regionG.appendChild(regionRow);
    regionG.appendChild(elI18n('div', 'form-hint', 'add.regionHint'));
    form.appendChild(regionG);

    // idc-only client fields
    var idcBox = el('div'); idcBox.id = 'idcBox';
    idcBox.appendChild(formField('accForm.clientId', 'accClientId', 'text', '', { reqStar: true, phKey: 'add.clientIdPh' }));
    idcBox.appendChild(formField('accForm.clientSecret', 'accClientSecret', 'password', '', { reqStar: true, phKey: 'add.clientSecretPh' }));
    form.appendChild(idcBox);

    // Profile ARN (always visible in reference add dialog)
    form.appendChild(formField('accForm.profileArn', 'accProfileArn', 'text', '', { phKey: 'add.profileArnPh', hintKey: 'add.profileArnHint' }));

    // 优先级
    form.appendChild(formField('accForm.priority', 'accPriority', 'number', '0', { phKey: 'add.priorityPh', hintKey: 'add.priorityHint' }));

    // Machine ID
    form.appendChild(formField('accForm.machineId', 'accMachineId', 'text', '', { phKey: 'add.machineIdPh', hintKey: 'add.machineIdHint' }));

    // 代理配置
    var proxyG = el('div', 'form-group');
    proxyG.appendChild(elI18n('label', 'form-label', 'accForm.proxyConfig'));
    var proxyUrl = el('input', 'form-control'); proxyUrl.type = 'text'; proxyUrl.id = 'accProxyUrl';
    proxyUrl.setAttribute('data-i18n-ph', 'add.proxyUrlPh'); proxyUrl.setAttribute('placeholder', t('add.proxyUrlPh'));
    proxyG.appendChild(proxyUrl);
    var proxyRow = el('div', 'form-row');
    proxyRow.appendChild(formField('accForm.proxyUsername', 'accProxyUser', 'text', '', { phKey: 'accForm.proxyUsername' }));
    proxyRow.appendChild(formField('accForm.proxyPassword', 'accProxyPass', 'password', '', { phKey: 'accForm.proxyPassword' }));
    proxyG.appendChild(proxyRow);
    proxyG.appendChild(elI18n('div', 'form-hint', 'add.proxyHint'));
    form.appendChild(proxyG);

    function syncIdc() { idcBox.style.display = authSel.value === 'idc' ? '' : 'none'; }
    authSel.addEventListener('change', syncIdc); syncIdc();

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var save = elI18n('button', 'btn btn-primary', 'add.submit'); save.type = 'button';
    footer.appendChild(cancel); footer.appendChild(save);

    var m = openModal({ title: t('accForm.addTitle'), bodyEl: form, footerEl: footer, size: 'lg' });
    cancel.addEventListener('click', function () { m.close(); });
    save.addEventListener('click', function () {
      var isIdc = authSel.value === 'idc';
      var token = fval('accToken');
      if (!token) { api.toast(t('add.needToken'), 'error'); return; }
      if (isIdc && (!fval('accClientId') || !fval('accClientSecret'))) { api.toast(t('add.needClientCreds'), 'error'); return; }
      var payload = {
        refreshToken: token,
        authMethod: authSel.value,
        email: fval('accEmail') || undefined,
        authRegion: fval('accAuthRegion') || undefined,
        apiRegion: fval('accApiRegion') || undefined,
        profileArn: fval('accProfileArn') || undefined,
        priority: parseInt(fval('accPriority'), 10) || 0,
        machineId: fval('accMachineId') || undefined,
        proxyUrl: fval('accProxyUrl') || undefined,
        proxyUsername: fval('accProxyUser') || undefined,
        proxyPassword: fval('accProxyPass') || undefined
      };
      if (isIdc) {
        payload.clientId = fval('accClientId') || undefined;
        payload.clientSecret = fval('accClientSecret') || undefined;
      }
      save.disabled = true;
      api.post('/credentials', payload)
        .then(function () { api.toast(t('acc.saved'), 'success'); m.close(); loadAccounts(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); save.disabled = false; });
    });
  }

  // ---------- Web Cookie import (frontend-only → POST /credentials, social) ----------
  // Mirrors web-cookie-import-dialog.tsx: Google / GitHub provider select +
  // RefreshToken textarea + email + nickname; posts as a social credential.
  function openWebCookieModal() {
    var form = el('div');
    form.appendChild(elI18n('p', 'mng-inline-note', 'webcookie.desc'));

    var provG = el('div', 'form-group');
    provG.appendChild(elI18n('label', 'form-label', 'webcookie.provider'));
    var provSel = el('select', 'form-control'); provSel.id = 'wcProvider';
    [['google', 'webcookie.google'], ['github', 'webcookie.github']].forEach(function (o) {
      var opt = elI18n('option', null, o[1]); opt.value = o[0]; provSel.appendChild(opt);
    });
    provG.appendChild(provSel);
    form.appendChild(provG);

    form.appendChild(formField('webcookie.refreshToken', 'wcToken', 'textarea', '', { reqStar: true, phKey: 'webcookie.tokenPh', hintKey: 'webcookie.steps' }));
    form.appendChild(formField('accForm.email', 'wcEmail', 'text', '', { phKey: 'localcache.emailPh' }));
    form.appendChild(formField('accForm.nickname', 'wcNick', 'text', '', { phKey: 'localcache.nickPh' }));

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var save = elI18n('button', 'btn btn-primary', 'add.submit'); save.type = 'button';
    footer.appendChild(cancel); footer.appendChild(save);

    var m = openModal({ title: t('webcookie.title'), bodyEl: form, footerEl: footer, size: 'lg' });
    cancel.addEventListener('click', function () { m.close(); });
    save.addEventListener('click', function () {
      var token = fval('wcToken');
      if (!token) { api.toast(t('webcookie.needToken'), 'error'); return; }
      save.disabled = true;
      api.post('/credentials', {
        refreshToken: token,
        authMethod: 'social',
        email: fval('wcEmail') || undefined,
        nickname: fval('wcNick') || undefined
      }).then(function () { api.toast(t('acc.saved'), 'success'); m.close(); loadAccounts(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); save.disabled = false; });
    });
  }

  // ---------- Local Cache import (frontend-only → POST /credentials) ----------
  // Mirrors local-cache-import-dialog.tsx: provider select (Builder ID / Enterprise
  // / Google / GitHub); kiro-auth-token.json textarea (+ file upload); IdC client
  // registration JSON (only for builderid/enterprise); email + nickname. Parses
  // refreshToken/region (+ clientId/clientSecret) client-side and posts a credential.
  function openLocalCacheModal() {
    var form = el('div');
    form.appendChild(elI18n('p', 'mng-inline-note', 'localcache.desc'));

    var provG = el('div', 'form-group');
    provG.appendChild(elI18n('label', 'form-label', 'localcache.provider'));
    var provSel = el('select', 'form-control'); provSel.id = 'lcProvider';
    [['builderid', 'localcache.builderid'], ['enterprise', 'localcache.enterprise'], ['google', 'webcookie.google'], ['github', 'webcookie.github']].forEach(function (o) {
      var opt = elI18n('option', null, o[1]); opt.value = o[0]; provSel.appendChild(opt);
    });
    provG.appendChild(provSel);
    form.appendChild(provG);

    // kiro-auth-token.json textarea + upload
    var tokG = formField('localcache.tokenJson', 'lcTokenJson', 'textarea', '', { reqStar: true, phKey: 'localcache.tokenJsonPh', hintKey: 'localcache.tokenPath' });
    var tokUpload = fileUploadRow('lcTokenFile', function (text) { document.getElementById('lcTokenJson').value = text; });
    tokG.insertBefore(tokUpload, tokG.querySelector('.form-hint'));
    form.appendChild(tokG);

    // IdC client registration JSON (builderid/enterprise only)
    var clientBox = el('div'); clientBox.id = 'lcClientBox';
    var cliG = formField('localcache.clientJson', 'lcClientJson', 'textarea', '', { reqStar: true, phKey: 'localcache.clientJsonPh', hintKey: 'localcache.clientPath' });
    var cliUpload = fileUploadRow('lcClientFile', function (text) { document.getElementById('lcClientJson').value = text; });
    cliG.insertBefore(cliUpload, cliG.querySelector('.form-hint'));
    clientBox.appendChild(cliG);
    form.appendChild(clientBox);

    form.appendChild(formField('accForm.email', 'lcEmail', 'text', '', { phKey: 'localcache.emailPh' }));
    form.appendChild(formField('accForm.nickname', 'lcNick', 'text', '', { phKey: 'localcache.nickPh' }));

    function needsClientCreds() { var p = provSel.value; return p === 'builderid' || p === 'enterprise'; }
    function syncClient() { clientBox.style.display = needsClientCreds() ? '' : 'none'; }
    provSel.addEventListener('change', syncClient); syncClient();

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var save = elI18n('button', 'btn btn-primary', 'add.submit'); save.type = 'button';
    footer.appendChild(cancel); footer.appendChild(save);

    var m = openModal({ title: t('localcache.title'), bodyEl: form, footerEl: footer, size: 'lg' });
    cancel.addEventListener('click', function () { m.close(); });
    save.addEventListener('click', function () {
      var tokenRaw = fval('lcTokenJson');
      if (!tokenRaw) { api.toast(t('localcache.needToken'), 'error'); return; }
      var refreshToken, region;
      try {
        var parsed = JSON.parse(tokenRaw);
        if (typeof parsed.refreshToken === 'string') refreshToken = parsed.refreshToken;
        if (typeof parsed.region === 'string') region = parsed.region;
      } catch (e) { api.toast(t('localcache.badTokenJson'), 'error'); return; }
      if (!refreshToken || !refreshToken.trim()) { api.toast(t('localcache.noRefreshToken'), 'error'); return; }

      var clientId, clientSecret;
      if (needsClientCreds()) {
        var clientRaw = fval('lcClientJson');
        if (!clientRaw) { api.toast(t('localcache.needClientJson'), 'error'); return; }
        try {
          var pc = JSON.parse(clientRaw);
          if (typeof pc.clientId === 'string') clientId = pc.clientId;
          if (typeof pc.clientSecret === 'string') clientSecret = pc.clientSecret;
        } catch (e2) { api.toast(t('localcache.badClientJson'), 'error'); return; }
        if (!clientId || !clientSecret) { api.toast(t('localcache.noClientCreds'), 'error'); return; }
      }
      var authMethod = (clientId && clientSecret) ? 'idc' : 'social';

      save.disabled = true;
      api.post('/credentials', {
        refreshToken: refreshToken.trim(),
        authMethod: authMethod,
        clientId: clientId ? clientId.trim() : undefined,
        clientSecret: clientSecret ? clientSecret.trim() : undefined,
        authRegion: region ? region.trim() : undefined,
        apiRegion: region ? region.trim() : undefined,
        email: fval('lcEmail') || undefined,
        nickname: fval('lcNick') || undefined
      }).then(function () { api.toast(t('acc.saved'), 'success'); m.close(); loadAccounts(); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); save.disabled = false; });
    });
  }

  // hidden file-input + "上传文件" button; reads the picked .json into `onText`.
  function fileUploadRow(inputId, onText) {
    var row = el('div', 'upload-row');
    var btn = elI18n('button', 'btn btn-outline btn-sm', 'localcache.uploadFile'); btn.type = 'button';
    var fi = el('input'); fi.type = 'file'; fi.id = inputId; fi.accept = '.json,application/json'; fi.style.display = 'none';
    btn.addEventListener('click', function () { fi.click(); });
    fi.addEventListener('change', function () {
      var f = fi.files && fi.files[0];
      if (!f) return;
      var reader = new FileReader();
      reader.onload = function (ev) { if (typeof ev.target.result === 'string') onText(ev.target.result); };
      reader.onerror = function () { api.toast(t('localcache.fileReadFail'), 'error'); };
      reader.readAsText(f);
    });
    row.appendChild(btn); row.appendChild(fi);
    return row;
  }

  // ---------- bulk import ----------
  // 批量导入 JSON(自动验活):
  // 逐条实时「检查重复 → 添加 → 验活 → 失败回滚」,每个账号一行、实时显示状态图标 +
  // 进度条 + 成功/重复/失败统计。账号是**逐个**加进池并即时验活的:中途关掉也不白等,
  // 已验活通过的都已落库保留(未通过的已回滚删除)。
  function openImportModal() {
    var list = null, results = [], rowEls = [];
    var importing = false, anyAdded = false;
    var total = 0, cur = 0, okCount = 0, dupCount = 0, failCount = 0;
    var rbSuccess = 0, rbFailed = 0, rbSkipped = 0;

    var body = el('div');
    // JSON 输入
    var g = el('div', 'form-group');
    g.appendChild(elI18n('label', 'form-label', 'imp.jsonLabel'));
    var ta = el('textarea', 'form-control'); ta.rows = 8; ta.style.fontFamily = 'monospace';
    ta.setAttribute('placeholder', t('imp.paste'));
    g.appendChild(ta);
    var hint = elI18n('div', null, 'imp.hint');
    hint.style.cssText = 'font-size:0.78rem;color:var(--text-secondary);margin-top:6px;';
    g.appendChild(hint);
    body.appendChild(g);

    // 进度 + 统计 + 结果列表(导入开始前隐藏)
    var panel = el('div'); panel.style.cssText = 'display:none;margin-top:1rem;';
    var progHead = el('div'); progHead.style.cssText = 'display:flex;justify-content:space-between;font-size:0.85rem;margin-bottom:5px;';
    var progLabel = elI18n('span', null, 'imp.progress');
    var progCount = el('span', null, '0 / 0');
    progHead.appendChild(progLabel); progHead.appendChild(progCount); panel.appendChild(progHead);
    var progBar = el('div'); progBar.style.cssText = 'width:100%;height:8px;border-radius:999px;background:var(--bg-tertiary);overflow:hidden;';
    var progFill = el('div'); progFill.style.cssText = 'height:100%;width:0;background:var(--primary);border-radius:999px;transition:width 0.2s;';
    progBar.appendChild(progFill); panel.appendChild(progBar);
    var procText = el('div'); procText.style.cssText = 'font-size:0.75rem;color:var(--text-secondary);margin-top:6px;min-height:1em;';
    panel.appendChild(procText);
    var stats = el('div'); stats.style.cssText = 'display:flex;gap:1.25rem;font-size:0.85rem;margin:0.85rem 0;';
    var statOk = el('span'); statOk.style.color = 'var(--success)';
    var statDup = el('span'); statDup.style.color = 'var(--warning)';
    var statFail = el('span'); statFail.style.color = 'var(--danger)';
    stats.appendChild(statOk); stats.appendChild(statDup); stats.appendChild(statFail); panel.appendChild(stats);
    var listEl = el('div'); listEl.style.cssText = 'border:1px solid var(--border-color);border-radius:var(--radius-md,8px);max-height:300px;overflow-y:auto;';
    panel.appendChild(listEl);
    body.appendChild(panel);

    var footer = el('div', 'modal-footer');
    var cancel = elI18n('button', 'btn btn-outline', 'common.cancel'); cancel.type = 'button';
    var run = elI18n('button', 'btn btn-primary', 'imp.run'); run.type = 'button';
    footer.appendChild(cancel); footer.appendChild(run);

    var m = openModal({ title: t('imp.title'), bodyEl: body, footerEl: footer, size: 'lg',
      onClose: function () { if (anyAdded) loadAccounts(); } });
    // 导入进行中禁止关闭(隐藏 ✕、拦截遮罩点击)。
    var closeX = m.modal.querySelector('.modal-close');
    m.overlay.addEventListener('click', function (e) {
      if (importing && e.target === m.overlay) e.stopImmediatePropagation();
    }, true);
    cancel.addEventListener('click', function () { if (!importing) m.close(); });

    function iconEl(status) {
      var s = el('span'); s.style.cssText = 'flex-shrink:0;width:20px;height:20px;display:inline-flex;';
      if (status === 'pending') {
        s.innerHTML = '<span style="width:16px;height:16px;margin:2px;border-radius:50%;border:2px solid var(--border-color);display:inline-block;"></span>';
      } else if (status === 'checking' || status === 'verifying') {
        s.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="var(--info)" stroke-width="2" stroke-linecap="round" style="animation:spin 0.8s linear infinite"><path d="M21 12a9 9 0 1 1-6.219-8.56"/></svg>';
      } else if (status === 'verified') {
        s.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="var(--success)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/></svg>';
      } else if (status === 'duplicate') {
        s.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="var(--warning)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>';
      } else {
        s.innerHTML = '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="var(--danger)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/></svg>';
      }
      return s;
    }
    function statusText(r) {
      if (r.status === 'failed') {
        if (r.rollbackStatus === 'success') return t('imp.st.failExcluded');
        if (r.rollbackStatus === 'failed') return t('imp.st.failNotExcluded');
        return t('imp.st.failNotCreated');
      }
      return t('imp.st.' + r.status);
    }
    function renderRow(i) {
      var r = results[i], row = rowEls[i];
      row.textContent = '';
      row.style.cssText = 'padding:0.6rem 0.75rem;display:flex;align-items:flex-start;gap:0.6rem;' + (i ? 'border-top:1px solid var(--border-color);' : '');
      row.appendChild(iconEl(r.status));
      var main = el('div'); main.style.cssText = 'flex:1;min-width:0;';
      var line = el('div'); line.style.cssText = 'display:flex;align-items:center;gap:0.5rem;';
      var name = el('span', null, r.email || t('imp.acctNo', { n: r.index }));
      name.style.cssText = 'font-size:0.85rem;font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;';
      var st = el('span', null, statusText(r)); st.style.cssText = 'font-size:0.75rem;color:var(--text-secondary);flex-shrink:0;';
      line.appendChild(name); line.appendChild(st); main.appendChild(line);
      if (r.usage) { var u = el('div', null, t('imp.usage', { usage: r.usage })); u.style.cssText = 'font-size:0.72rem;color:var(--text-secondary);margin-top:2px;'; main.appendChild(u); }
      if (r.error) { var e = el('div', null, r.error); e.style.cssText = 'font-size:0.72rem;color:var(--danger);margin-top:2px;'; main.appendChild(e); }
      row.appendChild(main);
    }
    function paintStats() {
      statOk.textContent = '✓ ' + t('imp.statOk') + ': ' + okCount;
      statDup.textContent = '⚠ ' + t('imp.statDup') + ': ' + dupCount;
      statFail.textContent = '✗ ' + t('imp.statFail') + ': ' + failCount;
    }
    function paintProgress() {
      progCount.textContent = cur + ' / ' + total;
      progFill.style.width = (total ? (cur / total * 100) : 0) + '%';
    }
    function fmtNum(v) {
      var n = Number(v);
      if (v == null || !isFinite(n)) return v == null ? '?' : String(v);
      return String(Math.round(n * 100) / 100);
    }

    run.addEventListener('click', function () {
      var raw = ta.value.trim();
      if (!raw) return;
      var parsed;
      try { parsed = JSON.parse(raw); } catch (e) { api.toast(t('imp.badJson'), 'error'); return; }
      // 归整顶层为账号列表:数组 / KAM `{accounts:[...]}` / 单对象。
      list = Array.isArray(parsed) ? parsed
        : (parsed && Array.isArray(parsed.accounts)) ? parsed.accounts
        : (parsed && typeof parsed === 'object') ? [parsed] : null;
      if (!list || !list.length) { api.toast(t('imp.noAccounts'), 'error'); return; }

      total = list.length; cur = okCount = dupCount = failCount = 0;
      rbSuccess = rbFailed = rbSkipped = 0;
      results = list.map(function (_, i) { return { index: i + 1, status: 'pending' }; });
      rowEls = []; listEl.textContent = '';
      results.forEach(function (_, i) { var row = el('div'); rowEls.push(row); listEl.appendChild(row); renderRow(i); });

      importing = true; anyAdded = false;
      run.style.display = 'none'; cancel.disabled = true; ta.disabled = true;
      if (closeX) closeX.style.display = 'none';
      panel.style.display = ''; progLabel.textContent = t('imp.progress');
      paintStats(); paintProgress();
      processNext(0);
    });

    function advance(i) { cur = i + 1; paintProgress(); paintStats(); processNext(i + 1); }

    // 逐条:检查重复(后端按 refreshToken 去重)→ 添加 → 等 1s → 验活(查余额)→ 失败回滚删除。
    function processNext(i) {
      if (i >= total) { finish(); return; }
      results[i].status = 'checking'; renderRow(i);
      procText.textContent = t('imp.processing', { i: i + 1, total: total });
      var payload = toAddPayload(list[i]);
      if (!payload) { results[i].status = 'failed'; results[i].rollbackStatus = 'skipped'; failCount++; rbSkipped++; renderRow(i); advance(i); return; }
      api.post('/credentials', payload).then(function (r) {
        if (r && r.duplicate) {
          results[i].status = 'duplicate'; results[i].error = t('imp.dupExists');
          if (r.email) results[i].email = r.email;
          dupCount++; renderRow(i); advance(i); return;
        }
        var id = r && (r.credentialId != null ? r.credentialId : r.id);
        if (id == null) { results[i].status = 'failed'; results[i].rollbackStatus = 'skipped'; failCount++; rbSkipped++; renderRow(i); advance(i); return; }
        anyAdded = true;
        if (r.email) results[i].email = r.email;
        results[i].credentialId = id; results[i].status = 'verifying'; renderRow(i);
        setTimeout(function () {
          api.get('/credentials/' + id + '/balance').then(function (bal) {
            results[i].status = 'verified';
            if (bal && bal.usageLimit != null) results[i].usage = fmtNum(bal.currentUsage) + '/' + fmtNum(bal.usageLimit);
            okCount++;
            procText.textContent = t('imp.verifiedName', { name: results[i].email || t('imp.acctNo', { n: results[i].index }) });
            renderRow(i); advance(i);
          }, function () {
            // 验活失败 → 回滚删除(过滤失效账号)
            api.del('/credentials/' + id).then(function () {
              results[i].status = 'failed'; results[i].rollbackStatus = 'success'; failCount++; rbSuccess++; renderRow(i); advance(i);
            }, function () {
              results[i].status = 'failed'; results[i].rollbackStatus = 'failed'; failCount++; rbFailed++; renderRow(i); advance(i);
            });
          });
        }, 1000);
      }, function (err) {
        results[i].status = 'failed'; results[i].rollbackStatus = 'skipped';
        results[i].error = (err && err.message) ? err.message : undefined;
        failCount++; rbSkipped++; renderRow(i); advance(i);
      });
    }

    function finish() {
      importing = false; cancel.disabled = false;
      cancel.textContent = t('imp.close'); cancel.removeAttribute('data-i18n');
      if (closeX) closeX.style.display = '';
      progLabel.textContent = t('imp.progressDone'); procText.textContent = '';
      if (failCount === 0 && dupCount === 0) {
        api.toast(t('imp.doneOk', { n: okCount }), 'success');
      } else {
        var failPart = failCount > 0 ? t('imp.doneFailPart', { fail: failCount, ex: rbSuccess, nex: rbFailed }) : '';
        api.toast(t('imp.doneMixed', { ok: okCount, dup: dupCount, fail: failPart }), 'info');
        if (rbFailed > 0) api.toast(t('imp.rollbackWarn', { n: rbFailed }), 'warning');
      }
      if (anyAdded) loadAccounts();
    }
  }

  // 把一个账号对象(顶层扁平或 KAM `credentials` 嵌套)映射成单条 add 的 payload(camelCase)。
  // 无有效 refreshToken → 返回 null(调用方计为跳过/失败)。accessToken/expiresAt 不传:账号
  // 首次使用时会自行用 refreshToken 刷新取回,故无需导入。
  function toAddPayload(acc) {
    if (!acc || typeof acc !== 'object') return null;
    var creds = (acc.credentials && typeof acc.credentials === 'object') ? acc.credentials : null;
    var pick = function (k) {
      var v = (creds && creds[k] != null && creds[k] !== '') ? creds[k] : acc[k];
      if (typeof v === 'string') { v = v.trim(); return v || undefined; }
      return (v != null && v !== '') ? v : undefined;
    };
    var refreshToken = pick('refreshToken');
    if (!refreshToken || typeof refreshToken !== 'string') return null;
    var clientId = pick('clientId'), clientSecret = pick('clientSecret');
    return {
      refreshToken: refreshToken,
      authMethod: pick('authMethod') || (clientId && clientSecret ? 'idc' : 'social'),
      email: pick('email') || pick('nickname'),
      nickname: pick('nickname'),
      clientId: clientId,
      clientSecret: clientSecret,
      profileArn: pick('profileArn'),
      machineId: pick('machineId'),
      authRegion: pick('authRegion') || pick('region'),
      apiRegion: pick('apiRegion'),
      priority: (typeof acc.priority === 'number') ? acc.priority : undefined
    };
  }

  // ---------- interactive login flows ----------
  function openLoginModal(initialTab) {
    var pollTimer = null, pollSessionId = null;

    var body = el('div');
    var tabs = el('div', 'login-tabs');
    var panes = {};
    var tabDefs = [
      ['builderid', 'login2.tabBuilder'],
      ['iam-sso', 'login2.tabIamSso'],
      ['sso-token', 'login2.tabSsoToken']
    ];
    tabDefs.forEach(function (d) {
      var b = elI18n('button', 'login-tab', d[1]); b.type = 'button'; b.setAttribute('data-tab', d[0]);
      b.addEventListener('click', function () { selectTab(d[0]); });
      tabs.appendChild(b);
    });
    body.appendChild(tabs);

    // Builder-ID pane
    var pB = el('div', 'login-pane'); pB.setAttribute('data-pane', 'builderid');
    pB.appendChild(elI18n('p', 'mng-inline-note', 'login2.builderIdDesc'));
    var pbRegion = regionField('login2.region', 'us-east-1', 'login2.regionPh');
    pB.appendChild(pbRegion.group);
    var pbStart = elI18n('button', 'btn btn-primary', 'login2.startBuilder'); pbStart.type = 'button';
    pB.appendChild(pbStart);
    var pbResult = el('div'); pB.appendChild(pbResult);
    panes.builderid = pB; body.appendChild(pB);

    // IAM SSO pane
    var pI = el('div', 'login-pane'); pI.setAttribute('data-pane', 'iam-sso');
    pI.appendChild(elI18n('p', 'mng-inline-note', 'login2.iamSsoDesc'));
    var piStartUrl = textField('login2.startUrl', '', 'login2.startUrlPh');
    pI.appendChild(piStartUrl.group);
    var piRegion = regionField('login2.region', 'us-east-1', 'login2.regionPh');
    pI.appendChild(piRegion.group);
    var piGet = elI18n('button', 'btn btn-primary', 'login2.startIamSso'); piGet.type = 'button';
    pI.appendChild(piGet);
    var piResult = el('div'); pI.appendChild(piResult);
    panes['iam-sso'] = pI; body.appendChild(pI);

    // SSO token pane
    var pT = el('div', 'login-pane'); pT.setAttribute('data-pane', 'sso-token');
    pT.appendChild(elI18n('p', 'mng-inline-note', 'login2.ssoTokenDesc'));
    var ptTokens = el('div', 'form-group');
    ptTokens.appendChild(elI18n('label', 'form-label', 'login2.bearerTokens'));
    var ptTa = el('textarea', 'form-control'); ptTa.rows = 6;
    ptTa.setAttribute('data-i18n-ph', 'login2.bearerTokensPh');
    ptTa.setAttribute('placeholder', t('login2.bearerTokensPh'));
    ptTokens.appendChild(ptTa);
    pT.appendChild(ptTokens);
    var ptRegion = regionField('login2.region', 'us-east-1', 'login2.regionPh');
    pT.appendChild(ptRegion.group);
    var ptImport = elI18n('button', 'btn btn-primary', 'login2.importTokens'); ptImport.type = 'button';
    pT.appendChild(ptImport);
    panes['sso-token'] = pT; body.appendChild(pT);

    function selectTab(name) {
      tabs.querySelectorAll('.login-tab').forEach(function (b) {
        b.classList.toggle('active', b.getAttribute('data-tab') === name);
      });
      Object.keys(panes).forEach(function (k) { panes[k].classList.toggle('active', k === name); });
    }

    var m = openModal({
      title: t('login2.title'), bodyEl: body, size: 'lg',
      onClose: function () { if (pollTimer) clearTimeout(pollTimer); }
    });
    selectTab(initialTab && panes[initialTab] ? initialTab : 'builderid');

    // ---- Builder-ID device flow ----
    pbStart.addEventListener('click', function () {
      pbStart.disabled = true;
      pbResult.innerHTML = '';
      api.post('/login/builderid/start', { region: pbRegion.input.value.trim() || 'us-east-1' })
        .then(function (r) {
          pbStart.style.display = 'none';
          renderDeviceCode(pbResult, r);
          startPoll('/login/builderid/poll', r.sessionId || r.session_id, r.interval, pbResult);
        })
        .catch(function (e) { pbStart.disabled = false; api.toast(e.message || t('common.error'), 'error'); });
    });

    // ---- IAM SSO flow ----
    piGet.addEventListener('click', function () {
      piGet.disabled = true;
      piResult.innerHTML = '';
      api.post('/login/iam-sso/start', { startUrl: piStartUrl.input.value.trim(), region: piRegion.input.value.trim() || 'us-east-1' })
        .then(function (r) {
          pollSessionId = r.sessionId || r.session_id;
          var url = r.authorizeUrl || r.authorize_url;
          var note = elI18n('p', 'mng-inline-note', 'login2.authorizeOpen');
          piResult.appendChild(note);
          if (url) {
            var a = elI18n('a', 'btn btn-outline btn-sm', 'login2.openLink');
            a.href = url; a.target = '_blank'; a.rel = 'noopener';
            piResult.appendChild(a);
          }
          var cbG = el('div', 'form-group');
          cbG.appendChild(elI18n('label', 'form-label', 'login2.callbackUrl'));
          var cbInput = el('input', 'form-control'); cbInput.type = 'text';
          cbInput.setAttribute('data-i18n-ph', 'login2.callbackUrlPh');
          cbInput.setAttribute('placeholder', t('login2.callbackUrlPh'));
          cbG.appendChild(cbInput);
          cbG.appendChild(elI18n('div', 'form-hint', 'login2.callbackHint'));
          piResult.appendChild(cbG);
          var done = elI18n('button', 'btn btn-primary', 'login2.complete'); done.type = 'button';
          piResult.appendChild(done);
          if (K.i18n) K.i18n.apply(piResult);
          done.addEventListener('click', function () {
            done.disabled = true;
            api.post('/login/iam-sso/complete', { sessionId: pollSessionId, callbackUrl: cbInput.value.trim() })
              .then(function () { api.toast(t('login2.done'), 'success'); m.close(); loadAccounts(); })
              .catch(function (e) { done.disabled = false; api.toast(e.message || t('common.error'), 'error'); });
          });
        })
        .catch(function (e) { piGet.disabled = false; api.toast(e.message || t('common.error'), 'error'); });
    });

    // ---- SSO token import ----
    ptImport.addEventListener('click', function () {
      var tokens = ptTa.value.split('\n').map(function (s) { return s.trim(); }).filter(Boolean);
      if (!tokens.length) return;
      ptImport.disabled = true;
      api.post('/login/sso-token', { bearerToken: tokens.join('\n'), tokens: tokens, region: ptRegion.input.value.trim() || 'us-east-1' })
        .then(function (r) {
          r = r || {};
          var added = r.added != null ? r.added : tokens.length;
          api.toast(t('login2.addedN', { n: added }), 'success');
          m.close(); loadAccounts();
        })
        .catch(function (e) { ptImport.disabled = false; api.toast(e.message || t('common.error'), 'error'); });
    });

    function startPoll(path, sessionId, interval, mount) {
      var waitMs = (Number(interval) || 5) * 1000;
      var waitEl = el('div', 'waiting-line');
      waitEl.appendChild(el('span', 'loading-spinner'));
      waitEl.appendChild(elI18n('span', null, 'login2.waiting'));
      mount.appendChild(waitEl);
      if (K.i18n) K.i18n.apply(waitEl);
      function tick() {
        api.post(path, { sessionId: sessionId }).then(function (r) {
          r = r || {};
          var status = r.status || (r.completed ? 'completed' : 'pending');
          if (status === 'completed' || r.completed) {
            api.toast(t('login2.done'), 'success'); m.close(); loadAccounts();
            return;
          }
          if (status === 'slow_down') waitMs += 5000;
          pollTimer = setTimeout(tick, waitMs);
        }).catch(function (e) {
          waitEl.innerHTML = '';
          waitEl.appendChild(el('span', 'text-danger', e.message || t('common.error')));
        });
      }
      pollTimer = setTimeout(tick, waitMs);
    }

    function renderDeviceCode(mount, r) {
      var box = el('div', 'device-code-box');
      box.appendChild(elI18n('div', 'device-code-label', 'login2.userCode'));
      box.appendChild(el('div', 'device-code', r.userCode || r.user_code || ''));
      mount.appendChild(box);
      var uri = r.verificationUriComplete || r.verification_uri_complete || r.verificationUri || r.verification_uri;
      if (uri) {
        var a = elI18n('a', 'btn btn-outline btn-sm', 'login2.openLink');
        a.href = uri; a.target = '_blank'; a.rel = 'noopener';
        mount.appendChild(a);
      }
      if (K.i18n) K.i18n.apply(mount);
    }
  }

  // small field builders for the login modal. `phKey` (optional) wires an i18n
  // placeholder (kept in sync on langchange via data-i18n-ph) so login fields
  // carry inline guidance.
  function textField(labelKey, value, phKey) {
    var g = el('div', 'form-group');
    g.appendChild(elI18n('label', 'form-label', labelKey));
    var input = el('input', 'form-control'); input.type = 'text'; if (value) input.value = value;
    if (phKey) { input.setAttribute('data-i18n-ph', phKey); input.setAttribute('placeholder', t(phKey)); }
    g.appendChild(input);
    return { group: g, input: input };
  }
  function regionField(labelKey, def, phKey) { return textField(labelKey, def, phKey); }

  // ---------- section registration ----------
  K.sections.accounts = {
    init: function () { buildShell(); },
    onShow: function (arg) {
      if (arg && arg.focusId != null) focusId = arg.focusId;
      if (!container) buildShell();
      loadAccounts();
    },
    // 离开账号页时作废在飞的余额扇出。两个场景都需要它:
    //  ① 切到别的分区 —— 池里上千个账号,一轮串行要跑很久,人都走了还继续打上游
    //     纯属白烧配额;
    //  ② 401 掉线 —— app.js 的 onUnauthorized 会主动调当前分区的 onHide,这里
    //     必须真的把扇出停掉,否则剩余请求会一路 401、把登录浮层反复重弹并抢走
    //     输入框焦点,运维根本打不进钥匙。
    // 同时放掉 queryingInfo,免得下次进本页的自动查询被 `if (queryingInfo) return`
    // 挡住(在飞的那一发解析时会看到令牌已变,自己静默停工,不会再往下发)。
    onHide: function () {
      autoQueryToken++;
      queryingInfo = false;
    }
  };

  // Allow dashboard to jump here focused on an account.
  K.focusAccount = function (id) {
    focusId = id;
    if (K.showSection) K.showSection('accounts');
  };
})();

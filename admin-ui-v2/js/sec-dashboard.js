/* kiro2api Admin UI v2 — section-dashboard.js
   Renders the Dashboard section (<section id="sec-dashboard">).

   Registers K.sections.dashboard = { init, onShow } and also exposes
   window.renderDashboard() for the app router / manual refresh.

   Data sources (all admin, x-api-key via K.api):
     GET /api/admin/credentials  -> available / total accounts (merged card)
     GET /api/admin/usage/daily  -> today's credits + cost
     GET /api/admin/models       -> available model count (+ cached in K.models)
     GET /api/admin/server-info  -> version, kiroVersion, serverTime, os,
                                    memoryUsedBytes, memoryTotalBytes, cpuPercent,
                                    runMode, pid, uptimeSecs, masterApiKey
     GET /api/admin/config       -> host/port, region, LB mode, kiro/node/system versions
     GET /api/admin/config/load-balancing -> {mode} live rotation strategy (badge)
     GET /api/admin/credits/global -> shared-cache aggregate for global credits
                                     (read-only, zero upstream): { globalCredits,
                                     cachedCount, totalCount, oldestCacheUnix }
     GET /api/admin/credentials/{id}/balance (cold-start + manual button ONLY)
                                     -> active fan-out that seeds the SHARED
                                        BalanceCache; never on plain render

   The dashboard is intentionally an OVERVIEW: merged accounts card
   (available/total), today usage (credits + cost), global credits, model count,
   plus a system-info panel. Per-account status lives on the Accounts page and is
   NOT duplicated here.

   Safety: the section container's innerHTML is set ONCE from a STATIC template
   string (no interpolation) in init(); every dynamic value thereafter is bound
   via textContent, so untrusted API fields can never inject markup. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};

  var SEC_ID = 'sec-dashboard';
  var els = {};          // cached child refs after init
  var inited = false;

  // ---------------- helpers ----------------
  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }

  function fmtInt(n) {
    n = Number(n) || 0;
    return Math.round(n).toLocaleString();
  }
  function fmtCost(n) {
    n = Number(n) || 0;
    return '$' + n.toFixed(2);
  }
  function fmtCredits(n) {
    n = Number(n) || 0;
    // credits can be fractional; show up to 1 decimal but drop trailing .0
    return (Math.round(n * 10) / 10).toLocaleString();
  }
  // Full-precision variants for the "today" stat cards — match the Usage page,
  // which shows cost as $x.xxxx and credits to 4 decimals (no truncation to 1
  // place). Used ONLY by today-credits / today-cost; global credits keeps the
  // coarser fmtCredits above.
  function fmtCostFull(n) {
    n = Number(n) || 0;
    return '$' + n.toFixed(4);
  }
  function fmtCreditsFull(n) {
    n = Number(n) || 0;
    return n.toFixed(4);
  }

  // CST (UTC+8) day key "YYYY-MM-DD" — matches backend cst_daykey so today's
  // row in usage/daily lines up regardless of the viewer's local timezone.
  function cstToday() {
    var nowMs = Date.now();
    var cst = new Date(nowMs + 8 * 3600 * 1000);
    var y = cst.getUTCFullYear();
    var m = String(cst.getUTCMonth() + 1).padStart(2, '0');
    var d = String(cst.getUTCDate()).padStart(2, '0');
    return y + '-' + m + '-' + d;
  }

  // Human-friendly uptime from seconds, gemini2api-style: always render all four
  // components "{d}天{h}小时{m}分{s}秒" (localized units). zh units are single
  // chars glued to the number ("1天2小时"); latin suffix tokens read better
  // spaced ("1d 2h 3m 4s") — detect by unit-string length.
  function fmtUptime(secs) {
    secs = Math.max(0, Math.floor(Number(secs) || 0));
    var d = Math.floor(secs / 86400);
    var h = Math.floor((secs % 86400) / 3600);
    var m = Math.floor((secs % 3600) / 60);
    var s = secs % 60;
    var dayU = t('dash.unit.day');
    var glue = dayU.length <= 1 ? '' : ' ';
    return [
      d + dayU,
      h + t('dash.unit.hour'),
      m + t('dash.unit.minute'),
      s + t('dash.unit.second')
    ].join(glue);
  }

  // Bytes -> whole MB (integer, thousands-grouped) for "{used} MB / {total} MB".
  function bytesToMB(n) {
    n = Number(n);
    if (!isFinite(n) || n < 0) return null;
    return Math.round(n / (1024 * 1024));
  }

  // ---------------- static template (safe innerHTML, no data) ----------------
  function template() {
    return '' +
      '<div class="stats-head">' +
        '<h2 data-i18n="dash.title">Dashboard</h2>' +
        '<div class="stats-toolbar">' +
          '<button type="button" class="btn btn-outline btn-sm" id="dashRefreshBtn" data-i18n="common.refresh">Refresh</button>' +
        '</div>' +
      '</div>' +

      // ---- overview stat cards ----
      '<div class="stats-grid" id="dashStats">' +
        statCard('dashAccounts', 'dash.accounts', '👤', 'tint-green') +
        statCard('dashTodayCredits', 'dash.todayCredits', '🎟️', '') +
        statCard('dashTodayCost', 'dash.todayCost', '💵', 'tint-amber') +
        globalCreditsCard() +
        statCard('dashModelCount', 'dash.availableModels', '🧩', 'tint-blue') +
      '</div>' +

      // ---- system info panel ("系统信息 vX" card) ----
      // Faithful port of gemini2api's .system-info-panel: header = server icon +
      // "系统信息" title + version badge + 检查更新 button; then an info grid whose
      // rows/icons/order match gemini2api EXACTLY. gemini2api's 2nd row is
      // "Python版本" — here the equivalent build-toolchain row is "Rust版本":
      //   版本号(git-branch) → Rust版本(cog) → 服务器时间(clock) → 操作系统(desktop) →
      //   内存使用(memory) → CPU使用(cpu) → 运行模式(server) → 进程PID(hash)
      // = 8 fields laid out 4-per-row × 2 rows (see .dash-info-grid in CSS).
      // Uptime remains a separate live-ticking card (below), not a grid row.
      // Each .info-item lays out icon+label on top and the value below.
      '<div class="card dash-sysinfo">' +
        '<div class="card-header">' +
          // Version lives ONLY here, appended to the panel title ("系统信息 v{version}").
          '<h3>' + SVG_SERVER + ' <span data-i18n="dash.systemInfo">System Info</span>' +
            '<span class="dash-sysinfo-ver" id="dashSysinfoVer"></span></h3>' +
          // Replaces the old duplicate version badge: live rotation strategy
          // ("轮换策略: 均衡/优先级") fetched from /config/load-balancing, re-fetched
          // on every dashboard show/refresh so Settings changes reflect immediately.
          '<span class="dash-rotation-badge" id="dashRotationBadge">' +
            '<span data-i18n="dash.rotationStrategy"></span> ' +
            '<span class="dash-rotation-value" id="dashRotationValue">—</span>' +
          '</span>' +
          '<button type="button" class="btn btn-outline btn-sm dash-check-update" id="dashCheckUpdateBtn" data-i18n="dash.checkUpdate">Check Update</button>' +
        '</div>' +
        '<div class="dash-info-grid" id="dashInfoGrid">' +
          infoItem('dashVerDetail', 'dash.version', SVG_GIT_BRANCH) +
          infoItem('dashRustVersion', 'dash.rustVersion', SVG_RUST) +
          infoItem('dashServerTime', 'dash.serverTime', SVG_CLOCK) +
          infoItem('dashOs', 'dash.os', SVG_DESKTOP) +
          infoItem('dashMemory', 'dash.memory', SVG_MEMORY) +
          infoItem('dashCpu', 'dash.cpu', SVG_CPU) +
          infoItem('dashRunMode', 'dash.runMode', SVG_SERVER) +
          infoItem('dashPid', 'dash.pid', SVG_HASH) +
        '</div>' +
      '</div>' +

      // ---- uptime widget (left) + community/sponsor QR cards (right) ----
      // gemini2api-style: a dedicated uptime card that live-ticks every second,
      // sitting to the LEFT of the QR row in a single flex row.
      '<div class="dash-uptime-row">' +
        uptimeCard() +
        '<div class="dash-qr-cards" id="dashQrCards">' +
          // 初始为内置回退卡片;loadQrConfig() 会实时拉取远程配置后替换,
          // 使作者在仓库改 qr-config.json(增卡/改图文)所有部署即时反映。
          qrCard('dash.community', 'dash.communityDesc', '👥', QR_WECHAT) +
          qrCard('dash.sponsor', 'dash.sponsorDesc', '❤️', QR_SPONSOR) +
        '</div>' +
      '</div>';
  }

  // Dedicated uptime card (gemini2api .uptime-card port): clock icon + big
  // live-ticking value + label. The value is composed in JS (localized units),
  // so it starts blank ("—") and is filled once server-info seeds uptimeSecs.
  function uptimeCard() {
    return '' +
      '<div class="dash-uptime-card">' +
        '<div class="dash-uptime-icon">' + CLOCK_SVG + '</div>' +
        '<div class="dash-uptime-value" id="dashUptimeValue">—</div>' +
        '<div class="dash-uptime-label" data-i18n="dash.uptime"></div>' +
      '</div>';
  }

  // Inline clock icon (SVG, no external font dependency).
  var CLOCK_SVG = '' +
    '<svg viewBox="0 0 24 24" width="32" height="32" fill="none" ' +
      'stroke="currentColor" stroke-width="2" stroke-linecap="round" ' +
      'stroke-linejoin="round" aria-hidden="true">' +
      '<circle cx="12" cy="12" r="9"></circle>' +
      '<polyline points="12 7 12 12 15.5 14"></polyline>' +
    '</svg>';

  // ---- system-info row icons (16px, inline SVG) ----
  // kiro2api is self-contained (no FontAwesome CDN), so these hand-drawn SVGs
  // stand in for gemini2api's fa icons, one per system-info row, matching them
  // visually: fa-code-branch, fa-clock, fa-desktop, fa-memory, fa-microchip,
  // fa-server, and hashtag (fa-hashtag).
  function svgIcon(inner) {
    return '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" ' +
      'stroke="currentColor" stroke-width="2" stroke-linecap="round" ' +
      'stroke-linejoin="round" aria-hidden="true">' + inner + '</svg>';
  }
  // fa-code-branch — two nodes on a vertical line + a branch merging in
  var SVG_GIT_BRANCH = svgIcon(
    '<circle cx="6" cy="4" r="2.2"></circle>' +
    '<circle cx="6" cy="20" r="2.2"></circle>' +
    '<circle cx="18" cy="7" r="2.2"></circle>' +
    '<path d="M6 6.2v11.6"></path>' +
    '<path d="M18 9.2c0 4-4 4.8-6 6.4"></path>');
  // Rust version — a cog/gear glyph (stands in for gemini2api's "Python版本" row,
  // here the toolchain that built the binary). Outer teeth ring + inner hub.
  var SVG_RUST = svgIcon(
    '<circle cx="12" cy="12" r="3.2"></circle>' +
    '<path d="M12 2.2v2.4M12 19.4v2.4M2.2 12h2.4M19.4 12h2.4' +
      'M5.1 5.1l1.7 1.7M17.2 17.2l1.7 1.7M18.9 5.1l-1.7 1.7M6.8 17.2l-1.7 1.7"></path>');
  // fa-clock — circle + hands
  var SVG_CLOCK = svgIcon(
    '<circle cx="12" cy="12" r="9"></circle>' +
    '<polyline points="12 7 12 12 15.5 14"></polyline>');
  // fa-desktop — monitor + stand
  var SVG_DESKTOP = svgIcon(
    '<rect x="3" y="4" width="18" height="12" rx="1.5"></rect>' +
    '<path d="M9 20h6"></path><path d="M12 16v4"></path>');
  // fa-memory — RAM stick: chip body + contact pins
  var SVG_MEMORY = svgIcon(
    '<rect x="3" y="7" width="18" height="9" rx="1.2"></rect>' +
    '<path d="M8 7v4M12 7v4M16 7v4"></path>' +
    '<path d="M6 16v2M10 16v2M14 16v2M18 16v2"></path>');
  // fa-microchip — CPU: square die + pins on all sides
  var SVG_CPU = svgIcon(
    '<rect x="7" y="7" width="10" height="10" rx="1"></rect>' +
    '<path d="M10 3v2M14 3v2M10 19v2M14 19v2"></path>' +
    '<path d="M3 10h2M3 14h2M19 10h2M19 14h2"></path>');
  // fa-server — stacked rack units
  var SVG_SERVER = svgIcon(
    '<rect x="3" y="4" width="18" height="7" rx="1.5"></rect>' +
    '<rect x="3" y="13" width="18" height="7" rx="1.5"></rect>' +
    '<path d="M7 7.5h.01M7 16.5h.01"></path>');
  // fa-hashtag — process id "#"
  var SVG_HASH = svgIcon(
    '<path d="M9 3L7 21M17 3l-2 18M4 9h17M3 15h17"></path>');

  // 二维码资源远程根(项目仓库,固定)。qr-config.json + 图片都从这里实时拉取,
  // 作者改仓库配置后所有部署即时反映(与 gemini2api 一致);拉取失败则用下方内置回退卡片。
  var QR_BASE    = 'https://raw.githubusercontent.com/xwteam/gemini2api/main/api/';
  var QR_WECHAT  = QR_BASE + 'wechat-qr.png';
  var QR_SPONSOR = QR_BASE + 'sponsor-qr.png';

  // 实时拉取远程 qr-config.json,按其 cards[] 重建二维码卡片(标题/图片/说明由远程驱动)。
  // 每张卡:远程图 + 标题 + 说明;图片加载失败回退 emoji。所有文本用 textContent(防 XSS)。
  function loadQrConfig() {
    var host = document.getElementById('dashQrCards');
    if (!host || typeof fetch === 'undefined') return;
    fetch(QR_BASE + 'qr-config.json', { cache: 'no-store' })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (cfg) {
        var cards = cfg && Array.isArray(cfg.cards) ? cfg.cards : null;
        if (!cards || !cards.length) return;   // 拉取失败/为空 → 保留内置回退
        host.innerHTML = '';
        cards.forEach(function (c) {
          var img = c.image ? (/^https?:/i.test(c.image) ? c.image : QR_BASE + c.image) : '';
          var card = document.createElement('div'); card.className = 'dash-qr-card';
          var box = document.createElement('div'); box.className = 'dash-qr-box';
          if (img) {
            var im = document.createElement('img'); im.className = 'dash-qr-img';
            im.src = img; im.alt = 'QR'; im.loading = 'lazy';
            im.onerror = function () { this.style.display = 'none'; };
            box.appendChild(im);
          }
          card.appendChild(box);
          var tt = document.createElement('p'); tt.className = 'dash-qr-title';
          tt.textContent = c.title || '';                 // 远程文本,直接显示(XSS 安全)
          var dd = document.createElement('p'); dd.className = 'dash-qr-desc';
          dd.textContent = c.description || '';
          card.appendChild(tt); card.appendChild(dd);
          host.appendChild(card);
        });
      })
      .catch(function () { /* 网络失败 → 保留内置回退卡片 */ });
  }

  // 一张二维码卡片:远程图片 + 标题 + 说明。图片加载失败时回退为 emoji 占位框。
  function qrCard(titleKey, descKey, emoji, imgUrl) {
    return '' +
      '<div class="dash-qr-card">' +
        '<div class="dash-qr-box">' +
          '<img class="dash-qr-img" src="' + imgUrl + '" alt="QR" loading="lazy" ' +
            'onerror="this.style.display=\'none\';this.nextElementSibling.style.display=\'flex\';">' +
          '<span class="dash-qr-emoji" style="display:none">' + emoji + '</span>' +
        '</div>' +
        '<p class="dash-qr-title" data-i18n="' + titleKey + '"></p>' +
        '<p class="dash-qr-desc" data-i18n="' + descKey + '"></p>' +
      '</div>';
  }

  function statCard(valueId, labelKey, emoji, tint) {
    return '' +
      '<div class="stat-card">' +
        '<div class="dash-stat-icon ' + tint + '">' + emoji + '</div>' +
        '<div class="dash-stat-body">' +
          '<p class="dash-stat-value" id="' + valueId + '">—</p>' +
          '<p class="dash-stat-label" data-i18n="' + labelKey + '"></p>' +
        '</div>' +
      '</div>';
  }

  // Global credits card. Reads the SHARED balance cache via the read-only
  // aggregate endpoint (no upstream fan-out on render); the inline button forces
  // an active refresh that repopulates the shared cache. A subtle meta line under
  // the value shows the cache basis ("基于 X/N 个已缓存账号 · 更新于 X 前").
  function globalCreditsCard() {
    return '' +
      '<div class="stat-card">' +
        '<div class="dash-stat-icon">💰</div>' +
        '<div class="dash-stat-body">' +
          '<p class="dash-stat-value" id="dashGlobalCredits">—</p>' +
          '<p class="dash-stat-label">' +
            '<span data-i18n="dash.globalCredits"></span> ' +
            '<button type="button" class="btn btn-outline btn-xs dash-gc-btn" id="dashGlobalCreditsBtn" data-i18n="common.refresh">Refresh</button>' +
          '</p>' +
          '<p class="dash-gc-meta" id="dashGlobalCreditsMeta"></p>' +
        '</div>' +
      '</div>';
  }

  // One system-info row: icon+label on top (gemini2api .info-label), value below.
  // `icon` is an inline SVG string that visually matches the gemini2api fa icon.
  function infoItem(valueId, labelKey, icon) {
    return '' +
      '<div class="dash-info-item">' +
        '<span class="dash-info-label">' +
          '<span class="dash-info-icon">' + (icon || '') + '</span>' +
          '<span data-i18n="' + labelKey + '"></span>' +
        '</span>' +
        '<span class="dash-info-value" id="' + valueId + '">—</span>' +
      '</div>';
  }

  // ---------------- init (build DOM once) ----------------
  function init() {
    if (inited) return;
    var host = document.getElementById(SEC_ID);
    if (!host) return;
    host.innerHTML = template();

    els.accounts = host.querySelector('#dashAccounts');
    els.todayCredits = host.querySelector('#dashTodayCredits');
    els.todayCost = host.querySelector('#dashTodayCost');
    els.globalCredits = host.querySelector('#dashGlobalCredits');
    els.globalCreditsMeta = host.querySelector('#dashGlobalCreditsMeta');
    els.modelCount = host.querySelector('#dashModelCount');

    els.sysinfoVer = host.querySelector('#dashSysinfoVer');
    els.rotationValue = host.querySelector('#dashRotationValue');
    els.verDetail = host.querySelector('#dashVerDetail');
    els.rustVersion = host.querySelector('#dashRustVersion');
    els.serverTime = host.querySelector('#dashServerTime');
    els.os = host.querySelector('#dashOs');
    els.memory = host.querySelector('#dashMemory');
    els.cpu = host.querySelector('#dashCpu');
    els.runMode = host.querySelector('#dashRunMode');
    els.pid = host.querySelector('#dashPid');
    els.uptime = host.querySelector('#dashUptimeValue');

    var refreshBtn = host.querySelector('#dashRefreshBtn');
    if (refreshBtn) refreshBtn.addEventListener('click', function () { render(); });

    // Manual refresh: force an ACTIVE fan-out (repopulates the shared cache),
    // then re-read the aggregate. This is the only render-path that intentionally
    // hits upstream on click.
    var gcBtn = host.querySelector('#dashGlobalCreditsBtn');
    if (gcBtn) gcBtn.addEventListener('click', function () { refreshGlobalCredits(gcBtn); });

    // "Check update": query GitHub Releases via the backend (/api/admin/check-update).
    // Up to date → success toast; new release → modal with the update command + notes.
    var cuBtn = host.querySelector('#dashCheckUpdateBtn');
    if (cuBtn) cuBtn.addEventListener('click', function () { runCheckUpdate(cuBtn); });
    autoCheckUpdate();   // gemini2api 同款:加载即静默自检,有更新按钮变绿「更新到 vX」

    // re-apply i18n to the freshly injected static markup
    if (K.i18n) K.i18n.apply(host);
    // uptime string is composed in JS (localized units) — recompose on lang change;
    // 按钮的动态「更新到 vX」文案也随语言重绘
    window.addEventListener('langchange', function () { paintUptime(); paintUpdateBtn(lastUpdate); });

    inited = true;
  }

  // ---- check for updates (GitHub Releases, via backend) — gemini2api 同款流程 ----
  // 仪表盘加载即静默自检:有更新则「检查更新」按钮变实心翡翠绿、文案「更新到 vX」;
  // 点击 → 有更新弹出「更新服务」卡片(含当前语言的更新内容),否则重查 / 提示已是最新。
  function autoCheckUpdate() {
    K.api.get('/check-update').then(function (d) {
      lastUpdate = d || {};
      paintUpdateBtn(lastUpdate);
    }).catch(function () { /* 自检失败保持按钮原样,不打扰 */ });
  }

  function paintUpdateBtn(d) {
    var btn = document.getElementById('dashCheckUpdateBtn');
    if (!btn) return;
    if (d && d.hasUpdate) {
      var latest = String(d.latest || '').replace(/^v/, '');
      btn.classList.remove('btn-outline');
      btn.classList.add('btn-primary');        // 实心翡翠绿=有更新(对齐 gemini2api 的 btn-success)
      btn.removeAttribute('data-i18n');         // 文案改为动态「更新到 vX」,别被 i18n.apply 覆盖
      btn.textContent = t('dash.updateTo', { latest: latest });
    } else {
      btn.classList.remove('btn-primary');
      btn.classList.add('btn-outline');
      btn.setAttribute('data-i18n', 'dash.checkUpdate');
      btn.textContent = t('dash.checkUpdate');
    }
  }

  function runCheckUpdate(btn) {
    // 已知有更新(自检过)→ 直接弹窗,不再请求(对齐 gemini2api)。
    if (lastUpdate && lastUpdate.hasUpdate) { openUpdateModal(lastUpdate); return; }
    if (btn) btn.disabled = true;
    K.api.get('/check-update').then(function (d) {
      lastUpdate = d || {};
      paintUpdateBtn(lastUpdate);
      if (lastUpdate.hasUpdate) {
        openUpdateModal(lastUpdate);
      } else {
        var cur = String(lastUpdate.current || lastVersion || '?').replace(/^v/, '');
        K.api.toast(t('dash.upToDate', { version: cur }), 'success');
      }
    }).catch(function (e) {
      K.api.toast((e && e.message) || t('dash.checkFailed'), 'error');
    }).then(function () { if (btn) btn.disabled = false; });
  }

  function openUpdateModal(d) {
    var cur = String(d.current || lastVersion || '').replace(/^v/, '');
    var latest = String(d.latest || cur).replace(/^v/, '');
    showUpdateModal(cur, latest, 'docker compose pull && docker compose up -d', d.releaseNotes || '');
  }

  // 从发布说明正文抽出「当前 UI 语言」那一段(对齐 gemini2api 的 extractLocalizedNotes)。
  // kiro2api 发布说明按 "### <国旗> <语言名>" 分节;取当前语言那段的正文,到下一小节止,
  // 并去掉 markdown 强调符(**、`)让展示更干净。
  function extractLocalizedNotes(body) {
    if (!body) return '';
    var lang = (K.i18n && K.i18n.getLang) ? K.i18n.getLang() : 'zh-CN';
    var names = { 'zh-CN': '简体中文', 'zh-TW': '繁體中文', 'en': 'English', 'ja': '日本語', 'ko': '한국어' };
    var lines = String(body).replace(/\r/g, '').split('\n');
    function sliceByName(name) {
      var start = -1, i;
      for (i = 0; i < lines.length; i++) {
        if (/^#{1,4}\s/.test(lines[i]) && lines[i].indexOf(name) !== -1) { start = i + 1; break; }
      }
      if (start === -1) return '';
      var out = [];
      for (i = start; i < lines.length; i++) {
        if (/^#{1,4}\s/.test(lines[i])) break;   // 下一小节(其它语言或「升级」)即止
        out.push(lines[i]);
      }
      return out.join('\n').trim();
    }
    var sec = sliceByName(names[lang] || '简体中文');
    if (!sec) sec = sliceByName('English');
    if (!sec) sec = String(body).replace(/^#.*$/gm, '').replace(/```[\s\S]*?```/g, '').trim().split('\n').slice(0, 10).join('\n').trim();
    return sec.replace(/\*\*/g, '').replace(/`/g, '');   // 去掉 **粗体** 与 `代码` 标记
  }

  // gemini2api 同款「更新服务」弹窗(app.js handleCheckUpdate 1:1):
  // 图标圆圈 → 标题「更新服务 v最新」→ 可滚动的本地化更新内容框 → 命令说明 →
  // 命令行(内嵌复制按钮,复制后变对勾)→ 底部单个「取消」按钮。
  function showUpdateModal(current, latest, command, notes) {
    var cmd = command || 'docker compose pull && docker compose up -d';
    var localized = extractLocalizedNotes(notes);
    var root = document.getElementById('modalRoot') || document.body;
    var overlay = document.createElement('div'); overlay.className = 'modal-overlay active';
    var modal = document.createElement('div'); modal.className = 'modal';
    modal.style.cssText = 'max-width:460px;text-align:center;';

    var body = document.createElement('div'); body.style.cssText = 'padding:1.5rem 2rem;';

    // 图标圆圈
    var ic = document.createElement('div');
    ic.style.cssText = 'width:56px;height:56px;border-radius:50%;background:var(--primary-soft);color:var(--primary);display:inline-flex;align-items:center;justify-content:center;margin-bottom:1rem;';
    ic.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 13v8"/><path d="m8 17 4 4 4-4"/><path d="M4.393 15.269A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.436 8.284"/></svg>';
    body.appendChild(ic);

    // 标题:更新服务 v最新
    var h3 = document.createElement('h3');
    h3.textContent = t('dash.updateService') + ' v' + latest;
    h3.style.cssText = 'margin:0 0 0.5rem;font-size:1.125rem;font-weight:700;color:var(--text-primary);';
    body.appendChild(h3);

    // 本地化更新内容(可滚动框)= 核心:展示更新内容,对齐 gemini2api
    if (localized) {
      var nb = document.createElement('div');
      nb.style.cssText = 'max-height:140px;overflow-y:auto;text-align:left;padding:0.75rem;margin:0.75rem 0;background:var(--bg-tertiary);border:1px solid var(--border-color);border-radius:var(--radius-lg,10px);font-size:0.8rem;color:var(--text-secondary);line-height:1.55;white-space:pre-wrap;';
      nb.textContent = localized;
      body.appendChild(nb);
    }

    // 命令说明
    var msg = document.createElement('p');
    msg.textContent = t('dash.updateCommand');
    msg.style.cssText = 'margin:0.75rem 0 0.5rem;color:var(--text-secondary);font-size:0.85rem;';
    body.appendChild(msg);

    // 命令行 + 内嵌复制按钮(复制后图标变对勾)
    var cmdRow = document.createElement('div');
    cmdRow.style.cssText = 'display:flex;align-items:center;background:var(--bg-tertiary);border:1px solid var(--border-color);border-radius:var(--radius-lg,10px);padding:0.6rem 1rem;margin:0.5rem 0;';
    var code = document.createElement('code');
    code.textContent = cmd;
    code.style.cssText = 'flex:1;font-size:0.8rem;color:var(--text-primary);font-family:monospace;word-break:break-all;text-align:left;';
    var copyBtn = document.createElement('button');
    copyBtn.type = 'button'; copyBtn.title = t('common.copy');
    copyBtn.style.cssText = 'flex-shrink:0;margin-left:0.5rem;background:none;border:none;color:var(--text-secondary);cursor:pointer;padding:0.25rem;border-radius:4px;display:inline-flex;';
    var SVG_COPY = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>';
    var SVG_CHECK = '<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
    copyBtn.innerHTML = SVG_COPY;
    copyBtn.addEventListener('click', function () {
      var okCopy = function () {
        copyBtn.innerHTML = SVG_CHECK; copyBtn.style.color = 'var(--primary)';
        K.api.toast(t('common.copied'), 'success');
        setTimeout(function () { copyBtn.innerHTML = SVG_COPY; copyBtn.style.color = 'var(--text-secondary)'; }, 1500);
      };
      // navigator.clipboard 仅安全上下文(HTTPS/localhost)可用;纯 HTTP 下为 undefined,
      // 必须用 execCommand('copy') 兜底(与 sec-settings/sec-apikeys 一致,亦同 gemini2api)。
      function fallbackCopy() {
        var ta = document.createElement('textarea'); ta.value = cmd;
        ta.style.cssText = 'position:fixed;top:-9999px;left:-9999px;';
        document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); okCopy(); } catch (_) {}
        ta.remove();
      }
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(cmd).then(okCopy, fallbackCopy);
      } else {
        fallbackCopy();
      }
    });
    cmdRow.appendChild(code); cmdRow.appendChild(copyBtn); body.appendChild(cmdRow);
    modal.appendChild(body);

    // 底部单个「取消」按钮(对齐 gemini2api)
    var foot = document.createElement('div');
    foot.style.cssText = 'display:flex;gap:0.75rem;padding:0 2rem 1.5rem;justify-content:center;';
    var cancel = document.createElement('button');
    cancel.type = 'button'; cancel.className = 'btn btn-outline'; cancel.style.flex = '1';
    cancel.textContent = t('common.cancel');
    foot.appendChild(cancel); modal.appendChild(foot);

    overlay.appendChild(modal); root.appendChild(overlay);
    function done() { document.removeEventListener('keydown', onKey, true); overlay.remove(); }
    function onKey(e) { if (e.key === 'Escape') { e.preventDefault(); done(); } }
    document.addEventListener('keydown', onKey, true);
    cancel.addEventListener('click', done);
    overlay.addEventListener('click', function (e) { if (e.target === overlay) done(); });
  }

  // ---------------- uptime live ticker ----------------
  // gemini2api-style live counter: seed from backend uptimeSecs, then increment
  // client-side every 1s. We store the seed value plus the wall-clock instant it
  // was seeded, so each tick recomputes uptime = seed + elapsed — this stays
  // accurate even if the tab is throttled/backgrounded (no cumulative drift).
  var uptimeSeedSecs = null;   // seconds reported by backend at seed time
  var uptimeSeedAt = null;     // Date.now() when seeded
  var uptimeTimer = null;

  function currentUptimeSecs() {
    if (uptimeSeedSecs == null) return null;
    return uptimeSeedSecs + Math.floor((Date.now() - uptimeSeedAt) / 1000);
  }

  function paintUptime() {
    if (!els.uptime) return;
    var s = currentUptimeSecs();
    els.uptime.textContent = s != null ? fmtUptime(s) : '—';
  }

  function seedUptime(secs) {
    if (secs == null) {           // backend gave nothing usable
      uptimeSeedSecs = null;
      uptimeSeedAt = null;
      stopUptimeTicker();
      paintUptime();
      return;
    }
    uptimeSeedSecs = secs;
    uptimeSeedAt = Date.now();
    paintUptime();                // immediate repaint on (re)seed
    startUptimeTicker();
  }

  function startUptimeTicker() {
    if (uptimeTimer != null) return;
    uptimeTimer = setInterval(paintUptime, 1000);
  }
  function stopUptimeTicker() {
    if (uptimeTimer != null) { clearInterval(uptimeTimer); uptimeTimer = null; }
  }

  // ---------------- data render ----------------
  var lastCreds = null;
  var lastVersion = null;
  var lastUpdate = null;   // 缓存 /check-update 结果(自检+点击共用,对齐 gemini2api)

  async function render() {
    if (!inited) init();

    loadQrConfig();   // 实时拉取远程二维码配置(异步,失败保留内置回退)

    var results = await Promise.allSettled([
      K.api.get('/credentials'),
      K.api.get('/usage/daily'),
      K.api.get('/models'),
      K.api.get('/server-info'),
      K.api.get('/config')
    ]);

    var creds = results[0].status === 'fulfilled' ? results[0].value : null;
    var daily = results[1].status === 'fulfilled' ? results[1].value : null;
    var models = results[2].status === 'fulfilled' ? results[2].value : null;
    var info = results[3].status === 'fulfilled' ? results[3].value : null;
    var cfg = results[4].status === 'fulfilled' ? results[4].value : null;
    // The backend serves every system metric (serverTime/os/memory*/cpuPercent/
    // runMode/pid/uptimeSecs) from GET /api/admin/server-info — there is no
    // separate /system-info route. `sys` and `info` are the same payload; the
    // alias keeps the metric-binding code below readable.
    var sys = info;

    // ---- merged accounts card: available / total ----
    if (creds && Array.isArray(creds.credentials)) {
      lastCreds = creds;
      var total = creds.total != null ? creds.total : creds.credentials.length;
      var avail = creds.available != null
        ? creds.available
        : creds.credentials.filter(function (c) { return !c.disabled; }).length;
      els.accounts.textContent = fmtInt(avail) + '/' + fmtInt(total);
    } else {
      els.accounts.textContent = '—';
    }

    // ---- today's usage (match CST daykey row) ----
    // "/usage/daily 这一发失败了" 和 "今天确实还没有用量" 是两回事。老写法两种情况
    // 都印 0.0000 / $0.0000,而 render() 用的是 allSettled、失败连个提示都没有 ——
    // 统计后端挂掉时,运维看到的是一个和真实零流量一模一样的面板,会当成"今天没花钱"。
    // 拿不到数据一律回落 '—'(与本页账号数/模型数等其它指标的失败表现一致),
    // 只有确实收到了(数组形态的)日汇总才允许把没有今日行解释成 0。
    var dailyOk = Array.isArray(daily);
    var today = todayRow(daily);
    els.todayCredits.textContent = dailyOk ? fmtCreditsFull(today ? today.totalCredits : 0) : '—';
    els.todayCost.textContent = dailyOk ? fmtCostFull(today ? today.totalCost : 0) : '—';

    // ---- models ----
    if (models && Array.isArray(models.data)) {
      K.models = models.data;   // cache for other sections
      els.modelCount.textContent = fmtInt(models.data.length);
    } else {
      els.modelCount.textContent = '—';
    }

    // ---- system info (server-info + config) ----
    renderSystemInfo(sys, info, cfg);

    // ---- rotation strategy (live) + global credits (cache-first) ----
    // Both re-run on every render (dashboard show + manual refresh). Rotation
    // strategy is a single cheap GET. Global credits is CACHE-FIRST: it only
    // reads the shared BalanceCache aggregate (zero upstream) — the heavy
    // per-account balance fan-out runs ONLY on cold start (empty cache) or the
    // manual button, never on every render (see loadGlobalCredits).
    loadRotationStrategy();
    loadGlobalCredits();
  }

  function todayRow(daily) {
    if (!Array.isArray(daily) || !daily.length) return null;
    var key = cstToday();
    for (var i = 0; i < daily.length; i++) {
      if (daily[i] && daily[i].date === key) return daily[i];
    }
    return null;   // no usage recorded today yet
  }

  function renderSystemInfo(sys, info, cfg) {
    // ---- version: shown ONLY in the panel title ("系统信息 v{version}") and the
    // first 版本号 grid row (gemini2api parity). No separate version badge — that
    // slot now holds the live rotation strategy. lastVersion also feeds the
    // "check update" toast. ----
    var ver = (sys && sys.version) ? sys.version
            : (info && info.version) ? info.version : null;
    lastVersion = ver;
    var verStr = ver ? ('v' + String(ver).replace(/^v/, '')) : '—';
    if (els.sysinfoVer) els.sysinfoVer.textContent = ver ? (' ' + verStr) : '';
    if (els.verDetail) els.verDetail.textContent = verStr;

    // ---- Rust toolchain version (build-time rustc --version string) ----
    if (els.rustVersion) {
      els.rustVersion.textContent = (sys && sys.rustVersion) ? sys.rustVersion : '—';
    }

    // ---- server time (backend-formatted string) ----
    els.serverTime.textContent = (sys && sys.serverTime) ? sys.serverTime : '—';

    // ---- operating system ----
    els.os.textContent = (sys && sys.os) ? sys.os : '—';

    // ---- memory: "{used} MB / {total} MB" ----
    var usedMB = sys ? bytesToMB(sys.memoryUsedBytes) : null;
    var totalMB = sys ? bytesToMB(sys.memoryTotalBytes) : null;
    if (usedMB != null && totalMB != null) {
      els.memory.textContent = usedMB.toLocaleString() + ' MB / ' + totalMB.toLocaleString() + ' MB';
    } else if (usedMB != null) {
      els.memory.textContent = usedMB.toLocaleString() + ' MB';
    } else {
      els.memory.textContent = '—';
    }

    // ---- CPU: "{cpuPercent}%" ----
    var cpu = (sys && typeof sys.cpuPercent === 'number' && isFinite(sys.cpuPercent))
      ? sys.cpuPercent : null;
    els.cpu.textContent = cpu != null ? (Math.round(cpu * 10) / 10) + '%' : '—';

    // ---- run mode: Docker / Bare (localized), else echo raw runMode ----
    var mode = (sys && sys.runMode) ? String(sys.runMode) : null;
    if (mode) {
      var mk = /docker/i.test(mode) ? 'dash.mode.docker'
             : /bare/i.test(mode)   ? 'dash.mode.bare' : null;
      els.runMode.textContent = mk ? t(mk) : mode;
    } else {
      els.runMode.textContent = '—';
    }

    // ---- process PID ----
    els.pid.textContent = (sys && sys.pid != null) ? String(sys.pid) : '—';

    // ---- uptime: seed the dedicated live-ticking card (no longer a grid row).
    // /server-info.uptimeSecs preferred; probe legacy field names too. ----
    seedUptime(pickUptimeSeconds(sys, info, cfg));
  }

  // Uptime may arrive under a few possible field names depending on backend
  // build; probe the common ones (system-info first) and return seconds, else null.
  function pickUptimeSeconds(sys, info, cfg) {
    var candidates = [
      sys && sys.uptimeSecs, sys && sys.uptimeSeconds, sys && sys.uptime_seconds,
      info && info.uptimeSeconds, info && info.uptime_seconds, info && info.uptime,
      cfg && cfg.uptimeSeconds, cfg && cfg.uptime_seconds, cfg && cfg.uptime
    ];
    for (var i = 0; i < candidates.length; i++) {
      var v = candidates[i];
      if (typeof v === 'number' && isFinite(v) && v >= 0) return v;
    }
    return null;
  }

  // Rotation strategy (live): single GET /config/load-balancing → {mode}.
  // mode ∈ {priority, balanced}; render the localized label next to the panel
  // title. Re-fetched on every render (dashboard show + manual refresh) so a
  // change made in Settings reflects here in real time.
  async function loadRotationStrategy() {
    if (!els.rotationValue) return;
    var lb = null;
    try { lb = await K.api.get('/config/load-balancing'); } catch (e) { lb = null; }
    var mode = (lb && typeof lb.mode === 'string') ? lb.mode.toLowerCase() : null;
    var key = mode === 'balanced' ? 'dash.rot.balanced'
            : mode === 'priority' ? 'dash.rot.priority' : null;
    els.rotationValue.textContent = key ? t(key) : (mode || '—');
  }

  // Global credits — SHARED, CACHE-FIRST.
  //
  // The dashboard and the Accounts page share ONE balance cache on the backend
  // (BalanceCache). Whoever runs an active balance query (Accounts auto-queries
  // on open; the dashboard on cold-start or manual refresh) populates it; the
  // other side just reads it. So the default render path here is a single
  // READ-ONLY aggregate GET /api/admin/credits/global that NEVER hits upstream:
  //   { globalCredits, cachedCount, totalCount, oldestCacheUnix }
  //
  // Cold start ONLY: if the cache is empty (cachedCount == 0 — e.g. the user
  // landed on the dashboard before ever opening Accounts), we trigger ONE active
  // fan-out to seed the shared cache, then re-read the aggregate. This avoids a
  // blank global-credits on a fresh start while still populating the cache for
  // the Accounts page.
  //
  // gcInFlight coalesces overlapping calls so a cold-start/manual fan-out can't
  // be doubled by a concurrent render.
  var gcInFlight = false;
  var gcColdStartDone = false;   // guard: only auto-fan-out once per empty cache

  // 余额扇出的并发上限。
  //
  // 为什么必须有上限:冷启动(容器刚重启,共享 BalanceCache 是空的)会**无人点击**
  // 就自动扇出一次,给池内每个启用账号打一发 GET /credentials/{id}/balance;缓存
  // miss 的那一发在后端直通上游(credential_balance → fetch_usage_limits_fresh →
  // ensure_fresh + getUsageLimits),还可能顺带刷新令牌。生产池有上千个账号,老写法
  // 的 Promise.allSettled(全量 map) 等于**一瞬间对 AWS 打出上千并发**,极易撞限流/
  // 风控——这正是账号页那套"一次只发一个"的扇出刻意要避开的事。
  //
  // 为什么取一个小常数而不是照账号页完全串行:仪表盘这一轮是渲染时自动触发的,
  // 串行跑上千个账号要十几分钟,「全局积分」会长时间停在 …;并发 4 对上游依旧是
  // 可忽略的压力(与人手点几下同量级),却快 4 倍。改这个常数即可整体调松/调紧。
  var GC_FANOUT_CONCURRENCY = 4;

  // 扇出令牌:自增即作废在飞的那一轮(切走仪表盘 / 401 掉线 / 又点了一次刷新)。
  var gcFanoutToken = 0;

  // 401/403 = admin key 已失效:api.js 收到这两个状态码时已经清掉本地钥匙并弹出
  // 登录浮层了,这里只需要认出它。
  function isAuthError(e) { return !!e && (e.status === 401 || e.status === 403); }

  // Cache-first read used on every render. Reads the aggregate; on a cold start
  // (empty cache) does exactly one active refresh to seed the shared cache.
  async function loadGlobalCredits() {
    if (gcInFlight) return;
    var agg = await readGlobalAggregate();

    // Cold start: no cached account yet → seed the shared cache once, then
    // re-read. Any subsequent render just reads (no fan-out).
    if (agg && Number(agg.cachedCount) === 0 && !gcColdStartDone) {
      gcColdStartDone = true;
      await refreshGlobalCredits(null);   // active fan-out + re-read
      return;
    }
    if (agg && Number(agg.cachedCount) > 0) gcColdStartDone = true;
    paintGlobalCredits(agg);
  }

  // Manual refresh (button) + cold-start seed: ACTIVE per-account balance
  // fan-out. This is the only path that hits upstream. It populates the shared
  // BalanceCache; we then re-read the aggregate so the displayed number + meta
  // come from the same shared source the Accounts page sees.
  //
  // 并发受 GC_FANOUT_CONCURRENCY 限制、可被令牌取消、撞 401/403 立刻整轮停,
  // 细节见 fanoutBalances。
  async function refreshGlobalCredits(btn) {
    if (gcInFlight) return;
    if (!lastCreds || !Array.isArray(lastCreds.credentials)) {
      paintGlobalCredits(await readGlobalAggregate());
      return;
    }
    var enabled = lastCreds.credentials.filter(function (c) { return !c.disabled; });

    var token = ++gcFanoutToken;
    gcInFlight = true;
    if (btn) btn.disabled = true;
    if (els.globalCredits) els.globalCredits.textContent = '…';

    try {
      var res = enabled.length
        ? await fanoutBalances(enabled, token)
        : { aborted: false, cancelled: false };

      // 本轮已被作废(切走了仪表盘 / 又点了一次刷新):什么都别画,画了也是覆盖新一轮。
      // 同时把冷启动闸放回去 —— 这一轮没跑完,不该算"种子已播过";否则用户刚进仪表盘
      // 就切走、回来后「全局积分」会一直空着,除非手点刷新。(真跑完但全军覆没的情况
      // 走的是下面的正常分支,闸保持关闭,不会每次渲染都重扇一遍。)
      if (res.cancelled || token !== gcFanoutToken) { gcColdStartDone = false; return; }

      if (res.aborted) {
        // 撞 401/403 中止:钥匙已被 api.js 清掉,这时**绝不能**再去读聚合端点——
        // 那一发同样会 401,又触发一次登录浮层重弹(清空并抢占输入框焦点)。
        // 同时放开冷启动闸:本轮没能把共享缓存喂上,重新登录后的第一次渲染
        // 还得允许再补一次种子,否则「全局积分」会一直空到有人手点刷新。
        gcColdStartDone = false;
        if (els.globalCredits) els.globalCredits.textContent = '—';
        return;
      }

      // Re-read the shared aggregate now that the cache is populated.
      paintGlobalCredits(await readGlobalAggregate());
    } finally {
      gcInFlight = false;
      if (btn) btn.disabled = false;
    }
  }

  // 有界 + 可取消的余额扇出:最多 GC_FANOUT_CONCURRENCY 发同时在飞,完成一发才补
  // 下一发(工作者池,不是一次性 map 出上千个 Promise)。返回 { aborted, cancelled }:
  //   aborted   — 撞到 401/403,剩下的账号一发都不再打;
  //   cancelled — 令牌被顶掉(离开仪表盘 / 新一轮刷新),静默停工。
  //
  // 为什么"撞 401 就必须整轮停":api.js 每收到一个 401 都会清本地 key 并调
  // onUnauthorized 重弹登录浮层,而浮层每次弹出都会清空并抢占登录输入框的焦点。
  // 若照旧把剩余上千个请求发完,它们会在几秒内**全部**401,登录框被反复清空/抢焦点,
  // 运维根本打不进钥匙、进不来后台。
  async function fanoutBalances(list, token) {
    var i = 0;   // 共享游标:每个工作者取一个账号,取完即止
    var state = { aborted: false, cancelled: false };

    // 用 while + await 领取下一个账号(而不是 .then(worker) 递归):上千个账号
    // 递归链会一路挂着上千层 promise,循环则是常数内存。
    async function worker() {
      while (true) {
        if (state.aborted) return;
        if (token !== gcFanoutToken) { state.cancelled = true; return; }
        if (i >= list.length) return;
        var c = list[i++];
        try {
          await K.api.get('/credentials/' + encodeURIComponent(c.id) + '/balance');
        } catch (e) {
          // 单个账号 502 / 网络错只是跳过:聚合端点本来就会略过没有新鲜缓存的账号。
          // 只有鉴权失败才意味着"这一轮再打下去毫无意义且有害"。
          if (isAuthError(e)) { state.aborted = true; return; }
        }
      }
    }

    var n = Math.min(GC_FANOUT_CONCURRENCY, list.length);
    var running = [];
    for (var k = 0; k < n; k++) running.push(worker());
    await Promise.all(running);
    return state;
  }

  // READ-ONLY aggregate over the shared cache. Does NOT hit upstream.
  async function readGlobalAggregate() {
    try { return await K.api.get('/credits/global'); }
    catch (e) { return null; }
  }

  // Paint the value + subtle cache-basis meta from an aggregate payload.
  function paintGlobalCredits(agg) {
    if (!agg || typeof agg.globalCredits !== 'number') {
      els.globalCredits.textContent = '—';
      if (els.globalCreditsMeta) els.globalCreditsMeta.textContent = '';
      return;
    }
    els.globalCredits.textContent = fmtCredits(agg.globalCredits);

    if (!els.globalCreditsMeta) return;
    var cached = Number(agg.cachedCount) || 0;
    var total = Number(agg.totalCount) || 0;
    var meta = t('dash.gcCacheBasis', { cached: cached, total: total });
    var ago = fmtCacheAge(agg.oldestCacheUnix);
    if (ago) meta += ' · ' + t('dash.gcUpdatedAgo', { ago: ago });
    els.globalCreditsMeta.textContent = meta;
  }

  // Relative age of the oldest cache entry (unix seconds) → localized "X 前"
  // fragment, or '' when absent/invalid. Coarse buckets: s / m / h / d.
  function fmtCacheAge(oldestUnix) {
    var u = Number(oldestUnix);
    if (!isFinite(u) || u <= 0) return '';
    var sec = Math.floor(Date.now() / 1000 - u);
    if (sec < 0) sec = 0;
    if (sec < 60)    return t('dash.gcAgeSeconds', { n: sec });
    if (sec < 3600)  return t('dash.gcAgeMinutes', { n: Math.floor(sec / 60) });
    if (sec < 86400) return t('dash.gcAgeHours',   { n: Math.floor(sec / 3600) });
    return t('dash.gcAgeDays', { n: Math.floor(sec / 86400) });
  }

  // ---------------- register + expose ----------------
  K.sections.dashboard = {
    init: init,
    onShow: function () { render(); },
    // 离开仪表盘时作废在飞的余额扇出。两个场景都需要它:
    //  ① 切到账号页 —— 账号页自己也会串行扇出一遍同一批账号(共享同一份
    //     BalanceCache),放任仪表盘那轮继续跑就是双份上游压力 + 重复劳动;
    //  ② 401 掉线 —— app.js 的 onUnauthorized 会主动调当前分区的 onHide,
    //     这里停掉后台请求,登录浮层才不会被后续 401 反复重弹、抢走输入焦点。
    onHide: function () { gcFanoutToken++; }
  };

  window.renderDashboard = function () {
    if (!inited) init();
    return render();
  };
})();

/* kiro2api Admin UI v2 — sec-usage.js
   Renders the Usage Stats section (<section id="sec-usage">), restyled to
   faithfully match the reference usage-stats layout (summary stat cards +
   chart panel with range controls + daily table + drill-down).

   Registers K.sections.usage = { init, onShow, onHide } and exposes
   window.renderUsageStats() for the app router / manual refresh.

   Data sources (admin, x-api-key via K.api):
     GET /api/admin/usage/summary?range=<6h|24h|3d|7d|30d>  -> windowed aggregate
         totals + ascending time-bucketed `series` for the chart.
     GET /api/admin/usage/daily                             -> daily rollup (date desc)
     GET /api/admin/usage/daily/{date}/records?page&page_size -> per-day records (drill)

   The summary endpoint drives the top stat cards + chart for the selected
   range (6小时/24小时/3天/7天/30天, matching the reference panel). The daily rollup
   still powers the daily-breakdown table + drill-down below.

   Precision: totals are shown with the reference panel precision (cost & credits to 4
   decimals; requests as grouped integers) — dynamic values are NOT over-rounded.

   The chart is a hand-rolled inline SVG (same-origin, no external lib),
   recolored on 'themechange' by re-render.

   i18n: the static template uses data-i18n keys filled by K.i18n.apply(host);
   dynamic markup built via createElement uses textContent = t(key). No visible
   string is hardcoded.

   Safety: the section container's innerHTML is set ONCE from a STATIC template
   in init(); the SVG chart is built with createElementNS + textContent, and the
   tables are built with textContent per cell, so untrusted fields (model names,
   client IPs) can never inject markup. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};
  var SVGNS = 'http://www.w3.org/2000/svg';

  var SEC_ID = 'sec-usage';
  var els = {};
  var inited = false;

  // The five ranges match the reference panel's selector, bound 1:1 to the backend's
  // `range` query param values.
  var RANGES = ['6h', '24h', '3d', '7d', '30d'];
  var RANGE_I18N = {
    '6h': 'us.range6h',
    '24h': 'us.range24h',
    '3d': 'us.range3d',
    '7d': 'us.range7d',
    '30d': 'us.range30d'
  };

  var state = {
    range: '24h',         // one of RANGES
    summary: null,        // last /usage/summary response for the active range
    metric: 'requests',   // 'requests' | 'credits' | 'cost'
    reqId: 0,             // guards against out-of-order summary responses
    daily: [],            // full rollup from /usage/daily (date desc)
    drillDate: null,      // currently expanded day, or null
    drillPage: 1,
    drillPageSize: 20
  };

  // ---------------- helpers ----------------
  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }
  function fmtInt(n) { return (Math.round(Number(n) || 0)).toLocaleString(); }
  // Cost/credits shown with the reference panel precision (4 decimals; not over-rounded).
  function fmtCost(n) { return '$' + (Number(n) || 0).toFixed(4); }
  function fmtCredits(n) { return (Number(n) || 0).toFixed(4); }
  // errorRate / rotationSuccessRate arrive as [0,1] floats -> percent (1 decimal).
  function fmtPct(n) { return ((Number(n) || 0) * 100).toFixed(1) + '%'; }
  // avgLatencyMs arrives in ms; show grouped integer ms.
  function fmtLatency(n) { return (Math.round(Number(n) || 0)).toLocaleString() + ' ms'; }
  function fmtDateTime(s) { if (!s) return '—'; var d = new Date(s); return isNaN(d) ? String(s) : d.toLocaleString(); }

  // Chart series accessors — the summary series carries totalRequests /
  // totalCost / totalCredits per bucket (bucketStartUnix ascending).
  function seriesArr() {
    return (state.summary && Array.isArray(state.summary.series)) ? state.summary.series : [];
  }
  function metricValue(row) {
    if (state.metric === 'cost') return Number(row.totalCost) || 0;
    if (state.metric === 'credits') return Number(row.totalCredits) || 0;
    return Number(row.totalRequests) || 0;
  }

  // ---------------- static template (reference-style) ----------------
  function template() {
    return '' +
      '<div class="section-header">' +
        '<h2 class="section-title" data-i18n="us.title">Usage Statistics</h2>' +
      '</div>' +

      // ---- summary stat cards (reference layout .stats-grid / .stat-card) ----
      '<div class="stats-grid usage-summary-grid" id="usSummary">' +
        statCard('usSumRequests', 'us.totalRequests', '📨', 'primary') +
        statCard('usSumCost', 'us.totalCost', '💵', 'warning') +
        statCard('usSumCredits', 'us.totalCredits', '🎟️', 'info') +
        statCard('usSumErrorRate', 'us.errorRate', '⚠️', 'danger') +
        statCard('usSumLatency', 'us.avgLatency', '⏱️', 'info') +
        statCard('usSumRotation', 'us.rotationSuccessRate', '🔄', 'success') +
      '</div>' +

      // ---- chart panel (reference layout .usage-chart-panel) ----
      '<div class="usage-chart-panel">' +
        '<div class="chart-controls">' +
          '<div class="control-group">' +
            '<label data-i18n="us.metric">Metric:</label>' +
            '<div class="btn-group" id="usMetric">' +
              '<button type="button" class="btn btn-sm btn-outline active" data-metric="requests" data-i18n="us.chartRequests">Requests</button>' +
              '<button type="button" class="btn btn-sm btn-outline" data-metric="credits" data-i18n="us.chartCredits">Credits</button>' +
              '<button type="button" class="btn btn-sm btn-outline" data-metric="cost" data-i18n="us.chartCost">Cost</button>' +
            '</div>' +
          '</div>' +
          '<div class="control-group">' +
            '<label data-i18n="us.timeRange">Time range:</label>' +
            '<div class="btn-group" id="usRange">' +
              '<button type="button" class="btn btn-sm btn-outline" data-range="6h" data-i18n="us.range6h">6h</button>' +
              '<button type="button" class="btn btn-sm btn-outline active" data-range="24h" data-i18n="us.range24h">24h</button>' +
              '<button type="button" class="btn btn-sm btn-outline" data-range="3d" data-i18n="us.range3d">3d</button>' +
              '<button type="button" class="btn btn-sm btn-outline" data-range="7d" data-i18n="us.range7d">7d</button>' +
              '<button type="button" class="btn btn-sm btn-outline" data-range="30d" data-i18n="us.range30d">30d</button>' +
            '</div>' +
          '</div>' +
          '<button type="button" class="btn btn-sm btn-outline" id="usRefreshBtn">' +
            '<span data-i18n="common.refresh">Refresh</span>' +
          '</button>' +
        '</div>' +

        // long-window daily-fallback caveat (hidden unless the backend flags it)
        '<div class="usage-fallback-note" id="usFallbackNote" hidden>' +
          '<span data-i18n="us.fallbackNote">Token totals for long ranges reflect surviving raw records only.</span>' +
        '</div>' +

        // SVG chart container
        '<div class="chart-container" id="usChart">' +
          '<div class="chart-loading"><span class="stats-spinner"></span><span data-i18n="common.loading">Loading…</span></div>' +
        '</div>' +
      '</div>' +

      // ---- daily breakdown table (reference-styled) ----
      '<div class="usage-table-panel">' +
        '<h3 class="usage-panel-title" data-i18n="us.dailyTable">Daily Breakdown</h3>' +
        '<div class="usage-table-wrap">' +
          '<table class="data-table usage-daily-table">' +
            '<thead><tr>' +
              '<th data-i18n="us.colDate">Date</th>' +
              '<th class="num" data-i18n="us.colRequests">Requests</th>' +
              '<th class="num" data-i18n="us.colCost">Cost (USD)</th>' +
              '<th class="num" data-i18n="us.colCredits">Credits</th>' +
              '<th class="drill-cell" data-i18n="common.actions">Actions</th>' +
            '</tr></thead>' +
            '<tbody id="usDailyBody">' +
              '<tr><td colspan="5"><div class="chart-loading"><span class="stats-spinner"></span><span data-i18n="common.loading">Loading…</span></div></td></tr>' +
            '</tbody>' +
          '</table>' +
        '</div>' +
        '<div class="usage-drill" id="usDrill" hidden></div>' +
      '</div>' +

      '<div class="usage-tooltip" id="usTooltip"></div>';
  }

  function statCard(valueId, labelKey, emoji, tint) {
    return '' +
      '<div class="stat-card">' +
        '<div class="stat-icon ' + tint + '">' + emoji + '</div>' +
        '<div class="stat-info">' +
          '<h3 id="' + valueId + '">—</h3>' +
          '<p data-i18n="' + labelKey + '"></p>' +
        '</div>' +
      '</div>';
  }

  // ---------------- init ----------------
  function init() {
    if (inited) return;
    var host = document.getElementById(SEC_ID);
    if (!host) return;
    host.innerHTML = template();

    els.sumRequests = host.querySelector('#usSumRequests');
    els.sumCost = host.querySelector('#usSumCost');
    els.sumCredits = host.querySelector('#usSumCredits');
    els.sumErrorRate = host.querySelector('#usSumErrorRate');
    els.sumLatency = host.querySelector('#usSumLatency');
    els.sumRotation = host.querySelector('#usSumRotation');
    els.chart = host.querySelector('#usChart');
    els.fallbackNote = host.querySelector('#usFallbackNote');
    els.dailyBody = host.querySelector('#usDailyBody');
    els.drill = host.querySelector('#usDrill');
    els.tooltip = host.querySelector('#usTooltip');
    els.rangeGroup = host.querySelector('#usRange');
    els.metricGroup = host.querySelector('#usMetric');

    // range buttons -> refetch the summary for the selected window
    els.rangeGroup.addEventListener('click', function (e) {
      var b = e.target.closest('button[data-range]'); if (!b) return;
      var v = b.getAttribute('data-range');
      if (RANGES.indexOf(v) < 0 || v === state.range) return;
      state.range = v;
      paintActive();
      loadSummary();
    });
    // metric buttons -> re-render chart from the cached summary series
    els.metricGroup.addEventListener('click', function (e) {
      var b = e.target.closest('button[data-metric]'); if (!b) return;
      state.metric = b.getAttribute('data-metric');
      paintActive();
      renderChart();
    });
    var refreshBtn = host.querySelector('#usRefreshBtn');
    if (refreshBtn) refreshBtn.addEventListener('click', function () { load(); });

    if (K.i18n) K.i18n.apply(host);
    // recolor chart / re-localize dynamic rows on theme/lang change
    window.addEventListener('themechange', function () { if (state.summary) renderChart(); });
    window.addEventListener('langchange', function () {
      if (state.summary) { renderSummary(); renderChart(); }
      if (state.daily.length) renderDaily();
      if (state.drillDate) loadDrill(state.drillDate, state.drillPage);
    });

    paintActive();
    inited = true;
  }

  function paintActive() {
    els.rangeGroup.querySelectorAll('button[data-range]').forEach(function (b) {
      b.classList.toggle('active', b.getAttribute('data-range') === state.range);
    });
    els.metricGroup.querySelectorAll('button[data-metric]').forEach(function (b) {
      b.classList.toggle('active', b.getAttribute('data-metric') === state.metric);
    });
  }

  // ---------------- load ----------------
  // Full refresh: summary (cards + chart) and the daily table run in parallel.
  function load() {
    if (!inited) init();
    return Promise.all([loadSummary(), loadDaily()]);
  }

  // ---- windowed summary (cards + chart) ----
  async function loadSummary() {
    setChartLoading();
    els.sumRequests.textContent = '—';
    els.sumCost.textContent = '—';
    els.sumCredits.textContent = '—';
    if (els.sumErrorRate) els.sumErrorRate.textContent = '—';
    if (els.sumLatency) els.sumLatency.textContent = '—';
    if (els.sumRotation) els.sumRotation.textContent = '—';
    var myId = ++state.reqId;
    var data;
    try {
      data = await K.api.get('/usage/summary', { range: state.range });
    } catch (e) {
      if (myId !== state.reqId) return;
      state.summary = null;
      renderSummaryError();
      return;
    }
    if (myId !== state.reqId) return;   // a newer range click superseded us
    state.summary = data || null;
    renderSummary();
    renderChart();
  }

  function renderSummaryError() {
    els.sumRequests.textContent = '—';
    els.sumCost.textContent = fmtCost(0);
    els.sumCredits.textContent = fmtCredits(0);
    if (els.sumErrorRate) els.sumErrorRate.textContent = '—';
    if (els.sumLatency) els.sumLatency.textContent = '—';
    if (els.sumRotation) els.sumRotation.textContent = '—';
    if (els.fallbackNote) els.fallbackNote.hidden = true;
    setChartEmpty();
  }

  // ---- daily rollup (table + drill) ----
  async function loadDaily() {
    var daily;
    try {
      daily = await K.api.get('/usage/daily');
    } catch (e) {
      state.daily = [];
      renderDaily();
      return;
    }
    state.daily = Array.isArray(daily) ? daily : [];
    renderDaily();
  }

  // ---------------- summary cards ----------------
  function renderSummary() {
    var s = state.summary || {};
    els.sumRequests.textContent = fmtInt(s.totalRequests);
    els.sumCost.textContent = fmtCost(s.totalCost);
    els.sumCredits.textContent = fmtCredits(s.totalCredits);
    // new reliability metrics (backend GET /usage/summary):
    //   errorRate, rotationSuccessRate ∈ [0,1] -> percent;  avgLatencyMs -> ms.
    if (els.sumErrorRate) els.sumErrorRate.textContent = fmtPct(s.errorRate);
    if (els.sumLatency) els.sumLatency.textContent = fmtLatency(s.avgLatencyMs);
    if (els.sumRotation) els.sumRotation.textContent = fmtPct(s.rotationSuccessRate);
    if (els.fallbackNote) els.fallbackNote.hidden = !s.dailyFallbackApplied;
  }

  // ---------------- chart (hand-rolled inline SVG, reference layout style) ----------------
  function setChartLoading() {
    els.chart.innerHTML =
      '<div class="chart-loading"><span class="stats-spinner"></span><span>' + escapeText(t('common.loading')) + '</span></div>';
  }
  function setChartEmpty() {
    els.chart.innerHTML = '';
    els.chart.appendChild(emptyChart('us.noData'));
  }
  // reference-style empty placeholder: big glyph + caption
  function emptyChart(msgKey) {
    var box = document.createElement('div');
    box.className = 'empty-chart';
    var ic = document.createElement('span');
    ic.className = 'empty-chart-icon';
    ic.textContent = '📊';
    var p = document.createElement('p');
    p.textContent = t(msgKey);
    box.appendChild(ic);
    box.appendChild(p);
    return box;
  }
  function escapeText(s) { var d = document.createElement('div'); d.textContent = s; return d.innerHTML; }

  function renderChart() {
    var rows = seriesArr();
    if (!rows.length) { setChartEmpty(); return; }

    // viewBox geometry (responsive; scales to container width) — mirrors reference layout
    var W = 720, H = 320;
    var padL = 55, padR = 24, padT = 24, padB = 40;
    var plotW = W - padL - padR;
    var plotH = H - padT - padB;

    var maxV = 0;
    rows.forEach(function (r) { maxV = Math.max(maxV, metricValue(r)); });
    if (maxV <= 0) maxV = 1;
    var ceil = niceCeil(maxV);

    var n = rows.length;
    var slot = plotW / n;
    var barW = Math.max(2, Math.min(28, slot * 0.7));

    var bucketSecs = Number(state.summary && state.summary.bucketSecs) || 3600;

    var svg = document.createElementNS(SVGNS, 'svg');
    svg.setAttribute('viewBox', '0 0 ' + W + ' ' + H);
    svg.setAttribute('preserveAspectRatio', 'xMidYMid meet');
    svg.setAttribute('role', 'img');

    // y gridlines + labels (4 steps), dashed like reference layout
    var steps = 4;
    for (var g = 0; g <= steps; g++) {
      var yv = ceil * (g / steps);
      var y = padT + plotH - (yv / ceil) * plotH;
      var line = document.createElementNS(SVGNS, 'line');
      line.setAttribute('class', 'usage-chart-gridline');
      line.setAttribute('x1', padL); line.setAttribute('x2', W - padR);
      line.setAttribute('y1', y); line.setAttribute('y2', y);
      svg.appendChild(line);

      var yl = document.createElementNS(SVGNS, 'text');
      yl.setAttribute('class', 'usage-chart-ylabel');
      yl.setAttribute('x', padL - 8); yl.setAttribute('y', y + 4);
      yl.setAttribute('text-anchor', 'end');
      yl.textContent = shortNum(yv);
      svg.appendChild(yl);
    }

    // bars (reference layout uses primary fill, opacity 0.7, rx 2)
    var labelEvery = Math.max(1, Math.ceil(n / 8));

    rows.forEach(function (r, i) {
      var v = metricValue(r);
      var h = (v / ceil) * plotH;
      var x = padL + slot * i + (slot - barW) / 2;
      var y = padT + plotH - h;

      var rect = document.createElementNS(SVGNS, 'rect');
      rect.setAttribute('class', 'usage-chart-bar');
      rect.setAttribute('x', x); rect.setAttribute('y', y);
      rect.setAttribute('width', barW); rect.setAttribute('height', Math.max(0, h));
      rect.setAttribute('rx', 2);

      rect.addEventListener('mousemove', function (ev) { showTip(ev, r, bucketSecs); });
      rect.addEventListener('mouseleave', hideTip);

      svg.appendChild(rect);

      if (i % labelEvery === 0 || i === n - 1) {
        var xl = document.createElementNS(SVGNS, 'text');
        xl.setAttribute('class', 'usage-chart-label');
        xl.setAttribute('x', x + barW / 2);
        xl.setAttribute('y', padT + plotH + 16);
        xl.setAttribute('text-anchor', 'middle');
        xl.textContent = bucketLabel(r.bucketStartUnix, bucketSecs);
        svg.appendChild(xl);
      }
    });

    els.chart.innerHTML = '';
    els.chart.appendChild(svg);
  }

  function niceCeil(v) {
    if (v <= 0) return 1;
    var mag = Math.pow(10, Math.floor(Math.log10(v)));
    var norm = v / mag;
    var nice = norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10;
    return nice * mag;
  }
  function shortNum(v) {
    if (state.metric === 'cost') return '$' + (v >= 100 ? Math.round(v) : (Math.round(v * 100) / 100));
    if (v >= 1000) return (Math.round(v / 100) / 10) + 'k';
    return String(Math.round(v * 10) / 10);
  }
  // x-axis tick label from a bucket-start unix ts. Per-hour buckets show HH:00,
  // per-day buckets show MM-DD (local time).
  function bucketLabel(unix, bucketSecs) {
    var d = new Date((Number(unix) || 0) * 1000);
    if (isNaN(d)) return '';
    if (bucketSecs >= 86400) {
      return pad2(d.getMonth() + 1) + '-' + pad2(d.getDate());
    }
    return pad2(d.getHours()) + ':' + pad2(d.getMinutes());
  }
  // full bucket timestamp for the tooltip header
  function bucketFull(unix) {
    var d = new Date((Number(unix) || 0) * 1000);
    return isNaN(d) ? '' : d.toLocaleString();
  }
  function pad2(n) { return (n < 10 ? '0' : '') + n; }

  // tooltip
  function showTip(ev, r, bucketSecs) {
    var tip = els.tooltip;
    tip.innerHTML = '';
    var head = document.createElement('div');
    head.className = 'tt-date';
    head.textContent = bucketFull(r.bucketStartUnix);
    tip.appendChild(head);
    tip.appendChild(tipRow(t('us.colRequests'), fmtInt(r.totalRequests)));
    tip.appendChild(tipRow(t('us.colCost'), fmtCost(r.totalCost)));
    tip.appendChild(tipRow(t('us.colCredits'), fmtCredits(r.totalCredits)));
    tip.classList.add('show');
    var pad = 14;
    var tw = tip.offsetWidth, th = tip.offsetHeight;
    var x = ev.clientX + pad, y = ev.clientY + pad;
    if (x + tw > window.innerWidth) x = ev.clientX - tw - pad;
    if (y + th > window.innerHeight) y = ev.clientY - th - pad;
    tip.style.left = x + 'px';
    tip.style.top = y + 'px';
  }
  function tipRow(label, val) {
    var row = document.createElement('div');
    row.className = 'tt-row';
    var l = document.createElement('span'); l.textContent = label;
    var b = document.createElement('b'); b.textContent = val;
    row.appendChild(l); row.appendChild(b);
    return row;
  }
  function hideTip() { els.tooltip.classList.remove('show'); }

  // ---------------- daily table ----------------
  function renderDaily() {
    var body = els.dailyBody;
    body.innerHTML = '';
    // table shows the full rollup, newest first (desc)
    var rows = state.daily.slice();
    rows.sort(function (a, b) { return a.date < b.date ? 1 : a.date > b.date ? -1 : 0; });

    if (!rows.length) {
      var tr0 = document.createElement('tr');
      var td0 = document.createElement('td'); td0.colSpan = 5;
      td0.appendChild(emptyChart('us.noData'));
      tr0.appendChild(td0); body.appendChild(tr0);
      hideDrill();
      return;
    }

    rows.forEach(function (r) {
      var tr = document.createElement('tr');
      tr.setAttribute('data-date', r.date);

      tr.appendChild(td(r.date, ''));
      tr.appendChild(td(fmtInt(r.totalRequests), 'num'));
      tr.appendChild(td(fmtCost(r.totalCost), 'num'));
      tr.appendChild(td(fmtCredits(r.totalCredits), 'num'));

      var actTd = document.createElement('td');
      actTd.className = 'drill-cell';
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'btn btn-outline btn-sm';
      btn.textContent = t('us.viewRecords');
      btn.addEventListener('click', function (e) { e.stopPropagation(); toggleDrill(r.date); });
      actTd.appendChild(btn);
      tr.appendChild(actTd);

      tr.addEventListener('click', function () { toggleDrill(r.date); });
      body.appendChild(tr);
    });
  }

  function td(text, cls) {
    var el = document.createElement('td');
    if (cls) el.className = cls;
    el.textContent = text;
    return el;
  }

  // ---------------- drill-down records ----------------
  function toggleDrill(date) {
    if (state.drillDate === date && !els.drill.hidden) { hideDrill(); return; }
    state.drillDate = date;
    state.drillPage = 1;
    loadDrill(date, 1);
  }
  function hideDrill() {
    state.drillDate = null;
    els.drill.hidden = true;
    els.drill.innerHTML = '';
  }

  async function loadDrill(date, page) {
    state.drillPage = page;
    els.drill.hidden = false;
    els.drill.innerHTML =
      '<div class="chart-loading"><span class="stats-spinner"></span><span>' + escapeText(t('common.loading')) + '</span></div>';

    var data;
    try {
      data = await K.api.get('/usage/daily/' + encodeURIComponent(date) + '/records',
        { page: page, page_size: state.drillPageSize });
    } catch (e) {
      els.drill.innerHTML = '';
      els.drill.appendChild(emptyChart('common.error'));
      return;
    }
    renderDrill(date, data);
  }

  function renderDrill(date, data) {
    var recs = (data && Array.isArray(data.records)) ? data.records : [];
    var total = data && data.total != null ? data.total : recs.length;
    var totalPages = data && data.totalPages != null ? data.totalPages : 1;

    els.drill.innerHTML = '';

    // header
    var head = document.createElement('div');
    head.className = 'usage-drill-head';
    var h3 = document.createElement('h3');
    h3.textContent = t('us.drillTitle', { date: date });
    head.appendChild(h3);
    var closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'btn btn-outline btn-sm';
    closeBtn.textContent = t('common.close');
    closeBtn.addEventListener('click', hideDrill);
    head.appendChild(closeBtn);
    els.drill.appendChild(head);

    if (!recs.length) {
      els.drill.appendChild(emptyChart('us.noData'));
      return;
    }

    // table
    var wrap = document.createElement('div');
    wrap.className = 'usage-table-wrap';
    var table = document.createElement('table');
    table.className = 'data-table usage-records-table';

    var thead = document.createElement('thead');
    var htr = document.createElement('tr');
    htr.appendChild(th('usg.recModel', ''));
    htr.appendChild(th('usg.recInput', 'num'));
    htr.appendChild(th('usg.recOutput', 'num'));
    htr.appendChild(th('usg.recCost', 'num'));
    htr.appendChild(th('usg.recCredits', 'num'));
    htr.appendChild(th('usg.recCredential', ''));
    htr.appendChild(th('usg.recClientIp', ''));
    htr.appendChild(th('usg.recTime', ''));
    thead.appendChild(htr);
    table.appendChild(thead);

    var tbody = document.createElement('tbody');
    recs.forEach(function (r) {
      var tr = document.createElement('tr');
      tr.appendChild(td(r.model || '—', ''));
      tr.appendChild(td(fmtInt(r.inputTokens), 'num'));
      tr.appendChild(td(fmtInt(r.outputTokens), 'num'));
      tr.appendChild(td(fmtCost(r.estimatedCost), 'num'));
      tr.appendChild(td(r.creditsUsed != null ? fmtCredits(r.creditsUsed) : '—', 'num'));
      tr.appendChild(td(credentialLabel(r), ''));
      tr.appendChild(td(r.clientIp || '—', 'mono'));
      tr.appendChild(td(fmtDateTime(r.createdAt), ''));
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);
    wrap.appendChild(table);
    els.drill.appendChild(wrap);

    // pager
    if (totalPages > 1) {
      var pager = document.createElement('div');
      pager.className = 'usage-pager';

      var prev = document.createElement('button');
      prev.type = 'button'; prev.className = 'btn btn-outline btn-sm';
      prev.textContent = t('common.prev');
      prev.disabled = state.drillPage <= 1;
      prev.addEventListener('click', function () { loadDrill(date, state.drillPage - 1); });

      var info = document.createElement('span');
      info.className = 'page-info';
      info.textContent = t('common.page') + ' ' + state.drillPage + ' / ' + totalPages +
        ' · ' + t('common.total') + ' ' + fmtInt(total);

      var next = document.createElement('button');
      next.type = 'button'; next.className = 'btn btn-outline btn-sm';
      next.textContent = t('common.next');
      next.disabled = state.drillPage >= totalPages;
      next.addEventListener('click', function () { loadDrill(date, state.drillPage + 1); });

      pager.appendChild(prev);
      pager.appendChild(info);
      pager.appendChild(next);
      els.drill.appendChild(pager);
    }

    els.drill.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }

  function credentialLabel(r) {
    if (r.credentialLabel) return r.credentialLabel;
    if (r.credentialId != null) return '#' + r.credentialId;
    return '—';
  }

  function th(labelKey, cls) {
    var el = document.createElement('th');
    if (cls) el.className = cls;
    el.textContent = t(labelKey);
    return el;
  }

  // ---------------- register + expose ----------------
  K.sections.usage = {
    init: init,
    onShow: function () { load(); },
    onHide: function () { hideTip(); }
  };

  window.renderUsageStats = function () {
    if (!inited) init();
    return load();
  };
})();

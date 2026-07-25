/* kiro2api Admin UI v2 — section-logs.js
   Renders the Live Logs section (<section id="sec-logs">).

   Registers K.sections.logs = { init, onShow, onHide } and exposes
   window.renderLogs() for the app router.

   ── Data plane ─────────────────────────────────────────────────────────────
   SSE stream (admin key rides in ?api_key= because EventSource can't set
   headers — handled by K.api.eventSource):
       GET /api/admin/logs/stream?api_key=<adminKey>
         event "history"  -> data = LogEntry[]  (one-shot backfill)
         event "log"      -> data = LogEntry     (each new record)
         keep-alive comments are ignored by EventSource automatically
   REST snapshot fallback (in case a reverse proxy buffers the SSE history
   event so it never arrives):
       GET /api/admin/logs/snapshot -> LogEntry[]
   Download:
       GET /api/admin/logs/download -> text/plain (fetched with x-api-key via
       K.api.raw, turned into a Blob so the <a download> can carry the header).

   LogEntry shape (backend src/logcap.rs): { timestamp, level, target, message }.
   level ∈ TRACE | DEBUG | INFO | WARN | ERROR.

   ── XSS ────────────────────────────────────────────────────────────────────
   Every log field (timestamp, level, target, message) is written via
   textContent — NEVER innerHTML. Log messages are fully untrusted. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};

  var SEC_ID = 'sec-logs';
  var els = {};
  var inited = false;

  var MAX_LINES = 2000;     // ring-buffer cap for rendered rows

  var state = {
    lines: [],              // full ring buffer of LogEntry (post-cap)
    level: 'all',           // 'all' | 'info' | 'warn' | 'error'
    query: '',              // lowercased search filter
    paused: false,
    autoscroll: true,
    pending: [],            // entries received while paused (flushed on resume)
    es: null,               // EventSource
    connState: 'idle',      // idle | connecting | connected | disconnected | reconnecting
    reconnectTimer: null,
    reconnectDelay: 1000,
    seededSnapshot: false,
    selected: null          // currently expanded detail entry
  };

  // ---------------- helpers ----------------
  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }
  function toast(msg, type) { if (K.api && K.api.toast) K.api.toast(msg, type); }

  function levelClass(lvl) {
    var L = String(lvl || '').toUpperCase();
    if (L === 'ERROR') return 'lvl-error';
    if (L === 'WARN' || L === 'WARNING') return 'lvl-warn';
    if (L === 'INFO') return 'lvl-info';
    if (L === 'DEBUG') return 'lvl-debug';
    return 'lvl-trace';
  }
  // map a raw level to the coarse filter bucket
  function levelBucket(lvl) {
    var L = String(lvl || '').toUpperCase();
    if (L === 'ERROR') return 'error';
    if (L === 'WARN' || L === 'WARNING') return 'warn';
    if (L === 'INFO') return 'info';
    return 'debug';   // DEBUG/TRACE — only shown under "all"
  }
  function shortTime(ts) {
    // "2026-07-24T14:30:45.123Z" -> "14:30:45.123"
    var s = String(ts || '');
    var tIdx = s.indexOf('T');
    if (tIdx >= 0) { s = s.slice(tIdx + 1); s = s.replace(/Z$/, ''); }
    return s || String(ts || '');
  }

  function matches(entry) {
    if (state.level !== 'all' && levelBucket(entry.level) !== state.level) return false;
    if (state.query) {
      var hay = ((entry.message || '') + ' ' + (entry.target || '') + ' ' + (entry.level || '')).toLowerCase();
      if (hay.indexOf(state.query) < 0) return false;
    }
    return true;
  }

  // ---------------- static template ----------------
  function template() {
    return '' +
      '<div class="logs-head">' +
        '<h2 data-i18n="log.title">Live Logs</h2>' +
        '<div class="logs-toolbar">' +
          '<div class="logs-search">' +
            '<span class="logs-search-icon" aria-hidden="true">🔍</span>' +
            '<input type="text" id="logSearch" class="form-control" ' +
                   'data-i18n-ph="log.searchPh" placeholder="Filter logs…">' +
          '</div>' +
          '<div class="logs-levels btn-group" id="logLevels">' +
            '<button type="button" class="btn btn-outline btn-sm active" data-level="all" data-i18n="log.levelAll">All</button>' +
            '<button type="button" class="btn btn-outline btn-sm" data-level="info" data-i18n="log.levelInfo">Info</button>' +
            '<button type="button" class="btn btn-outline btn-sm" data-level="warn" data-i18n="log.levelWarn">Warn</button>' +
            '<button type="button" class="btn btn-outline btn-sm" data-level="error" data-i18n="log.levelError">Error</button>' +
          '</div>' +
          '<div class="logs-actions">' +
            '<button type="button" class="btn btn-outline btn-sm" id="logPauseBtn" data-i18n="log.pause">Pause</button>' +
            '<button type="button" class="btn btn-outline btn-sm" id="logClearBtn" data-i18n="log.clear">Clear</button>' +
            '<button type="button" class="btn btn-outline btn-sm" id="logDownloadBtn" data-i18n="log.download">Download</button>' +
            '<label class="mt-check logs-autoscroll"><input type="checkbox" id="logAutoscroll" checked>' +
              '<span data-i18n="log.autoscroll">Auto-scroll</span></label>' +
            '<span class="logs-status" id="logStatus"><span class="logs-status-dot"></span>' +
              '<span class="logs-status-text" id="logStatusText"></span></span>' +
          '</div>' +
        '</div>' +
      '</div>' +

      '<div class="logs-layout">' +
        '<div class="logs-console" id="logConsole">' +
          '<div class="logs-empty" id="logEmpty" data-i18n="log.empty">No logs yet</div>' +
        '</div>' +
        '<div class="logs-detail" id="logDetail" hidden>' +
          '<div class="logs-detail-head">' +
            '<h4 data-i18n="log.detail">Log Detail</h4>' +
            '<button type="button" class="btn btn-outline btn-sm" id="logDetailClose" data-i18n="common.close">Close</button>' +
          '</div>' +
          '<pre class="logs-detail-body" id="logDetailBody"></pre>' +
        '</div>' +
      '</div>';
  }

  // ---------------- init ----------------
  function init() {
    if (inited) return;
    var host = document.getElementById(SEC_ID);
    if (!host) return;
    host.innerHTML = template();

    els.search = host.querySelector('#logSearch');
    els.levels = host.querySelector('#logLevels');
    els.pauseBtn = host.querySelector('#logPauseBtn');
    els.clearBtn = host.querySelector('#logClearBtn');
    els.downloadBtn = host.querySelector('#logDownloadBtn');
    els.autoscroll = host.querySelector('#logAutoscroll');
    els.status = host.querySelector('#logStatus');
    els.statusText = host.querySelector('#logStatusText');
    els.console = host.querySelector('#logConsole');
    els.empty = host.querySelector('#logEmpty');
    els.detail = host.querySelector('#logDetail');
    els.detailBody = host.querySelector('#logDetailBody');
    els.detailClose = host.querySelector('#logDetailClose');

    // search (debounced)
    var searchTimer = null;
    els.search.addEventListener('input', function () {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(function () {
        state.query = (els.search.value || '').trim().toLowerCase();
        rerender();
      }, 250);
    });

    // level buttons
    els.levels.addEventListener('click', function (e) {
      var b = e.target.closest('button[data-level]'); if (!b) return;
      state.level = b.getAttribute('data-level');
      els.levels.querySelectorAll('button').forEach(function (x) {
        x.classList.toggle('active', x === b);
      });
      rerender();
    });

    // pause / resume
    els.pauseBtn.addEventListener('click', togglePause);
    // clear (client-only)
    els.clearBtn.addEventListener('click', function () {
      state.lines = []; state.pending = []; state.selected = null;
      closeDetail();
      rerender();
    });
    // download
    els.downloadBtn.addEventListener('click', download);
    // autoscroll
    els.autoscroll.addEventListener('change', function () {
      state.autoscroll = els.autoscroll.checked;
      if (state.autoscroll) scrollToBottom();
    });
    // if the user scrolls up, pause autoscroll implicitly (re-arm at bottom)
    els.console.addEventListener('scroll', function () {
      if (!state.autoscroll) return;
      var nearBottom = els.console.scrollHeight - els.console.scrollTop - els.console.clientHeight < 24;
      if (!nearBottom) { /* leave user in place; autoscroll checkbox still on but won't yank */ }
    });

    els.detailClose.addEventListener('click', closeDetail);

    if (K.i18n) K.i18n.apply(host);
    window.addEventListener('langchange', function () {
      paintStatus();
      updatePauseLabel();
    });

    paintStatus();
    updatePauseLabel();
    inited = true;
  }

  function updatePauseLabel() {
    els.pauseBtn.textContent = state.paused ? t('log.resume') : t('log.pause');
  }

  function togglePause() {
    state.paused = !state.paused;
    updatePauseLabel();
    if (!state.paused && state.pending.length) {
      // flush buffered entries collected while paused
      state.pending.forEach(pushEntry);
      state.pending = [];
      rerender();
    }
  }

  // ---------------- connection ----------------
  function setConn(s) {
    state.connState = s;
    paintStatus();
  }
  function paintStatus() {
    if (!els.status) return;
    var map = {
      idle: 'log.connecting',
      connecting: 'log.connecting',
      connected: 'log.connected',
      disconnected: 'log.disconnected',
      reconnecting: 'log.reconnecting'
    };
    els.statusText.textContent = t(map[state.connState] || 'log.connecting');
    els.status.setAttribute('data-state', state.connState);
  }

  function connect() {
    if (state.es) return;
    setConn('connecting');
    // snapshot fallback first (covers proxies that buffer the SSE history event)
    if (!state.seededSnapshot) seedSnapshot();

    var es;
    try {
      es = K.api.eventSource('/logs/stream');
    } catch (e) {
      setConn('disconnected');
      scheduleReconnect();
      return;
    }
    state.es = es;

    es.addEventListener('open', function () {
      state.reconnectDelay = 1000;
      setConn('connected');
    });

    es.addEventListener('history', function (ev) {
      var arr = parseJson(ev.data);
      if (Array.isArray(arr)) {
        // history overwrites the current buffer (authoritative initial batch)
        state.lines = arr.slice(-MAX_LINES);
        state.seededSnapshot = true;
        rerender();
      }
    });

    es.addEventListener('log', function (ev) {
      var entry = parseJson(ev.data);
      if (!entry || typeof entry !== 'object') return;
      if (state.paused) { state.pending.push(entry); capPending(); return; }
      pushEntry(entry);
      renderAppend(entry);
    });

    es.addEventListener('error', function () {
      // EventSource auto-reconnects, but surface the state + do our own backoff
      // if the browser gives up (readyState CLOSED).
      if (es.readyState === 2 /* CLOSED */) {
        teardownEs();
        setConn('reconnecting');
        scheduleReconnect();
      } else {
        setConn('reconnecting');
      }
    });
  }

  function teardownEs() {
    if (state.es) { try { state.es.close(); } catch (e) {} state.es = null; }
  }

  function scheduleReconnect() {
    if (state.reconnectTimer) return;
    var delay = state.reconnectDelay;
    state.reconnectTimer = setTimeout(function () {
      state.reconnectTimer = null;
      state.reconnectDelay = Math.min(state.reconnectDelay * 2, 15000);
      connect();
    }, delay);
  }

  function disconnect() {
    if (state.reconnectTimer) { clearTimeout(state.reconnectTimer); state.reconnectTimer = null; }
    teardownEs();
    setConn('disconnected');
  }

  function seedSnapshot() {
    K.api.get('/logs/snapshot').then(function (arr) {
      if (state.seededSnapshot) return;       // SSE history already won the race
      if (Array.isArray(arr) && arr.length) {
        state.lines = arr.slice(-MAX_LINES);
        state.seededSnapshot = true;
        rerender();
      }
    }).catch(function () { /* snapshot is best-effort */ });
  }

  function parseJson(s) { try { return JSON.parse(s); } catch (e) { return null; } }

  // ---------------- buffer ----------------
  function pushEntry(entry) {
    state.lines.push(entry);
    if (state.lines.length > MAX_LINES) state.lines.splice(0, state.lines.length - MAX_LINES);
  }
  function capPending() {
    if (state.pending.length > MAX_LINES) state.pending.splice(0, state.pending.length - MAX_LINES);
  }

  // ---------------- render ----------------
  function buildLine(entry) {
    var row = document.createElement('div');
    row.className = 'log-line ' + levelClass(entry.level);

    var ts = document.createElement('span');
    ts.className = 'log-ts';
    ts.textContent = shortTime(entry.timestamp);   // textContent — XSS-safe

    var lvl = document.createElement('span');
    lvl.className = 'log-level';
    lvl.textContent = String(entry.level || '').toUpperCase();

    var msg = document.createElement('span');
    msg.className = 'log-msg';
    msg.textContent = entry.message || '';          // untrusted — textContent ONLY

    row.appendChild(ts);
    row.appendChild(lvl);
    row.appendChild(msg);

    row.addEventListener('click', function () { openDetail(entry, row); });
    return row;
  }

  // full re-render from the buffer honoring filters
  function rerender() {
    if (!els.console) return;
    els.console.innerHTML = '';
    var shown = 0;
    for (var i = 0; i < state.lines.length; i++) {
      var e = state.lines[i];
      if (!matches(e)) continue;
      els.console.appendChild(buildLine(e));
      shown++;
    }
    if (shown === 0) {
      var empty = document.createElement('div');
      empty.className = 'logs-empty';
      empty.textContent = t('log.empty');
      els.console.appendChild(empty);
    }
    if (state.autoscroll) scrollToBottom();
  }

  // append a single new entry without rebuilding (fast path for live "log" events)
  function renderAppend(entry) {
    if (!matches(entry)) return;
    // drop the empty-state note if present
    var emptyNote = els.console.querySelector('.logs-empty');
    if (emptyNote) emptyNote.remove();

    var nearBottom = els.console.scrollHeight - els.console.scrollTop - els.console.clientHeight < 24;
    els.console.appendChild(buildLine(entry));

    // enforce DOM cap (mirror the buffer cap) so long sessions stay light
    while (els.console.childElementCount > MAX_LINES) {
      els.console.removeChild(els.console.firstElementChild);
    }
    if (state.autoscroll && nearBottom) scrollToBottom();
  }

  function scrollToBottom() {
    if (els.console) els.console.scrollTop = els.console.scrollHeight;
  }

  // ---------------- detail pane ----------------
  function openDetail(entry, row) {
    state.selected = entry;
    els.console.querySelectorAll('.log-line.selected').forEach(function (x) { x.classList.remove('selected'); });
    if (row) row.classList.add('selected');
    // pretty, readable dump of the full record (all fields textContent-safe)
    var lines = [
      'timestamp: ' + (entry.timestamp || ''),
      'level:     ' + (entry.level || ''),
      'target:    ' + (entry.target || ''),
      '',
      (entry.message || '')
    ];
    els.detailBody.textContent = lines.join('\n');   // textContent — XSS-safe
    els.detail.hidden = false;
  }
  function closeDetail() {
    state.selected = null;
    els.detail.hidden = true;
    if (els.console) els.console.querySelectorAll('.log-line.selected').forEach(function (x) { x.classList.remove('selected'); });
  }

  // ---------------- download ----------------
  function download() {
    // fetch with the admin key (link can't set x-api-key) → Blob → <a download>
    K.api.raw('/logs/download').then(function (res) {
      return res.blob();
    }).then(function (blob) {
      var url = URL.createObjectURL(blob);
      var a = document.createElement('a');
      a.href = url;
      a.download = 'kiro2api-logs.txt';
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(function () { URL.revokeObjectURL(url); }, 1000);
    }).catch(function () {
      toast(t('common.error'), 'error');
    });
  }

  // ---------------- register + expose ----------------
  K.sections.logs = {
    init: init,
    onShow: function () {
      if (!inited) init();
      // reset backoff and (re)open the stream on entry
      state.reconnectDelay = 1000;
      connect();
    },
    onHide: function () {
      // close SSE + cancel pending reconnect when leaving the section
      disconnect();
    }
  };

  window.renderLogs = function () {
    if (!inited) init();
    connect();
  };
})();

/* kiro2api Admin UI v2 — api.js
   Network layer.
     - Admin calls: base '/api/admin', authenticated with the admin key via
       the `x-api-key` header (localStorage 'kiro2api_admin_key').
     - SSE (logs): EventSource can't set headers, so the admin key is passed
       as the `?api_key=` query parameter (backend supports this).
     - Playground / relay calls: base '/v1', authenticated with the master
       key via `Authorization: Bearer` (localStorage 'kiro2api_master_key').

   On 401/403 from an admin call the session is cleared and the login overlay
   is shown (app.js registers the handler as K.api.onUnauthorized). */
(function () {
  'use strict';

  var ADMIN_BASE = '/api/admin';
  var RELAY_BASE = '/v1';
  var ADMIN_KEY = 'kiro2api_admin_key';
  var MASTER_KEY = 'kiro2api_master_key';

  function getAdminKey() { try { return localStorage.getItem(ADMIN_KEY) || ''; } catch (e) { return ''; } }
  function setAdminKey(k) { try { k ? localStorage.setItem(ADMIN_KEY, k) : localStorage.removeItem(ADMIN_KEY); } catch (e) {} }
  function getMasterKey() { try { return localStorage.getItem(MASTER_KEY) || ''; } catch (e) { return ''; } }
  function setMasterKey(k) { try { k ? localStorage.setItem(MASTER_KEY, k) : localStorage.removeItem(MASTER_KEY); } catch (e) {} }

  // ---- toast ----
  function toast(msg, type) {
    type = type || 'info';
    var box = document.getElementById('toastContainer');
    if (!box) { return; }
    var el = document.createElement('div');
    el.className = 'toast ' + type;
    el.textContent = msg;
    box.appendChild(el);
    // force reflow so the transition runs
    void el.offsetWidth;
    el.classList.add('show');
    setTimeout(function () {
      el.classList.remove('show');
      setTimeout(function () { el.remove(); }, 300);
    }, 3200);
  }

  // A small error carrier so callers can inspect HTTP status / parsed body.
  function ApiError(status, message, body) {
    this.name = 'ApiError';
    this.status = status;
    this.message = message || 'request failed';
    this.body = body;
  }
  ApiError.prototype = Object.create(Error.prototype);

  function tr(key, fallback) {
    if (window.K && window.K.i18n && typeof window.K.i18n.t === 'function') {
      var v = window.K.i18n.t(key);
      if (v && v !== key) return v;
    }
    return fallback;
  }

  // ---- core admin request ----
  // opts: { method, body, query, signal, raw }
  //   raw:true  -> returns the Response (caller reads blob/text itself)
  async function adminRequest(path, opts) {
    opts = opts || {};
    var key = getAdminKey();
    var url = ADMIN_BASE + path;
    if (opts.query) {
      var qs = new URLSearchParams(opts.query).toString();
      if (qs) url += (url.indexOf('?') >= 0 ? '&' : '?') + qs;
    }
    var headers = {};
    if (key) headers['x-api-key'] = key;
    var init = { method: opts.method || 'GET', headers: headers, signal: opts.signal };
    if (opts.body !== undefined && opts.body !== null) {
      headers['Content-Type'] = 'application/json';
      init.body = JSON.stringify(opts.body);
    }

    var res;
    try {
      res = await fetch(url, init);
    } catch (e) {
      if (e && e.name === 'AbortError') throw e;
      throw new ApiError(0, tr('common.error', 'Network error'));
    }

    if (res.status === 401 || res.status === 403) {
      setAdminKey('');
      if (typeof Api.onUnauthorized === 'function') Api.onUnauthorized();
      throw new ApiError(res.status, tr('login.error', 'Unauthorized'));
    }

    if (opts.raw) {
      if (!res.ok) throw new ApiError(res.status, tr('common.error', 'Request failed'));
      return res;
    }

    var data = null;
    var text = await res.text();
    if (text) { try { data = JSON.parse(text); } catch (e) { data = text; } }

    if (!res.ok) {
      var msg = (data && (data.error || data.message)) || tr('common.error', 'Request failed');
      throw new ApiError(res.status, msg, data);
    }
    return data;
  }

  // ---- SSE (logs) ----
  // EventSource can't set headers; the admin key rides in ?api_key=.
  function adminEventSource(path, query) {
    var q = new URLSearchParams(query || {});
    q.set('api_key', getAdminKey());
    var url = ADMIN_BASE + path + (path.indexOf('?') >= 0 ? '&' : '?') + q.toString();
    return new EventSource(url);
  }

  // ---- relay (playground) ----
  // opts: { body, signal, stream }  — always uses the master key as Bearer.
  async function relayFetch(path, opts) {
    opts = opts || {};
    var master = getMasterKey();
    var headers = { 'Content-Type': 'application/json' };
    if (master) { headers['Authorization'] = 'Bearer ' + master; headers['x-api-key'] = master; }
    var init = { method: 'POST', headers: headers, signal: opts.signal };
    if (opts.body !== undefined) init.body = JSON.stringify(opts.body);
    return fetch(RELAY_BASE + path, init);
  }

  var Api = {
    ADMIN_BASE: ADMIN_BASE,
    RELAY_BASE: RELAY_BASE,

    // key management
    getAdminKey: getAdminKey,
    setAdminKey: setAdminKey,
    hasAdminKey: function () { return !!getAdminKey(); },
    getMasterKey: getMasterKey,
    setMasterKey: setMasterKey,
    hasMasterKey: function () { return !!getMasterKey(); },
    clearSession: function () { setAdminKey(''); setMasterKey(''); },

    // admin verbs
    request: adminRequest,
    get: function (path, query) { return adminRequest(path, { method: 'GET', query: query }); },
    post: function (path, body) { return adminRequest(path, { method: 'POST', body: body }); },
    put: function (path, body) { return adminRequest(path, { method: 'PUT', body: body }); },
    del: function (path, body) { return adminRequest(path, { method: 'DELETE', body: body }); },
    raw: function (path, query) { return adminRequest(path, { method: 'GET', query: query, raw: true }); },

    // sse + relay
    eventSource: adminEventSource,
    relayFetch: relayFetch,

    // ui helpers
    toast: toast,
    ApiError: ApiError,

    // set by app.js
    onUnauthorized: null
  };

  window.K = window.K || {};
  window.K.api = Api;

  // Section-agent contract alias requested by the coordinating spec.
  window.api = Api;
})();

/* kiro2api Admin UI v2 — api.js
   Network layer.
     - Admin calls: base '/api/admin', authenticated with the admin key via
       the `x-api-key` header (localStorage 'kiro2api_admin_key').
     - SSE (logs): EventSource can't set headers, so the admin key is passed
       as the `?api_key=` query parameter (backend supports this).
     - Playground / relay calls: base '/v1', authenticated with the master
       key via `Authorization: Bearer` (localStorage 'kiro2api_master_key').

   On 401 from an admin call the session is cleared and the login overlay is
   shown (app.js registers the handler as K.api.onUnauthorized). 403 is NOT a
   session error here — the admin auth gate only ever answers 401; a 403 comes
   from business logic (登录被拒绝),so it surfaces as a normal ApiError. */
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

    // 只有 401 代表「管理员密钥无效」——鉴权闸(src/server/auth.rs 的
    // require_api_key)拒绝请求时恒回 401,不会回 403。
    //
    // 403 在 /api/admin 下是**业务拒绝**,唯一来源是登录流程:用户在 AWS 设备页
    // 点了「拒绝」(或上游回 access_denied)→ LoginError::Denied →
    // login_upstream_error 回 403 +「授权被拒绝」(src/admin/handler.rs)。老写法
    // 把 403 也当成掉线:清掉本地钥匙、弹登录浮层、拆掉当前分区,还把真正的原因
    // 替换成「管理员密钥无效」——管理员只是拒绝了一次授权,却被踢出后台并被告知
    // 钥匙坏了。所以 403 一律往下走普通错误分支,把后端的 error 文案原样抛给调用方。
    if (res.status === 401) {
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

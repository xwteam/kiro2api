/* kiro2api Admin UI v2 — sec-settings.js
   Settings section (#sec-settings): a 2-column responsive card grid
   (Auth Keys first, Load Balancing second; side-by-side on md+, stacked
   on narrow screens).

   1. Auth Keys        — GET (masked) /api/admin/config/auth-keys,
                         PUT {apiKey?, adminApiKey?}; on admin-key change the
                         local session key is updated so the session survives.
   2. Load Balancing  — GET/PUT /api/admin/config/load-balancing {mode}.

   (The supported-models list now lives on its own Models page — sec-models.js.
    The Server Info / system-info module now lives on the Dashboard.)

   Public entry: registers K.sections.settings = { init, onShow, onHide }.
   Dynamic values via textContent; static labels via data-i18n. */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};
  var api = K.api;

  (function ensureCss() {
    if (document.querySelector('link[data-manage-css]')) return;
    var l = document.createElement('link');
    l.rel = 'stylesheet'; l.href = 'css/sections-manage.css';
    l.setAttribute('data-manage-css', '1');
    document.head.appendChild(l);
  })();

  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }
  function el(tag, cls, txt) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (txt != null) n.textContent = txt;
    return n;
  }
  function copy(text) {
    try { navigator.clipboard.writeText(text).then(function () { api.toast(t('common.copied'), 'success'); }); }
    catch (e) {
      var ta = document.createElement('textarea'); ta.value = text; document.body.appendChild(ta);
      ta.select(); try { document.execCommand('copy'); api.toast(t('common.copied'), 'success'); } catch (_) {}
      ta.remove();
    }
  }

  // ---------- generic confirm modal ----------
  function confirmModal(msgKey) {
    return new Promise(function (resolve) {
      var root = document.getElementById('modalRoot') || document.body;
      var overlay = el('div', 'modal-overlay active');
      var modal = el('div', 'modal');
      var head = el('div', 'modal-header');
      head.appendChild(el('h3', null, t('common.confirm')));
      var closeBtn = el('button', 'modal-close'); closeBtn.type = 'button'; closeBtn.textContent = '✕';
      head.appendChild(closeBtn); modal.appendChild(head);
      var body = el('div', 'modal-body'); body.appendChild(el('p', null, t(msgKey)));
      modal.appendChild(body);
      var footer = el('div', 'modal-footer');
      var cancel = el('button', 'btn btn-outline', t('common.cancel')); cancel.type = 'button';
      var ok = el('button', 'btn btn-primary', t('common.confirm')); ok.type = 'button';
      footer.appendChild(cancel); footer.appendChild(ok); modal.appendChild(footer);
      overlay.appendChild(modal); root.appendChild(overlay);
      var resolved = false;
      function done(v) { if (resolved) return; resolved = true; overlay.remove(); resolve(v); }
      closeBtn.addEventListener('click', function () { done(false); });
      cancel.addEventListener('click', function () { done(false); });
      overlay.addEventListener('click', function (e) { if (e.target === overlay) done(false); });
      ok.addEventListener('click', function () { done(true); });
    });
  }

  // ---------- state ----------
  var container = null;

  function buildShell() {
    container = document.getElementById('sec-settings');
    if (!container) return;
    container.innerHTML = '';

    var header = el('div', 'section-header');
    header.appendChild(el('h2', null, t('set.title')));
    var reloadBtn = el('button', 'btn btn-outline', t('common.refresh')); reloadBtn.type = 'button';
    reloadBtn.addEventListener('click', function () { loadAll(); });
    var actions = el('div', 'section-actions'); actions.appendChild(reloadBtn);
    header.appendChild(actions);
    container.appendChild(header);

    // 2-column responsive grid (collapses to 1 column on narrow screens).
    // Order: Auth Keys first, then Load Balancing.
    var grid = el('div', 'settings-grid'); grid.id = 'settingsGrid';
    grid.appendChild(buildAuthKeysCard());
    grid.appendChild(buildLoadBalancingCard());
    grid.appendChild(buildClientIdentityCard());
    container.appendChild(grid);

    // 集成示例(全宽,置于两张设置卡之下):协议 × 语言的可复制代码片段。
    container.appendChild(buildIntegrationCard());

    if (K.i18n) K.i18n.apply(container);
  }

  var identityEndpoint, identityIde, identityCli, identityLatest, identitySaved = null;
  function buildClientIdentityCard() {
    var card = el('div', 'card');
    var header = el('div', 'card-header'); header.appendChild(el('h3', null, t('identity.title'))); card.appendChild(header);
    card.appendChild(el('p', 'settings-card-desc', t('identity.warning')));
    function field(label, input) { var g = el('div', 'form-group'); g.appendChild(el('label', 'form-label', label)); g.appendChild(input); card.appendChild(g); }
    identityEndpoint = el('select', 'form-control');
    [['ide', t('identity.ideEndpoint')], ['cli', t('identity.cliEndpoint')]].forEach(function (item) { var opt = el('option', null, item[1]); opt.value = item[0]; identityEndpoint.appendChild(opt); });
    field(t('identity.endpoint'), identityEndpoint);
    identityIde = el('input', 'form-control'); identityIde.type = 'text'; field(t('identity.ideVersion'), identityIde);
    identityCli = el('input', 'form-control'); identityCli.type = 'text'; field(t('identity.cliVersion'), identityCli);
    identityLatest = el('p', 'form-hint'); card.appendChild(identityLatest);
    var check = el('button', 'btn btn-outline', t('identity.check')); check.type = 'button';
    check.addEventListener('click', function () {
      check.disabled = true;
      api.get('/config/client-identity/check').then(function (r) {
        var newer = function (latest, current) { var a = latest.split('.').map(Number), b = current.split('.').map(Number); for (var i = 0; i < 3; i++) { if (a[i] !== b[i]) return a[i] > b[i]; } return false; };
        identityLatest.textContent = t('identity.latest') + ': IDE ' + r.latestIdeVersion + (newer(r.latestIdeVersion, r.kiroVersion) ? ' ↑' : '') + ' / CLI ' + r.latestCliVersion + (newer(r.latestCliVersion, r.kiroCliVersion) ? ' ↑' : '') + ' · ' + t('identity.checkedAt') + ': ' + r.checkedAt;
        identityLatest.style.color = newer(r.latestIdeVersion, r.kiroVersion) || newer(r.latestCliVersion, r.kiroCliVersion) ? 'var(--warning, #d97706)' : '';
      }).catch(function (e) { api.toast(e.message || t('common.error'), 'error'); }).then(function () { check.disabled = false; });
    });
    card.appendChild(check);
    var save = el('button', 'btn btn-primary', t('common.save')); save.type = 'button';
    save.addEventListener('click', function () {
      save.disabled = true;
      // 只提交改过的字段:别把运行中的旧值写回去覆盖了另一次已保存、待重启的选择。
      var body = {}, now = {upstreamEndpoint: identityEndpoint.value, kiroVersion: identityIde.value.trim(), kiroCliVersion: identityCli.value.trim()};
      Object.keys(now).forEach(function (k) { if (!identitySaved || identitySaved[k] !== now[k]) body[k] = now[k]; });
      if (!Object.keys(body).length) { save.disabled = false; api.toast(t('identity.restart'), 'success'); return; }
      api.put('/config/client-identity', body)
        .then(function (r) { applyClientIdentity(r); api.toast(t('identity.restart'), 'success'); }).catch(function (e) { api.toast(e.message || t('common.error'), 'error'); }).then(function () { save.disabled = false; });
    });
    card.appendChild(save); return card;
  }
  // 表单显示「已保存」的值(重启后生效的那份);和运行中的不一样时提示待重启。
  function applyClientIdentity(r) {
    var saved = r.saved || {upstreamEndpoint: r.upstreamEndpoint, kiroVersion: r.kiroVersion, kiroCliVersion: r.kiroCliVersion};
    identitySaved = {upstreamEndpoint: saved.upstreamEndpoint, kiroVersion: saved.kiroVersion, kiroCliVersion: saved.kiroCliVersion};
    identityEndpoint.value = saved.upstreamEndpoint; identityIde.value = saved.kiroVersion; identityCli.value = saved.kiroCliVersion;
    var notes = [];
    if (r.restartRequired) notes.push(t('identity.pending') + ': ' + r.upstreamEndpoint + ' / IDE ' + r.kiroVersion + ' / CLI ' + r.kiroCliVersion);
    if (r.lastUnsupportedVersionAt) notes.push(t('identity.unsupported') + ': ' + r.lastUnsupportedVersionAt);
    if (notes.length) { identityLatest.textContent = notes.join(' · '); identityLatest.style.color = 'var(--warning, #d97706)'; }
  }
  function loadClientIdentity() {
    if (!identityEndpoint) return;
    api.get('/config/client-identity').then(applyClientIdentity).catch(function () {});
  }

  // ---------- Load Balancing ----------
  var lbSelect = null;
  function buildLoadBalancingCard() {
    var card = el('div', 'card');
    var ch = el('div', 'card-header'); ch.appendChild(el('h3', null, t('set.loadBalancing')));
    card.appendChild(ch);

    var g = el('div', 'form-group');
    g.appendChild(el('label', 'form-label', t('set.lbMode')));
    lbSelect = el('select', 'form-control');
    [['priority', t('set.lbPriority')], ['balanced', t('set.lbBalanced')]].forEach(function (o) {
      var opt = el('option', null, o[1]); opt.value = o[0]; lbSelect.appendChild(opt);
    });
    g.appendChild(lbSelect);
    card.appendChild(g);

    var saveBtn = el('button', 'btn btn-primary', t('common.save')); saveBtn.type = 'button';
    saveBtn.addEventListener('click', function () {
      saveBtn.disabled = true;
      api.put('/config/load-balancing', { mode: lbSelect.value })
        .then(function () { api.toast(t('set.saved'), 'success'); })
        .catch(function (e) { api.toast(e.message || t('common.error'), 'error'); })
        .then(function () { saveBtn.disabled = false; });
    });
    card.appendChild(saveBtn);
    return card;
  }
  function loadLoadBalancing() {
    if (!lbSelect) return;
    api.get('/config/load-balancing').then(function (r) {
      var mode = (r && (r.mode || r.loadBalancing)) || 'priority';
      lbSelect.value = mode;
    }).catch(function () {});
  }

  // ---------- Auth Keys ----------
  var apiKeyInput = null, adminKeyInput = null;
  function buildAuthKeysCard() {
    var card = el('div', 'card');
    var ch = el('div', 'card-header'); ch.appendChild(el('h3', null, t('set.authKeys')));
    card.appendChild(ch);

    var g1 = el('div', 'form-group');
    g1.appendChild(el('label', 'form-label', t('set.apiKey')));
    apiKeyInput = el('input', 'form-control'); apiKeyInput.type = 'password'; apiKeyInput.autocomplete = 'new-password';
    g1.appendChild(apiKeyInput);
    g1.appendChild(el('div', 'form-hint', t('set.keyMasked')));
    card.appendChild(g1);

    var g2 = el('div', 'form-group');
    g2.appendChild(el('label', 'form-label', t('set.adminApiKey')));
    adminKeyInput = el('input', 'form-control'); adminKeyInput.type = 'password'; adminKeyInput.autocomplete = 'new-password';
    g2.appendChild(adminKeyInput);
    g2.appendChild(el('div', 'form-hint', t('set.keyMasked')));
    card.appendChild(g2);

    card.appendChild(el('p', 'settings-card-desc', t('set.authKeysHint')));

    var saveBtn = el('button', 'btn btn-primary', t('common.save')); saveBtn.type = 'button';
    saveBtn.addEventListener('click', function () { saveAuthKeys(saveBtn); });
    card.appendChild(saveBtn);
    return card;
  }
  function loadAuthKeys() {
    if (apiKeyInput) apiKeyInput.value = '';
    if (adminKeyInput) adminKeyInput.value = '';
    // GET is masked; we keep inputs blank (blank = keep) and just surface the hint.
    api.get('/config/auth-keys').then(function (r) {
      if (!r) return;
      if (apiKeyInput && r.apiKey) apiKeyInput.setAttribute('placeholder', r.apiKey);
      if (adminKeyInput && r.adminApiKey) adminKeyInput.setAttribute('placeholder', r.adminApiKey);
    }).catch(function () {});
  }
  function saveAuthKeys(btn) {
    var newApi = apiKeyInput ? apiKeyInput.value.trim() : '';
    var newAdmin = adminKeyInput ? adminKeyInput.value.trim() : '';
    if (!newApi && !newAdmin) { api.toast(t('common.empty'), 'info'); return; }
    confirmModal('set.confirmAuthKeys').then(function (ok) {
      if (!ok) return;
      var payload = {};
      if (newApi) payload.apiKey = newApi;
      if (newAdmin) payload.adminApiKey = newAdmin;
      btn.disabled = true;
      api.put('/config/auth-keys', payload).then(function () {
        // If admin key changed, keep the session alive with the new value.
        if (newAdmin) api.setAdminKey(newAdmin);
        api.toast(t('set.saved'), 'success');
        if (apiKeyInput) apiKeyInput.value = '';
        if (adminKeyInput) adminKeyInput.value = '';
      }).catch(function (e) {
        api.toast(e.message || t('common.error'), 'error');
      }).then(function () { btn.disabled = false; });
    });
  }

  // ---------- Integration Examples ----------
  // 协议 × 语言的可复制接入片段(base URL 由当前站点自动推导;密钥用占位符)。
  // 与 codex-proxy 设置页的「集成示例」同思路:上排选协议、下排选语言、下方代码 + 复制。
  var intProto = 'openai';   // openai | anthropic | gemini
  var intLang = 'python';    // python | node | curl
  var EXAMPLE_MODEL = 'claude-sonnet-4.5';   // 默认展示可用模型
  var API_KEY_PLACEHOLDER = 'YOUR_API_KEY';

  function apiOrigin() {
    return (typeof window !== 'undefined' && window.location && window.location.origin) || 'http://localhost:8080';
  }

  // 返回 { '<proto>-<lang>': '代码串' }。base 由 origin 推导:OpenAI=/v1、Anthropic=根(SDK
  // 自动补 /v1/messages)、Gemini=/v1beta。密钥/模型用占位与默认可用模型。
  function buildSnippets(origin, key, model) {
    var oai = origin + '/v1';
    return {
      'openai-python':
        'from openai import OpenAI\n\n' +
        'client = OpenAI(\n    base_url="' + oai + '",\n    api_key="' + key + '",\n)\n\n' +
        'resp = client.chat.completions.create(\n    model="' + model + '",\n' +
        '    messages=[{"role": "user", "content": "Hello"}],\n)\n' +
        'print(resp.choices[0].message.content)',
      'openai-node':
        'import OpenAI from "openai";\n\n' +
        'const client = new OpenAI({\n    baseURL: "' + oai + '",\n    apiKey: "' + key + '",\n});\n\n' +
        'const resp = await client.chat.completions.create({\n    model: "' + model + '",\n' +
        '    messages: [{ role: "user", content: "Hello" }],\n});\n' +
        'console.log(resp.choices[0].message.content);',
      'openai-curl':
        'curl ' + oai + '/chat/completions \\\n' +
        '  -H "Content-Type: application/json" \\\n' +
        '  -H "Authorization: Bearer ' + key + '" \\\n' +
        '  -d \'{\n    "model": "' + model + '",\n' +
        '    "messages": [{"role": "user", "content": "Hello"}]\n  }\'',

      'anthropic-python':
        'import anthropic\n\n' +
        'client = anthropic.Anthropic(\n    base_url="' + origin + '",\n    api_key="' + key + '",\n)\n\n' +
        'msg = client.messages.create(\n    model="' + model + '",\n    max_tokens=1024,\n' +
        '    messages=[{"role": "user", "content": "Hello"}],\n)\n' +
        'print(msg.content[0].text)',
      'anthropic-node':
        'import Anthropic from "@anthropic-ai/sdk";\n\n' +
        'const client = new Anthropic({\n    baseURL: "' + origin + '",\n    apiKey: "' + key + '",\n});\n\n' +
        'const msg = await client.messages.create({\n    model: "' + model + '",\n    max_tokens: 1024,\n' +
        '    messages: [{ role: "user", content: "Hello" }],\n});\n' +
        'console.log(msg.content[0].text);',
      'anthropic-curl':
        'curl ' + origin + '/v1/messages \\\n' +
        '  -H "Content-Type: application/json" \\\n' +
        '  -H "x-api-key: ' + key + '" \\\n' +
        '  -H "anthropic-version: 2023-06-01" \\\n' +
        '  -d \'{\n    "model": "' + model + '",\n    "max_tokens": 1024,\n' +
        '    "messages": [{"role": "user", "content": "Hello"}]\n  }\'',

      'gemini-python':
        'from google import genai\n\n' +
        'client = genai.Client(\n    api_key="' + key + '",\n' +
        '    http_options={"base_url": "' + origin + '/v1beta"},\n)\n\n' +
        'resp = client.models.generate_content(\n    model="' + model + '",\n    contents="Hello",\n)\n' +
        'print(resp.text)',
      'gemini-node':
        'import { GoogleGenAI } from "@google/genai";\n\n' +
        'const ai = new GoogleGenAI({\n    apiKey: "' + key + '",\n' +
        '    httpOptions: { baseUrl: "' + origin + '/v1beta" },\n});\n\n' +
        'const resp = await ai.models.generateContent({\n    model: "' + model + '",\n    contents: "Hello",\n});\n' +
        'console.log(resp.text);',
      'gemini-curl':
        'curl "' + origin + '/v1beta/models/' + model + ':generateContent?key=' + key + '" \\\n' +
        '  -H "Content-Type: application/json" \\\n' +
        '  -d \'{\n    "contents": [{"role": "user", "parts": [{"text": "Hello"}]}]\n  }\'',
    };
  }

  function buildIntegrationCard() {
    var card = el('div', 'card int-card');
    var ch = el('div', 'card-header'); ch.appendChild(el('h3', null, t('set.integration')));
    card.appendChild(ch);
    var body = el('div', 'card-body');
    body.appendChild(el('p', 'settings-card-desc', t('set.integrationDesc')));

    var protos = [['openai', 'OpenAI'], ['anthropic', 'Anthropic'], ['gemini', 'Gemini']];
    var langs = [['python', 'Python'], ['node', 'Node.js'], ['curl', 'cURL']];

    var codeEl = el('code');
    function render() {
      var snip = buildSnippets(apiOrigin(), API_KEY_PLACEHOLDER, EXAMPLE_MODEL);
      codeEl.textContent = snip[intProto + '-' + intLang] || '';
    }

    function segRow(items, current, onPick) {
      var row = el('div', 'seg-group int-seg');
      items.forEach(function (it) {
        var b = el('button', 'seg-btn' + (it[0] === current() ? ' is-active' : ''), it[1]);
        b.type = 'button';
        b.addEventListener('click', function () {
          onPick(it[0]);
          var kids = row.querySelectorAll('.seg-btn');
          for (var i = 0; i < kids.length; i++) kids[i].classList.toggle('is-active', items[i][0] === it[0]);
          render();
        });
        row.appendChild(b);
      });
      return row;
    }

    body.appendChild(segRow(protos, function () { return intProto; }, function (v) { intProto = v; }));
    body.appendChild(segRow(langs, function () { return intLang; }, function (v) { intLang = v; }));

    var wrap = el('div', 'int-codewrap');
    var copyBtn = el('button', 'btn btn-outline btn-sm int-copy', t('common.copy'));
    copyBtn.type = 'button';
    copyBtn.addEventListener('click', function () { copy(codeEl.textContent); });
    var pre = el('pre', 'int-code'); pre.appendChild(codeEl);
    wrap.appendChild(copyBtn); wrap.appendChild(pre);
    body.appendChild(wrap);

    card.appendChild(body);
    render();
    return card;
  }

  // ---------- orchestration ----------
  function loadAll() {
    loadClientIdentity();
    loadLoadBalancing();
    loadAuthKeys();
  }

  // 本页标签用 t() 在渲染时拼进 textContent(无 data-i18n 属性),故切换语言时
  // K.i18n.apply(document) 抓不到——需重建整页才能刷新为新语言(与账号页同思路)。
  var langWired = false;
  function wireLangSwitch() {
    if (langWired) return;
    langWired = true;
    window.addEventListener('langchange', function () {
      if (container && document.getElementById('sec-settings')) {
        buildShell();
        loadAll();
      }
    });
  }

  K.sections.settings = {
    init: function () { buildShell(); wireLangSwitch(); },
    onShow: function () { if (!container) buildShell(); loadAll(); },
    onHide: function () {}
  };
})();

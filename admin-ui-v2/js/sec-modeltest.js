/* kiro2api Admin UI v2 — sec-modeltest.js
   Renders the Model Test / Playground section (<section id="sec-modeltest">).

   A chat-style playground modelled on gemini2api's playground:
     • LEFT panel  — request config: API-key select, model select, endpoint,
                     stream toggle, prompt textarea, image attach, actions.
     • RIGHT panel — a scrolling CONVERSATION of user / assistant bubbles.

   Registers K.sections.modeltest = { init, onShow, onHide } and exposes
   window.renderModelTest() for the app router / manual refresh.

   ── MULTI-TURN CONTEXT ─────────────────────────────────────────────────────
   state.history is a running array of OpenAI-shaped messages
     [{ role:'user'|'assistant', content: <string | contentPart[]> }, …]
   On every Send we append the user turn, POST the FULL history to
   /v1/chat/completions (or the Anthropic-shaped equivalent to /v1/messages),
   then append the assistant reply — so context is preserved across turns.
   "New conversation" (and Send with no history rebuilt) clears state.history.

   ── IMAGE SUPPORT ──────────────────────────────────────────────────────────
   Attached images are read as base64 data URLs and included as OpenAI vision
   content parts  { type:'image_url', image_url:{ url:<dataURL> } }  inside the
   user message. When any image is attached, the user turn's `content` becomes
   an array of parts ([{type:'text',text}, {type:'image_url',…}, …]); a
   text-only turn stays a plain string. For the Anthropic endpoint the same
   history is converted to Anthropic content blocks at request time.

   ── KEY HANDLING ───────────────────────────────────────────────────────────
   The playground calls the RELAY endpoint (POST /v1/chat/completions or
   /v1/messages). Relay auth needs a valid API key as
   `Authorization: Bearer <key>`. We fetch existing keys from the ADMIN endpoint
   GET /api/admin/api-keys (full key in `key`) and offer them in a <select>;
   the option VALUE is the full key. The admin key (localStorage
   'kiro2api_admin_key') is only ever used to read the models catalog and the
   api-keys list — never sent to /v1/*. The chosen relay key is only sent to
   /v1/* — never to /api/admin/*.

   Model list is pulled from GET /api/admin/models
   ({object:"list", data:[{id, display_name,...}]}) via K.api.

   ── TOKEN STATS ────────────────────────────────────────────────────────────
   After each reply we read the response's `usage` object
   ({prompt_tokens, completion_tokens, total_tokens} for OpenAI, or
   {input_tokens, output_tokens} for Anthropic — incl. streaming message_delta)
   and render latency + prompt/completion/total next to the conversation.

   Safety: every dynamic value (key names, model names, message text, error
   bodies, token counts) is written via textContent — never innerHTML — so a
   hostile value can never inject markup. Images are rendered by assigning
   `img.src` to the (locally produced) data URL. The static shell markup is
   assigned once and localized through data-i18n / K.i18n.apply(). */
(function () {
  'use strict';

  var K = window.K = window.K || {};
  K.sections = K.sections || {};

  var SEC_ID = 'sec-modeltest';
  var RELAY_BASE = '/v1';
  var MAX_IMAGE_BYTES = 8 * 1024 * 1024; // 8 MiB per image (post-base64 guard)
  var els = {};
  var inited = false;

  var state = {
    models: [],          // [{id, display_name}]
    modelsLoaded: false,
    keys: [],            // [{id, key, name, enabled}]
    keysLoaded: false,
    endpoint: 'chat',    // 'chat' (OpenAI) | 'messages' (Anthropic)
    stream: false,
    sending: false,
    abort: null,
    history: [],         // running conversation (OpenAI-shaped messages)
    pending: []          // attached images for the NEXT turn: [{url, name}]
  };

  // ---------------- helpers ----------------
  function t(key, vars) { return K.i18n ? K.i18n.t(key, vars) : key; }
  function fmtInt(n) { return (Math.round(Number(n) || 0)).toLocaleString(); }
  function fmtMs(ms) { return (Math.round(Number(ms) || 0)).toLocaleString() + ' ms'; }
  function toast(msg, type) { if (K.api && K.api.toast) K.api.toast(msg, type); }
  function maskKey(k) {
    k = String(k || '');
    if (k.length <= 10) return k;
    return k.slice(0, 6) + '…' + k.slice(-4);
  }
  function el(tag, cls) { var e = document.createElement(tag); if (cls) e.className = cls; return e; }

  // ---------------- static template ----------------
  function template() {
    return '' +
      '<div class="mt-head">' +
        '<h2 data-i18n="mt.title">Model Test</h2>' +
      '</div>' +

      '<div class="mt-layout">' +

        // ── LEFT: request config ──
        '<div class="card mt-config">' +
          '<h3 class="mt-panel-title" data-i18n="mt.configTitle">Request</h3>' +

          // api key select (full key value pulled from GET /api/admin/api-keys)
          '<div class="form-group">' +
            '<label class="form-label" for="mtKey" data-i18n="mt.key">API Key</label>' +
            '<div class="mt-key-row">' +
              '<select id="mtKey" class="form-control">' +
                '<option value="" data-i18n="common.loading">Loading…</option>' +
              '</select>' +
              '<button type="button" class="btn btn-outline btn-sm" id="mtReloadKeys" ' +
                      'data-i18n-title="common.refresh" title="Refresh">↻</button>' +
            '</div>' +
            '<span class="form-hint" id="mtKeyHint" data-i18n="mt.keyHint">' +
              'Select an existing API key to authorize the test request</span>' +
          '</div>' +

          // model select
          '<div class="form-group">' +
            '<label class="form-label" for="mtModel" data-i18n="mt.model">Model</label>' +
            '<div class="mt-model-row">' +
              '<select id="mtModel" class="form-control">' +
                '<option value="" data-i18n="common.loading">Loading…</option>' +
              '</select>' +
              '<button type="button" class="btn btn-outline btn-sm" id="mtReloadModels" ' +
                      'data-i18n-title="common.refresh" title="Refresh">↻</button>' +
            '</div>' +
          '</div>' +

          // endpoint radio
          '<div class="form-group">' +
            '<label class="form-label" data-i18n="mt.endpoint">Endpoint</label>' +
            '<div class="mt-endpoint" id="mtEndpoint">' +
              '<label class="mt-radio"><input type="radio" name="mtEp" value="chat" checked>' +
                '<span data-i18n="mt.epChat">/v1/chat/completions (OpenAI)</span></label>' +
              '<label class="mt-radio"><input type="radio" name="mtEp" value="messages">' +
                '<span data-i18n="mt.epMessages">/v1/messages (Anthropic)</span></label>' +
            '</div>' +
          '</div>' +

          // stream toggle
          '<div class="form-group">' +
            '<label class="mt-check"><input type="checkbox" id="mtStream">' +
              '<span data-i18n="mt.stream">Stream response</span></label>' +
          '</div>' +

          // prompt
          '<div class="form-group">' +
            '<label class="form-label" for="mtPrompt" data-i18n="mt.prompt">Prompt</label>' +
            '<textarea id="mtPrompt" class="form-control mt-prompt" rows="6" ' +
                      'data-i18n-ph="mt.promptPh" placeholder="Type a message to test the model…"></textarea>' +
            '<span class="form-hint" data-i18n="mt.kbdHint">Enter to send · Ctrl+Enter for a new line</span>' +
          '</div>' +

          // images
          '<div class="form-group">' +
            '<label class="form-label" data-i18n="mt.images">Images</label>' +
            '<div id="mtImagePreview" class="mt-image-preview"></div>' +
            '<button type="button" class="btn btn-outline btn-sm" id="mtImageBtn">' +
              '<span data-i18n="mt.addImage">Add image</span></button>' +
            '<input type="file" id="mtImageInput" accept="image/*" multiple hidden>' +
          '</div>' +

          // actions
          '<div class="mt-actions">' +
            '<button type="button" class="btn btn-primary" id="mtSendBtn" data-i18n="mt.send">Send</button>' +
            '<button type="button" class="btn btn-danger" id="mtStopBtn" hidden data-i18n="mt.stop">Stop</button>' +
            '<button type="button" class="btn btn-outline" id="mtNewChatBtn" data-i18n="mt.newChat">New conversation</button>' +
          '</div>' +
        '</div>' +

        // ── RIGHT: conversation ──
        '<div class="card mt-response-card">' +
          '<div class="mt-response-head">' +
            '<h3 class="mt-panel-title" data-i18n="mt.chat">Conversation</h3>' +
            '<div class="mt-meta" id="mtMeta" hidden>' +
              '<span class="mt-meta-item"><span class="mt-meta-label" data-i18n="mt.usedModel">Model</span>' +
                '<span class="mt-meta-val" id="mtUsedModel">—</span></span>' +
              '<span class="mt-meta-item"><span class="mt-meta-label" data-i18n="mt.latency">Latency</span>' +
                '<span class="mt-meta-val" id="mtLatency">—</span></span>' +
              '<span class="mt-meta-item"><span class="mt-meta-label" data-i18n="mt.promptTokens">Prompt</span>' +
                '<span class="mt-meta-val" id="mtPromptTokens">—</span></span>' +
              '<span class="mt-meta-item"><span class="mt-meta-label" data-i18n="mt.completionTokens">Completion</span>' +
                '<span class="mt-meta-val" id="mtCompletionTokens">—</span></span>' +
              '<span class="mt-meta-item"><span class="mt-meta-label" data-i18n="mt.totalTokens">Total</span>' +
                '<span class="mt-meta-val" id="mtTotalTokens">—</span></span>' +
            '</div>' +
          '</div>' +
          '<div class="mt-chat" id="mtChat"></div>' +
        '</div>' +

      '</div>';
  }

  // ---------------- init ----------------
  function init() {
    if (inited) return;
    var host = document.getElementById(SEC_ID);
    if (!host) return;
    host.innerHTML = template();

    els.key = host.querySelector('#mtKey');
    els.reloadKeys = host.querySelector('#mtReloadKeys');
    els.keyHint = host.querySelector('#mtKeyHint');
    els.model = host.querySelector('#mtModel');
    els.reloadModels = host.querySelector('#mtReloadModels');
    els.endpoint = host.querySelector('#mtEndpoint');
    els.stream = host.querySelector('#mtStream');
    els.prompt = host.querySelector('#mtPrompt');
    els.imagePreview = host.querySelector('#mtImagePreview');
    els.imageBtn = host.querySelector('#mtImageBtn');
    els.imageInput = host.querySelector('#mtImageInput');
    els.sendBtn = host.querySelector('#mtSendBtn');
    els.stopBtn = host.querySelector('#mtStopBtn');
    els.newChatBtn = host.querySelector('#mtNewChatBtn');
    els.meta = host.querySelector('#mtMeta');
    els.usedModel = host.querySelector('#mtUsedModel');
    els.latency = host.querySelector('#mtLatency');
    els.promptTokens = host.querySelector('#mtPromptTokens');
    els.completionTokens = host.querySelector('#mtCompletionTokens');
    els.totalTokens = host.querySelector('#mtTotalTokens');
    els.chat = host.querySelector('#mtChat');

    els.reloadKeys.addEventListener('click', function () { loadKeys(true); });
    els.reloadModels.addEventListener('click', function () { loadModels(true); });

    els.endpoint.addEventListener('change', function (e) {
      var r = e.target.closest('input[name="mtEp"]');
      if (r) state.endpoint = r.value;
    });
    els.stream.addEventListener('change', function () { state.stream = els.stream.checked; });

    els.sendBtn.addEventListener('click', send);
    els.stopBtn.addEventListener('click', stop);
    els.newChatBtn.addEventListener('click', newConversation);

    // images
    els.imageBtn.addEventListener('click', function () { els.imageInput.click(); });
    els.imageInput.addEventListener('change', onImagePicked);

    // KEYBOARD: Enter = send, Ctrl/Cmd+Enter = newline
    els.prompt.addEventListener('keydown', function (e) {
      if (e.key !== 'Enter') return;
      if (e.ctrlKey || e.metaKey) {
        // Ctrl+Enter → insert a newline at the caret
        e.preventDefault();
        insertNewline(els.prompt);
      } else if (!e.shiftKey && !e.altKey && !e.isComposing) {
        // plain Enter → send
        e.preventDefault();
        send();
      }
      // Shift+Enter keeps the browser's default (newline) too
    });

    resetChat();

    if (K.i18n) K.i18n.apply(host);
    window.addEventListener('langchange', function () {
      paintKeyHint();
      if (!state.history.length) resetChat(); // relocalize empty-state placeholder
    });

    inited = true;
  }

  function insertNewline(ta) {
    var start = ta.selectionStart, end = ta.selectionEnd, v = ta.value;
    ta.value = v.slice(0, start) + '\n' + v.slice(end);
    ta.selectionStart = ta.selectionEnd = start + 1;
  }

  // ---------------- api keys ----------------
  function loadKeys(force) {
    if (!force && state.keysLoaded && state.keys.length) { fillKeySelect(); return; }
    setKeySelectLoading();
    K.api.get('/api-keys').then(function (data) {
      var list = [];
      if (Array.isArray(data)) list = data;
      else if (data && Array.isArray(data.data)) list = data.data;
      else if (data && Array.isArray(data.keys)) list = data.keys;
      state.keys = list.map(function (k) {
        return {
          id: k.id,
          key: k.key || k.apiKey || '',
          name: k.name || '',
          enabled: k.enabled !== false
        };
      }).filter(function (k) { return !!k.key; });
      state.keysLoaded = true;
      fillKeySelect();
    }).catch(function () {
      state.keysLoaded = false;
      setKeySelectError();
    });
  }

  function simpleOption(value, i18nKey) {
    var o = document.createElement('option');
    o.value = value || '';
    o.textContent = t(i18nKey);
    return o;
  }

  function setKeySelectLoading() {
    els.key.innerHTML = '';
    els.key.appendChild(simpleOption('', 'common.loading'));
    els.key.disabled = true;
    paintKeyHint('loading');
  }
  function setKeySelectError() {
    els.key.innerHTML = '';
    els.key.appendChild(simpleOption('', 'common.error'));
    els.key.disabled = true;
    paintKeyHint('error');
  }
  function fillKeySelect() {
    var prev = els.key.value;
    els.key.innerHTML = '';
    if (!state.keys.length) {
      els.key.appendChild(simpleOption('', 'mt.keyEmptyOption'));
      els.key.disabled = true;
      paintKeyHint('empty');
      return;
    }
    state.keys.forEach(function (k) {
      var o = document.createElement('option');
      o.value = k.key;                       // FULL key is the option value
      var label = (k.name || ('#' + (k.id != null ? k.id : ''))) + ' · ' + maskKey(k.key);
      if (!k.enabled) label += ' (' + t('common.disabled') + ')';
      o.textContent = label;                 // textContent — XSS-safe
      els.key.appendChild(o);
    });
    els.key.disabled = false;
    if (prev && state.keys.some(function (k) { return k.key === prev; })) els.key.value = prev;
    paintKeyHint('ready');
  }

  function paintKeyHint() {
    if (!els.keyHint) return;
    var empty = state.keysLoaded && !state.keys.length;
    var key = empty ? 'mt.keyEmpty' : 'mt.keyHint';
    els.keyHint.setAttribute('data-i18n', key);
    els.keyHint.textContent = t(key);
    els.keyHint.classList.toggle('mt-hint-warn', !!empty);
  }

  // ---------------- models ----------------
  function loadModels(force) {
    if (!force && K.models && K.models.length) {
      state.models = K.models.slice();
      state.modelsLoaded = true;
      fillModelSelect();
      return;
    }
    setModelSelectLoading();
    K.api.get('/models').then(function (data) {
      var list = (data && Array.isArray(data.data)) ? data.data : [];
      state.models = list.map(function (m) {
        return { id: m.id, display_name: m.display_name || m.id };
      });
      state.modelsLoaded = true;
      K.models = state.models.slice();
      fillModelSelect();
    }).catch(function () {
      state.modelsLoaded = false;
      setModelSelectError();
    });
  }

  function setModelSelectLoading() {
    els.model.innerHTML = '';
    els.model.appendChild(simpleOption('', 'common.loading'));
    els.model.disabled = true;
  }
  function setModelSelectError() {
    els.model.innerHTML = '';
    els.model.appendChild(simpleOption('', 'common.error'));
    els.model.disabled = true;
  }
  function fillModelSelect() {
    var prev = els.model.value;
    els.model.innerHTML = '';
    if (!state.models.length) { setModelSelectError(); return; }
    state.models.forEach(function (m) {
      var o = document.createElement('option');
      o.value = m.id;
      o.textContent = m.display_name;   // textContent — XSS-safe
      els.model.appendChild(o);
    });
    els.model.disabled = false;
    if (prev && state.models.some(function (m) { return m.id === prev; })) els.model.value = prev;
  }

  // ---------------- images (attach for next turn) ----------------
  function onImagePicked(e) {
    var files = Array.prototype.slice.call(e.target.files || []);
    e.target.value = ''; // allow re-picking the same file
    files.forEach(function (f) {
      if (!f.type || f.type.indexOf('image/') !== 0) return;
      var reader = new FileReader();
      reader.onload = function () {
        var url = String(reader.result || '');
        if (!url) return;
        if (url.length > MAX_IMAGE_BYTES) { toast(t('mt.imageTooLarge'), 'error'); return; }
        state.pending.push({ url: url, name: f.name || 'image' });
        renderPendingImages();
      };
      reader.readAsDataURL(f);
    });
  }

  function renderPendingImages() {
    els.imagePreview.innerHTML = '';
    state.pending.forEach(function (img, idx) {
      var thumb = el('div', 'mt-img-thumb');
      var im = el('img');
      im.src = img.url;                 // locally produced data URL
      im.alt = img.name;
      thumb.appendChild(im);
      var del = el('button', 'mt-img-del');
      del.type = 'button';
      del.textContent = '×';
      del.setAttribute('data-i18n-title', 'mt.removeImage');
      del.title = t('mt.removeImage');
      del.addEventListener('click', function () {
        state.pending.splice(idx, 1);
        renderPendingImages();
      });
      thumb.appendChild(del);
      els.imagePreview.appendChild(thumb);
    });
  }

  // ---------------- conversation rendering ----------------
  function resetChat() {
    els.chat.innerHTML = '';
    var ph = el('div', 'mt-chat-placeholder');
    var p = el('p');
    p.textContent = t('mt.startChat');
    ph.appendChild(p);
    els.chat.appendChild(ph);
  }

  function clearPlaceholder() {
    var ph = els.chat.querySelector('.mt-chat-placeholder');
    if (ph) ph.remove();
  }

  // Render one message from history-part content (string | contentPart[]).
  // Returns { bubble, textEl } so a streaming assistant reply can grow.
  function appendMessage(role, content) {
    clearPlaceholder();
    var row = el('div', 'mt-msg mt-msg-' + role);
    var avatar = el('div', 'mt-avatar');
    avatar.textContent = role === 'user' ? t('mt.you') : 'AI';
    var bubble = el('div', 'mt-bubble');

    var text = '';
    var images = [];
    if (typeof content === 'string') {
      text = content;
    } else if (Array.isArray(content)) {
      content.forEach(function (part) {
        if (!part) return;
        if (part.type === 'text') text += (part.text || '');
        else if (part.type === 'image_url' && part.image_url && part.image_url.url) {
          images.push(part.image_url.url);
        }
      });
    }

    var textEl = el('span', 'mt-bubble-text');
    textEl.textContent = text;
    bubble.appendChild(textEl);

    images.forEach(function (url) {
      var im = el('img', 'mt-chat-img');
      im.src = url;
      bubble.appendChild(im);
    });

    if (role === 'user') { row.appendChild(bubble); row.appendChild(avatar); }
    else { row.appendChild(avatar); row.appendChild(bubble); }

    els.chat.appendChild(row);
    scrollChat();
    return { row: row, bubble: bubble, textEl: textEl };
  }

  function appendTyping() {
    clearPlaceholder();
    var row = el('div', 'mt-msg mt-msg-assistant');
    var avatar = el('div', 'mt-avatar');
    avatar.textContent = 'AI';
    var bubble = el('div', 'mt-bubble');
    var textEl = el('span', 'mt-bubble-text');
    bubble.appendChild(textEl);
    var cursor = el('span', 'mt-typing-cursor');
    bubble.appendChild(cursor);
    row.appendChild(avatar);
    row.appendChild(bubble);
    els.chat.appendChild(row);
    scrollChat();
    return { row: row, bubble: bubble, textEl: textEl, cursor: cursor };
  }

  function appendError(msg) {
    clearPlaceholder();
    var row = el('div', 'mt-msg mt-msg-assistant');
    var avatar = el('div', 'mt-avatar');
    avatar.textContent = 'AI';
    var box = el('div', 'mt-error');
    var title = el('div', 'mt-error-title');
    title.textContent = t('mt.requestFailed');
    var body = el('pre', 'mt-error-body');
    body.textContent = msg;                 // real backend error, XSS-safe
    box.appendChild(title);
    box.appendChild(body);
    row.appendChild(avatar);
    row.appendChild(box);
    els.chat.appendChild(row);
    scrollChat();
  }

  function scrollChat() { els.chat.scrollTop = els.chat.scrollHeight; }

  // ---------------- meta / tokens ----------------
  function clearMeta() {
    els.meta.hidden = true;
    els.usedModel.textContent = '—';
    els.latency.textContent = '—';
    els.promptTokens.textContent = '—';
    els.completionTokens.textContent = '—';
    els.totalTokens.textContent = '—';
  }

  // Robustly render latency + full token triple. `usage` is the raw response
  // usage object (OpenAI: prompt_tokens/completion_tokens/total_tokens;
  // Anthropic: input_tokens/output_tokens). Missing fields show as “—”.
  function setMeta(latencyMs, usage, usedModel) {
    els.meta.hidden = false;
    // Show the model the RESPONSE actually reports it was served by, so routing
    // is transparent (dropdown selection vs. what really answered). Dynamic value
    // via textContent — XSS-safe.
    els.usedModel.textContent = usedModel ? String(usedModel) : '—';
    els.latency.textContent = (latencyMs != null) ? fmtMs(latencyMs) : '—';

    usage = usage || {};
    var prompt = (usage.prompt_tokens != null) ? usage.prompt_tokens
               : (usage.input_tokens != null) ? usage.input_tokens : null;
    var completion = (usage.completion_tokens != null) ? usage.completion_tokens
                   : (usage.output_tokens != null) ? usage.output_tokens : null;
    var total = (usage.total_tokens != null) ? usage.total_tokens : null;
    if (total == null && (prompt != null || completion != null)) {
      total = (Number(prompt) || 0) + (Number(completion) || 0);
    }

    els.promptTokens.textContent = (prompt != null) ? fmtInt(prompt) : '—';
    els.completionTokens.textContent = (completion != null) ? fmtInt(completion) : '—';
    els.totalTokens.textContent = (total != null) ? fmtInt(total) : '—';
  }

  // ---------------- history → request payload ----------------
  // OpenAI: history parts are already OpenAI-shaped, send verbatim.
  // Anthropic: convert each message's content to Anthropic blocks.
  function toAnthropicMessages(history) {
    return history.map(function (m) {
      var content;
      if (typeof m.content === 'string') {
        content = m.content;
      } else if (Array.isArray(m.content)) {
        content = m.content.map(function (part) {
          if (part.type === 'text') return { type: 'text', text: part.text || '' };
          if (part.type === 'image_url' && part.image_url && part.image_url.url) {
            var url = part.image_url.url;
            var mm = /^data:([^;]+);base64,(.*)$/.exec(url);
            if (mm) {
              return { type: 'image', source: { type: 'base64', media_type: mm[1], data: mm[2] } };
            }
            return { type: 'image', source: { type: 'url', url: url } };
          }
          return { type: 'text', text: '' };
        });
      } else {
        content = '';
      }
      return { role: m.role, content: content };
    });
  }

  // ---------------- relay request ----------------
  function relayFetch(path, apiKey, body, signal) {
    var headers = {
      'Content-Type': 'application/json',
      'Authorization': 'Bearer ' + apiKey,
      'x-api-key': apiKey
    };
    return fetch(RELAY_BASE + path, {
      method: 'POST', headers: headers, body: JSON.stringify(body), signal: signal
    });
  }

  // ---------------- send state ----------------
  function setSending(on) {
    state.sending = on;
    els.sendBtn.hidden = on;
    els.stopBtn.hidden = !on;
    els.prompt.disabled = on;
    els.imageBtn.disabled = on;
    els.newChatBtn.disabled = on;
    els.key.disabled = on || !state.keys.length;
    els.model.disabled = on || !state.models.length;
  }

  function stop() {
    if (state.abort) { try { state.abort.abort(); } catch (e) {} }
  }

  function newConversation() {
    if (state.sending) stop();
    state.history = [];
    state.pending = [];
    renderPendingImages();
    resetChat();
    clearMeta();
  }

  // ---------------- send ----------------
  async function send() {
    if (state.sending) return;

    var apiKey = els.key.value;
    if (!apiKey) { toast(t('mt.noKey'), 'error'); els.key.focus(); return; }
    var model = els.model.value;
    if (!model) { toast(t('mt.noModel'), 'error'); els.model.focus(); return; }
    var text = (els.prompt.value || '').trim();
    var images = state.pending.slice();
    if (!text && !images.length) { els.prompt.focus(); return; }

    // Build the user message content: plain string when text-only, else
    // an OpenAI vision content-part array.
    var userContent;
    if (images.length) {
      userContent = [];
      if (text) userContent.push({ type: 'text', text: text });
      images.forEach(function (img) {
        userContent.push({ type: 'image_url', image_url: { url: img.url } });
      });
    } else {
      userContent = text;
    }

    // Append the user turn to history + UI, clear the composer.
    state.history.push({ role: 'user', content: userContent });
    appendMessage('user', userContent);
    els.prompt.value = '';
    state.pending = [];
    renderPendingImages();

    var isAnthropic = state.endpoint === 'messages';
    var path = isAnthropic ? '/messages' : '/chat/completions';
    var messages = isAnthropic ? toAnthropicMessages(state.history) : state.history;
    var body;
    if (isAnthropic) {
      // Anthropic emits usage in the streamed message_start/message_delta events,
      // so no opt-in flag is needed for token stats on the /v1/messages path.
      body = { model: model, max_tokens: 1024, stream: state.stream, messages: messages };
    } else {
      body = { model: model, stream: state.stream, messages: messages };
      // OpenAI streaming omits `usage` by DEFAULT — must opt in so the FINAL
      // chunk carries a usage object we can render (token triple in stream mode).
      if (state.stream) body.stream_options = { include_usage: true };
    }

    state.abort = new AbortController();
    setSending(true);
    clearMeta();

    var typing = appendTyping();
    var t0 = performance.now();
    var res;
    try {
      res = await relayFetch(path, apiKey, body, state.abort.signal);
    } catch (e) {
      typing.row.remove();
      if (e && e.name === 'AbortError') { rollbackUserTurn(); finishAbort(); return; }
      setSending(false); state.abort = null;
      appendError(t('common.error'));
      return;
    }

    if (!res.ok) {
      typing.row.remove();
      var errText = await res.text().catch(function () { return ''; });
      var parsed = errText;
      try {
        var j = JSON.parse(errText);
        parsed = (j && j.error && (j.error.message || j.error)) || (j && j.message) || errText;
        if (typeof parsed === 'object') parsed = JSON.stringify(parsed, null, 2);
      } catch (e2) { /* keep raw text */ }
      setSending(false); state.abort = null;
      appendError('HTTP ' + res.status + (parsed ? '\n' + parsed : ''));
      toast(t('mt.requestFailed'), 'error');
      return;
    }

    try {
      if (state.stream) {
        await consumeStream(res, isAnthropic, t0, typing);
      } else {
        typing.row.remove();
        await consumeJson(res, isAnthropic, t0);
      }
    } catch (e) {
      if (typing.row.parentNode) typing.row.remove();
      if (e && e.name === 'AbortError') { finishAbort(); return; }
      appendError((e && e.message) ? String(e.message) : t('common.error'));
    } finally {
      setSending(false);
      state.abort = null;
    }
  }

  // If the very first request is aborted before any reply, drop the user turn
  // we optimistically pushed so history stays consistent.
  function rollbackUserTurn() {
    if (state.history.length && state.history[state.history.length - 1].role === 'user') {
      // keep it — user still sees their message; do NOT pop so a retry keeps
      // context. (No-op kept intentionally; see finishAbort for cursor cleanup.)
    }
  }

  function finishAbort() {
    setSending(false);
    state.abort = null;
    var cur = els.chat.querySelector('.mt-typing-cursor');
    if (cur) cur.remove();
  }

  // ---- non-streaming ----
  async function consumeJson(res, isAnthropic, t0) {
    var data = await res.json();
    var latency = performance.now() - t0;
    var text = '';
    if (isAnthropic) {
      if (data && Array.isArray(data.content)) {
        data.content.forEach(function (b) { if (b && b.type === 'text') text += (b.text || ''); });
      }
    } else {
      text = (data && data.choices && data.choices[0] && data.choices[0].message
              && data.choices[0].message.content) || '';
    }
    appendMessage('assistant', text || '');
    state.history.push({ role: 'assistant', content: text || '' });
    // TOKEN STATS: read straight off response.usage.
    // MODEL: response echoes the model that actually served the request.
    setMeta(latency, data && data.usage, data && data.model);
  }

  // ---- streaming (SSE over fetch body) ----
  async function consumeStream(res, isAnthropic, t0, typing) {
    var reader = res.body.getReader();
    var decoder = new TextDecoder();
    var buf = '';
    var firstByteMs = null;
    var acc = '';
    var usage = null;
    var usedModel = null;

    for (;;) {
      var chunk = await reader.read();
      if (chunk.done) break;
      if (firstByteMs == null) firstByteMs = performance.now() - t0;
      buf += decoder.decode(chunk.value, { stream: true });

      var parts = buf.split(/\r?\n\r?\n/);
      buf = parts.pop();
      for (var i = 0; i < parts.length; i++) {
        var lines = parts[i].split(/\r?\n/);
        for (var jx = 0; jx < lines.length; jx++) {
          var line = lines[jx];
          if (line.indexOf('data:') !== 0) continue;
          var payload = line.slice(5).trim();
          if (!payload) continue;
          if (payload === '[DONE]') { finalizeStream(typing, acc, usage, t0, usedModel); return; }
          var evt;
          try { evt = JSON.parse(payload); } catch (e) { continue; }
          usage = mergeUsage(usage, evt, isAnthropic);
          usedModel = extractModel(usedModel, evt, isAnthropic);
          var piece = extractDelta(evt, isAnthropic);
          if (piece) { acc += piece; typing.textEl.textContent = acc; scrollChat(); }
        }
      }
    }
    finalizeStream(typing, acc, usage, t0, usedModel);
  }

  // Pull the served model id out of stream events. OpenAI chunks carry a
  // top-level `model`; Anthropic reports it inside the message_start event.
  function extractModel(current, evt, isAnthropic) {
    if (current) return current;
    if (isAnthropic) {
      if (evt.type === 'message_start' && evt.message && evt.message.model) return evt.message.model;
      return current;
    }
    return (typeof evt.model === 'string' && evt.model) ? evt.model : current;
  }

  function extractDelta(evt, isAnthropic) {
    if (isAnthropic) {
      if (evt.type === 'content_block_delta' && evt.delta && typeof evt.delta.text === 'string') {
        return evt.delta.text;
      }
      return '';
    }
    var d = evt.choices && evt.choices[0] && evt.choices[0].delta;
    return (d && typeof d.content === 'string') ? d.content : '';
  }

  // Accumulate usage from streamed events into a single OpenAI/Anthropic-shaped
  // object so setMeta can read it uniformly.
  function mergeUsage(usage, evt, isAnthropic) {
    if (isAnthropic) {
      if (evt.type === 'message_start' && evt.message && evt.message.usage) {
        usage = usage || {};
        if (evt.message.usage.input_tokens != null) usage.input_tokens = evt.message.usage.input_tokens;
        if (evt.message.usage.output_tokens != null) usage.output_tokens = evt.message.usage.output_tokens;
      }
      if (evt.type === 'message_delta' && evt.usage) {
        usage = usage || {};
        if (evt.usage.output_tokens != null) usage.output_tokens = evt.usage.output_tokens;
        if (evt.usage.input_tokens != null) usage.input_tokens = evt.usage.input_tokens;
      }
      return usage;
    }
    if (evt.usage) {
      usage = usage || {};
      if (evt.usage.prompt_tokens != null) usage.prompt_tokens = evt.usage.prompt_tokens;
      if (evt.usage.completion_tokens != null) usage.completion_tokens = evt.usage.completion_tokens;
      if (evt.usage.total_tokens != null) usage.total_tokens = evt.usage.total_tokens;
    }
    return usage;
  }

  function finalizeStream(typing, acc, usage, t0, usedModel) {
    if (typing.cursor) typing.cursor.remove();
    typing.textEl.textContent = acc || '';
    state.history.push({ role: 'assistant', content: acc || '' });
    var total = performance.now() - t0;
    setMeta(total, usage, usedModel);
  }

  // ---------------- register + expose ----------------
  K.sections.modeltest = {
    init: init,
    onShow: function () {
      if (!inited) init();
      if (!state.keysLoaded) loadKeys(false);
      if (!state.modelsLoaded) loadModels(false);
    },
    onHide: function () {
      if (state.abort) { try { state.abort.abort(); } catch (e) {} }
    }
  };

  window.renderModelTest = function () {
    if (!inited) init();
    loadKeys(false);
    loadModels(false);
  };
})();

/* kiro2api Admin UI v2 — sec-models.js
   Models section (#sec-models): the standalone "Supported Models" page.

   Data:
     - GET  /api/admin/models                      -> { data:[ {id, display_name,
                                                        owned_by, type, max_tokens,
                                                        rate_multiplier} ] }
     - POST /api/admin/credentials/models/refresh  -> { refreshed, failed,
                                                        errors:[ {email/credentialId,
                                                        reason} ] } (per-account)

   Renders a card holding a responsive table of model rows (id / display name /
   type / owned_by / max_tokens / rate multiplier). The refresh button re-pulls
   models across every account and surfaces the backend's per-account errors[].

   Public entry: registers K.sections.models = { init, onShow, onHide }.
   Dynamic values are written with textContent; every static string carries a
   data-i18n key and is filled by K.i18n.apply(container). */
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
  function i18nEl(tag, cls, key) {
    var n = el(tag, cls);
    n.setAttribute('data-i18n', key);
    n.textContent = t(key);
    return n;
  }
  function fmtInt(n) {
    if (n == null || isNaN(n)) return '—';
    try { return Number(n).toLocaleString(); } catch (e) { return String(n); }
  }

  // ---------- state ----------
  var container = null;
  var tbody = null;
  var refreshBtn = null;

  function buildShell() {
    container = document.getElementById('sec-models');
    if (!container) return;
    container.innerHTML = '';

    // header
    var header = el('div', 'section-header');
    header.appendChild(i18nEl('h2', null, 'models.title'));
    var actions = el('div', 'section-actions');
    refreshBtn = el('button', 'btn btn-primary');
    refreshBtn.type = 'button';
    refreshBtn.setAttribute('data-i18n', 'models.refresh');
    refreshBtn.textContent = t('models.refresh');
    refreshBtn.addEventListener('click', function () { refreshModels(refreshBtn); });
    actions.appendChild(refreshBtn);
    header.appendChild(actions);
    container.appendChild(header);

    // description
    container.appendChild(i18nEl('p', 'settings-card-desc', 'models.desc'));

    // card + table
    var card = el('div', 'card');
    var body = el('div', 'card-body');
    var scroll = el('div', 'table-scroll models-table-scroll');
    var table = el('table', 'data-table models-table');

    var thead = el('thead');
    var hr = el('tr');
    hr.appendChild(i18nEl('th', null, 'models.col.id'));
    hr.appendChild(i18nEl('th', null, 'models.col.name'));
    hr.appendChild(i18nEl('th', null, 'models.col.type'));
    hr.appendChild(i18nEl('th', null, 'models.col.ownedBy'));
    var thTokens = i18nEl('th', 'ta-right', 'models.col.maxTokens');
    var thRate = i18nEl('th', 'ta-right', 'models.col.rate');
    hr.appendChild(thTokens);
    hr.appendChild(thRate);
    thead.appendChild(hr);
    table.appendChild(thead);

    tbody = el('tbody');
    table.appendChild(tbody);
    scroll.appendChild(table);
    body.appendChild(scroll);
    card.appendChild(body);
    container.appendChild(card);

    renderLoading();
    if (K.i18n) K.i18n.apply(container);
  }

  function stateRow(key) {
    tbody.innerHTML = '';
    var tr = el('tr', 'models-state-row');
    var td = el('td');
    td.setAttribute('colspan', '6');
    var box = i18nEl('div', 'empty-state', key);
    td.appendChild(box);
    tr.appendChild(td);
    tbody.appendChild(tr);
  }
  function renderLoading() { if (tbody) stateRow('common.loading'); }

  // ---------- load ----------
  function loadModels() {
    if (!tbody) return;
    renderLoading();
    api.get('/models').then(function (r) {
      var list = (r && (r.data || r.models)) || [];
      K.models = list;
      renderRows(list);
    }).catch(function (e) {
      stateRow('common.error');
      api.toast((e && e.message) || t('common.error'), 'error');
    });
  }

  function renderRows(list) {
    tbody.innerHTML = '';
    if (!list || !list.length) { stateRow('common.empty'); return; }
    list.forEach(function (m) {
      var id = m.id || m.model || '';
      var name = m.display_name || m.displayName || id;
      var type = m.type || m.object || '';
      var owner = m.owned_by || m.ownedBy || m.owner || '';
      var maxTok = (m.max_tokens != null) ? m.max_tokens
        : (m.maxTokens != null ? m.maxTokens : null);
      var rate = (m.rate_multiplier != null) ? m.rate_multiplier
        : (m.rateMultiplier != null ? m.rateMultiplier : null);

      var row = el('tr', 'model-row');

      var tdId = el('td', 'model-cell-id');
      tdId.appendChild(el('code', 'kv-mono', id));
      row.appendChild(tdId);

      row.appendChild(el('td', 'model-cell-name', name || '—'));

      var tdType = el('td');
      if (type) {
        tdType.appendChild(el('span', 'model-tag', String(type)));
      } else {
        tdType.textContent = '—';
      }
      row.appendChild(tdType);

      row.appendChild(el('td', 'model-cell-owner', owner || '—'));

      var tdTok = el('td', 'ta-right tabular');
      tdTok.textContent = (maxTok != null) ? fmtInt(maxTok) : '—';
      row.appendChild(tdTok);

      var tdRate = el('td', 'ta-right tabular');
      tdRate.textContent = (rate != null) ? (Number(rate).toFixed(2) + 'x') : '—';
      row.appendChild(tdRate);

      tbody.appendChild(row);
    });
  }

  // ---------- refresh (across accounts, surface per-account errors[]) ----------
  // The refresh fans out over ALL accounts server-side and can take a while /
  // partially fail. It returns { refreshed, failed, errors[] }. We treat a
  // partial failure as SUCCESS-WITH-WARNINGS (summary + per-account errors[]),
  // NOT a fatal '发生错误'. A hard error toast is shown ONLY when the request
  // itself fails (network / non-2xx / client-side timeout).
  var REFRESH_TIMEOUT_MS = 120000;   // longer client timeout for the fan-out
  function refreshModels(btn) {
    if (btn) { btn.disabled = true; btn.textContent = t('common.loading'); }
    // clear any stale per-account error panel from a previous run
    if (container) {
      var stale = container.querySelector('.refresh-errors');
      if (stale) stale.remove();
    }

    // abort-based client timeout so a slow (but progressing) fan-out doesn't die
    // on a short default; only a real stall trips it.
    var ctrl = (typeof AbortController !== 'undefined') ? new AbortController() : null;
    var timedOut = false;
    var timer = ctrl ? setTimeout(function () { timedOut = true; ctrl.abort(); }, REFRESH_TIMEOUT_MS) : null;

    api.request('/credentials/models/refresh', {
      method: 'POST', body: {}, signal: ctrl ? ctrl.signal : undefined
    })
      .then(function (r) {
        r = r || {};
        var refreshed = r.refreshed != null ? r.refreshed
          : (r.success != null ? r.success : 0);
        var failed = r.failed != null ? r.failed
          : (r.errors ? r.errors.length : 0);
        var errors = r.errors || [];
        var tiers = Array.isArray(r.tiers) ? r.tiers : [];
        // 后端按订阅档位各刷一个代表账号(FREE/PRO...)+有界发现,故 refreshed>=1
        // 就代表已取得覆盖各档位的完整模型清单 = 成功;跳过/封禁号不算本操作失败
        // (那是账号本身问题)。仅当没有任何账号能提供模型(refreshed==0)才算失败并列出原因。
        if (refreshed >= 1) {
          if (tiers.length) {
            api.toast(t('models.refreshedTiers', {
              count: fmtInt(refreshed), tiers: fmtInt(tiers.length)
            }), 'success');
          } else {
            api.toast(t('models.refreshed', { count: fmtInt(refreshed) }), 'success');
          }
        } else {
          api.toast(t('models.refreshSummary', {
            ok: fmtInt(refreshed), failed: fmtInt(failed)
          }), 'error');
          if (failed) showRefreshErrors(refreshed, failed, errors);
        }
        loadModels();
      })
      .catch(function (e) {
        // hard failure of the request itself (abort/timeout, network, non-2xx)
        if (timedOut || (e && e.name === 'AbortError')) {
          api.toast(t('models.refreshTimeout'), 'error');
        } else {
          api.toast((e && e.message) || t('common.error'), 'error');
        }
      })
      .then(function () {
        if (timer) clearTimeout(timer);
        if (btn) { btn.disabled = false; btn.textContent = t('models.refresh'); }
      });
  }

  // per-account error panel injected below the header (dismissable)
  function showRefreshErrors(refreshed, failed, errors) {
    if (!container) return;
    var old = container.querySelector('.refresh-errors');
    if (old) old.remove();

    var box = el('div', 'refresh-errors');
    var head = el('div', 'rex-head');
    head.textContent = t('models.refreshErrors', {
      ok: fmtInt(refreshed), failed: fmtInt(failed)
    });
    box.appendChild(head);
    errors.forEach(function (er) {
      var item = el('div', 'rex-item');
      var acctLabel = er.email || er.credentialId || er.id || er.account || '?';
      item.appendChild(el('span', 'rex-acct', String(acctLabel)));
      item.appendChild(el('span', 'rex-reason',
        String(er.reason || er.error || er.message || t('common.error'))));
      box.appendChild(item);
    });
    // insert right after the description paragraph (or header)
    var anchor = container.querySelector('.settings-card-desc')
      || container.querySelector('.section-header');
    if (anchor && anchor.nextSibling) {
      container.insertBefore(box, anchor.nextSibling);
    } else {
      container.appendChild(box);
    }
  }

  // 标签用 t() 拼进 textContent(无 data-i18n),故切语言需重建整页才刷新为新语言。
  var langWired = false;
  function wireLangSwitch() {
    if (langWired) return;
    langWired = true;
    window.addEventListener('langchange', function () {
      if (container && document.getElementById('sec-models')) {
        buildShell();
        loadModels();
      }
    });
  }

  K.sections.models = {
    init: function () { buildShell(); wireLangSwitch(); },
    onShow: function () { if (!container) buildShell(); loadModels(); },
    onHide: function () {}
  };
})();

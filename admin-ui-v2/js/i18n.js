/* kiro2api Admin UI v2 — i18n.js
   data-i18n framework. The dictionary itself lives in i18n-dict.js, which
   MUST load before this file and define window.I18N as:
       window.I18N = { "some.key": { "zh-CN": "...", "zh-TW": "...",
                                     "en": "...", "ja": "...", "ko": "..." }, ... }

   Supported languages: zh-CN, zh-TW, en, ja, ko. Default zh-CN.

   Markup hooks:
     data-i18n="key"        -> textContent
     data-i18n-ph="key"     -> placeholder attribute
     data-i18n-title="key"  -> title attribute (also used for aria-label)

   Dynamic code calls K.i18n.t('key') (optionally with {vars} replaced by
   the caller, or via K.i18n.t('key', {n: 3}) which does the replace here). */
(function () {
  'use strict';

  var SUPPORTED = ['zh-CN', 'zh-TW', 'en', 'ja', 'ko'];
  var FALLBACK = 'en';
  var DEFAULT = 'zh-CN';
  var STORAGE_KEY = 'kiro2api_lang';

  function dict() { return window.I18N || {}; }

  function normalize(code) {
    if (window.K && typeof window.K._normalizeLang === 'function') return window.K._normalizeLang(code);
    return SUPPORTED.indexOf(code) >= 0 ? code : FALLBACK;
  }

  var I18n = {
    SUPPORTED: SUPPORTED,
    // Human-readable native names for the language switcher (never translated).
    LABELS: {
      'zh-CN': '简体中文',
      'zh-TW': '繁體中文',
      'en': 'English',
      'ja': '日本語',
      'ko': '한국어'
    },
    lang: (function () {
      var l = document.documentElement.getAttribute('data-lang');
      return (l && SUPPORTED.indexOf(l) >= 0) ? l : DEFAULT;
    })(),

    getLang: function () { return this.lang; },

    // Look up a key. `vars` optionally interpolates {token} placeholders.
    t: function (key, vars) {
      var entry = dict()[key];
      var str;
      if (entry) str = (entry[this.lang] != null ? entry[this.lang] : (entry[FALLBACK] != null ? entry[FALLBACK] : key));
      else str = key;
      if (vars) {
        Object.keys(vars).forEach(function (k) {
          str = str.split('{' + k + '}').join(String(vars[k]));
        });
      }
      return str;
    },

    apply: function (root) {
      var scope = root || document;
      var self = this;
      scope.querySelectorAll('[data-i18n]').forEach(function (el) {
        el.textContent = self.t(el.getAttribute('data-i18n'));
      });
      scope.querySelectorAll('[data-i18n-ph]').forEach(function (el) {
        el.setAttribute('placeholder', self.t(el.getAttribute('data-i18n-ph')));
      });
      scope.querySelectorAll('[data-i18n-title]').forEach(function (el) {
        var v = self.t(el.getAttribute('data-i18n-title'));
        el.setAttribute('title', v);
        if (!el.getAttribute('aria-label')) el.setAttribute('aria-label', v);
      });
    },

    setLang: function (code) {
      code = SUPPORTED.indexOf(code) >= 0 ? code : normalize(code);
      this.lang = code;
      document.documentElement.setAttribute('data-lang', code);
      try { localStorage.setItem(STORAGE_KEY, code); } catch (e) {}
      this.apply(document);
      window.dispatchEvent(new CustomEvent('langchange', { detail: { lang: code } }));
    }
  };

  window.K = window.K || {};
  window.K.i18n = I18n;

  // Section-agent contract aliases requested by the coordinating spec.
  window.i18n = I18n;
  window.applyI18n = function (root) { I18n.apply(root); };
})();

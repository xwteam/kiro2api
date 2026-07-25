/* kiro2api Admin UI v2 — boot.js
   Runs FIRST, synchronously in <head>, before <body> paints.
   Applies saved theme + language to <html> so there is no flash of the
   wrong theme / wrong language. Keep this file tiny and dependency-free. */
(function () {
  'use strict';

  function normalizeLang(l) {
    if (!l) return 'en';
    l = String(l);
    var low = l.toLowerCase();
    if (low === 'zh' || low === 'zh-hans' || low === 'zh-cn' || low === 'zh-sg' || low.indexOf('zh-hans') === 0) return 'zh-CN';
    if (low === 'zh-hant' || low === 'zh-tw' || low === 'zh-hk' || low === 'zh-mo' || low.indexOf('zh-hant') === 0) return 'zh-TW';
    if (low.indexOf('zh') === 0) return 'zh-CN';
    if (low.indexOf('ja') === 0) return 'ja';
    if (low.indexOf('ko') === 0) return 'ko';
    if (low.indexOf('en') === 0) return 'en';
    return 'en';
  }

  // ---- theme ----
  var theme;
  try { theme = localStorage.getItem('kiro2api_theme'); } catch (e) {}
  if (theme !== 'dark' && theme !== 'light') {
    theme = (window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light';
  }
  if (theme === 'dark') document.documentElement.setAttribute('data-theme', 'dark');

  // ---- language ----
  var lang;
  try { lang = localStorage.getItem('kiro2api_lang'); } catch (e) {}
  if (!lang) {
    // Default zh-CN when the browser can't tell us otherwise.
    lang = normalizeLang(navigator.language || (navigator.languages && navigator.languages[0]) || 'zh-CN');
  }
  document.documentElement.setAttribute('data-lang', lang);

  // Expose the normalizer so i18n.js can reuse it.
  window.K = window.K || {};
  window.K._normalizeLang = normalizeLang;
})();

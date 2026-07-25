/* kiro2api Admin UI v2 — theme.js
   Light/dark theme controller. Persists to localStorage 'kiro2api_theme'.
   Applies data-theme on <html>. boot.js in <head> applies the saved value
   before first paint; this module owns runtime toggling. */
(function () {
  'use strict';

  var STORAGE_KEY = 'kiro2api_theme';

  function current() {
    return document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';
  }

  function apply(theme) {
    if (theme === 'dark') document.documentElement.setAttribute('data-theme', 'dark');
    else document.documentElement.removeAttribute('data-theme');
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', theme === 'dark' ? '#0f172a' : '#059669');
  }

  var Theme = {
    get: current,
    set: function (theme) {
      theme = theme === 'dark' ? 'dark' : 'light';
      apply(theme);
      try { localStorage.setItem(STORAGE_KEY, theme); } catch (e) {}
      window.dispatchEvent(new CustomEvent('themechange', { detail: { theme: theme } }));
      return theme;
    },
    toggle: function () {
      return this.set(current() === 'dark' ? 'light' : 'dark');
    }
  };

  // Ensure the meta theme-color matches whatever boot.js applied.
  apply(current());

  window.K = window.K || {};
  window.K.theme = Theme;
})();

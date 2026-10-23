(function () {
  'use strict';
  var KEY = 'VKS_PNM_THEME';
  var DARK = 'dark';
  var LIGHT = 'light';
  function normalize(value) { return value === LIGHT ? LIGHT : DARK; }
  function current() {
    var value = null;
    try { value = localStorage.getItem(KEY); } catch (e) { /* private mode */ }
    return normalize(value);
  }
  function apply(theme) {
    var dark = normalize(theme) === DARK;
    var root = document.documentElement;
    if (!root) return;
    root.classList.toggle('theme-dark', dark);
    var btn = document.getElementById('vks-theme-toggle');
    if (btn) {
      var icon = btn.querySelector('.material-icons');
      if (icon) icon.textContent = dark ? 'light_mode' : 'dark_mode';
      btn.title = dark ? 'Switch to light theme' : 'Switch to dark theme';
    }
  }
  function safeWrite(value) {
    try { localStorage.setItem(KEY, value); } catch (e) { /* private mode */ }
  }
  apply(current());
  function ensureControl() {
    var wrap = document.querySelector('.main-large-header__actions-wrapper');
    if (!wrap || document.getElementById('vks-theme-toggle')) return;
    var button = document.createElement('button');
    button.id = 'vks-theme-toggle';
    button.type = 'button';
    button.setAttribute('data-qa', 'vks-theme-toggle');
    button.className = 'tc-button mdc-button mdc-button--text tc-button--icon';
    button.setAttribute('aria-label', 'Toggle color theme');
    var dark = current() === DARK;
    var icon = document.createElement('i');
    icon.className = 'material-icons tc-button__icon mdc-button__icon notranslate';
    icon.textContent = dark ? 'light_mode' : 'dark_mode';
    button.appendChild(icon);
    button.title = dark ? 'Switch to light theme' : 'Switch to dark theme';
    button.onclick = function () {
      var next = document.documentElement.classList.contains('theme-dark') ? LIGHT : DARK;
      safeWrite(next);
      apply(next);
    };
    var anchor = wrap.querySelector('.header-profile') || null;
    if (anchor) wrap.insertBefore(button, anchor);
    else wrap.appendChild(button);
  }
  var observer = null;
  if (typeof MutationObserver === 'function') {
    observer = new MutationObserver(function () { ensureControl(); });
    if (document.documentElement) observer.observe(document.documentElement, { childList: true, subtree: true });
  }
  ensureControl();
  document.addEventListener('DOMContentLoaded', ensureControl, { once: true });
  window.addEventListener('storage', function (event) {
    if (event.key === KEY) apply(event.newValue === LIGHT ? LIGHT : DARK);
  });
})();
try { localStorage.setItem('VKS_PNM_THEME', (localStorage.getItem('VKS_PNM_THEME') === 'light' ? 'light' : 'dark')); } catch (e) { /* noop */ }

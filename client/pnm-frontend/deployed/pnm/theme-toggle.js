(function () {
  'use strict';
  var KEY = 'VKS_PNM_THEME';
  var DARK = 'dark', LIGHT = 'light';
  var root = document.documentElement;

  function currentTheme() {
    try {
      var val = localStorage.getItem(KEY);
      if (val === LIGHT || val === DARK) return val;
    } catch (_) {}
    return (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) ? LIGHT : DARK;
  }

  function apply(theme) {
    var isDark = theme === DARK;
    root.classList.toggle('theme-dark', isDark);
    root.classList.toggle('theme-light', !isDark);
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', isDark ? '#1a202c' : '#0097a7');
    var btn = document.getElementById('vks-theme-toggle-btn');
    if (btn) {
      var label = isDark ? 'Переключить на светлую тему' : 'Переключить на тёмную тему';
      btn.title = label;
      btn.setAttribute('aria-pressed', isDark ? 'true' : 'false');
      btn.setAttribute('aria-label', label);
      var icon = btn.querySelector('.tc-button__icon');
      if (icon) icon.textContent = isDark ? 'light_mode' : 'dark_mode';
    }
  }

  apply(currentTheme());

  var container = document.createElement('div');
  container.id = 'vks-theme-toggle-container';
  container.className = 'tc-button-container tc-button-container--primary notification-center__button vks-theme-toggle';
  container.innerHTML = '<button type="button" id="vks-theme-toggle-btn" class="tc-button mdc-button mdc-button--text tc-button--icon" data-qa="vks-theme-toggle" aria-label="Тёмная тема"><span class="mdc-button__ripple"></span><div class="tc-button__content"><i class="material-icons tc-button__icon mdc-button__icon notranslate" aria-hidden="true">dark_mode</i></div></button>';

  function mount() {
    var wrap = document.querySelector('.main-large-header__actions-wrapper');
    if (!wrap) return;
    var spacer = wrap.querySelector('.main-large-header__actions-spacer');
    if (container.parentNode === wrap && container.nextElementSibling === spacer) return;
    if (spacer) wrap.insertBefore(container, spacer);
    else wrap.appendChild(container);
    apply(currentTheme());
  }

  document.addEventListener('click', function (e) {
    var t = e.target && e.target.closest && e.target.closest('#vks-theme-toggle-btn');
    if (t) {
      var next = root.classList.contains('theme-dark') ? LIGHT : DARK;
      try { localStorage.setItem(KEY, next); } catch (_) {}
      apply(next);
    }
  });

  if (typeof MutationObserver === 'function') {
    new MutationObserver(function () {
      if (!container.isConnected && document.querySelector('.main-large-header__actions-wrapper')) {
        mount();
      }
    }).observe(document.getElementById('root') || document.body, { childList: true, subtree: true });
  }

  mount();
  document.addEventListener('DOMContentLoaded', mount, { once: true });

  window.addEventListener('storage', function (e) {
    if (e.key === KEY) {
      apply(e.newValue === LIGHT || e.newValue === DARK ? e.newValue : currentTheme());
    }
  });
})();

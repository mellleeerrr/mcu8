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

  var container = document.createElement('div');
  container.id = 'vks-theme-toggle-container';
  container.className = 'tc-button-container tc-button-container--primary notification-center__button vks-theme-toggle';
  container.innerHTML = '<button type="button" id="vks-theme-toggle-btn" class="vks-theme-toggle-btn tc-button mdc-button mdc-button--text tc-button--icon" data-qa="vks-theme-toggle" role="button" tabindex="0"><span class="mdc-button__ripple"></span><div class="tc-button__content"><i class="material-icons tc-button__icon mdc-button__icon notranslate" aria-hidden="true">dark_mode</i></div></button>';

  var btn = container.querySelector('#vks-theme-toggle-btn');

  function apply(theme) {
    var isDark = theme === DARK;
    root.classList.toggle('theme-dark', isDark);
    root.classList.toggle('theme-light', !isDark);
    var meta = document.querySelector('meta[name="theme-color"]');
    if (meta) meta.setAttribute('content', isDark ? '#0E1621' : '#0088CC');

    var label = isDark ? 'Переключить на светлую тему' : 'Переключить на тёмную тему';
    if (btn) {
      btn.title = label;
      btn.setAttribute('aria-pressed', isDark ? 'true' : 'false');
      btn.setAttribute('aria-label', label);
      var icon = btn.querySelector('.tc-button__icon, .material-icons');
      if (icon) icon.textContent = isDark ? 'light_mode' : 'dark_mode';
    }
  }

  apply(currentTheme());

  function getTarget() {
    return document.querySelector('.main-large-header__actions-wrapper') ||
           document.querySelector('.main-compact-header__actions-wrapper') ||
           document.querySelector('.main-compact-header') ||
           document.querySelector('.auth-form__header');
  }

  function mount() {
    var target = getTarget();
    if (!target) return;
    var spacer = target.querySelector('.main-large-header__actions-spacer');
    if (container.parentNode === target && (!spacer || container.nextElementSibling === spacer)) return;
    if (spacer) target.insertBefore(container, spacer);
    else target.appendChild(container);
    apply(currentTheme());
  }

  btn.onclick = function () {
    var next = root.classList.contains('theme-dark') ? LIGHT : DARK;
    try { localStorage.setItem(KEY, next); } catch (_) {}
    apply(next);
  };

  var rAFPending = false;
  function scheduledMount() {
    if (rAFPending) return;
    rAFPending = true;
    window.requestAnimationFrame(function () {
      rAFPending = false;
      var target = getTarget();
      if (!container.isConnected || (target && container.parentNode !== target)) {
        mount();
      }
    });
  }

  if (typeof MutationObserver === 'function') {
    new MutationObserver(scheduledMount).observe(root, { childList: true, subtree: true });
  }

  mount();
  document.addEventListener('DOMContentLoaded', mount);
  window.addEventListener('load', mount);
  window.addEventListener('pageshow', function (e) {
    if (e.persisted) apply(currentTheme());
    mount();
  });

  window.addEventListener('storage', function (e) {
    if (e.key === KEY) {
      apply(e.newValue === LIGHT || e.newValue === DARK ? e.newValue : currentTheme());
    }
  });

  if (window.matchMedia) {
    var media = window.matchMedia('(prefers-color-scheme: dark)');
    var listener = function (e) {
      try { if (!localStorage.getItem(KEY)) apply(e.matches ? DARK : LIGHT); } catch (_) {}
    };
    if (media.addEventListener) media.addEventListener('change', listener);
    else if (media.addListener) media.addListener(listener);
  }
})();

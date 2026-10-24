(function () {
  'use strict';
  var KEY = 'VKS_PNM_THEME';
  var DARK = 'dark';
  var LIGHT = 'light';
  var root = document.documentElement;

  function currentTheme() {
    try {
      var val = localStorage.getItem(KEY);
      if (val === LIGHT || val === DARK) return val;
    } catch (_) {}
    if (window.matchMedia && window.matchMedia('(prefers-color-scheme: light)').matches) {
      return LIGHT;
    }
    return DARK;
  }

  function safeWrite(value) {
    try { localStorage.setItem(KEY, value); } catch (_) {}
  }

  var btnContainer = document.createElement('div');
  btnContainer.id = 'vks-theme-toggle-container';
  btnContainer.className = 'tc-button-container tc-button-container--primary notification-center__button vks-theme-toggle';

  var btn = document.createElement('button');
  btn.type = 'button';
  btn.id = 'vks-theme-toggle-btn';
  btn.className = 'tc-button mdc-button mdc-button--text tc-button--icon';
  btn.setAttribute('data-qa', 'vks-theme-toggle');
  btn.setAttribute('aria-label', 'Тёмная тема');

  var ripple = document.createElement('span');
  ripple.className = 'mdc-button__ripple';

  var content = document.createElement('div');
  content.className = 'tc-button__content';

  var icon = document.createElement('i');
  icon.className = 'material-icons tc-button__icon mdc-button__icon notranslate';
  icon.setAttribute('aria-hidden', 'true');

  content.appendChild(icon);
  btn.appendChild(ripple);
  btn.appendChild(content);
  btnContainer.appendChild(btn);

  function apply(theme) {
    var isDark = theme === DARK;
    if (root) {
      root.classList.toggle('theme-dark', isDark);
    }
    var label = isDark ? 'Переключить на светлую тему' : 'Переключить на тёмную тему';
    var iconName = isDark ? 'light_mode' : 'dark_mode';

    btnContainer.title = label;
    btn.title = label;
    btn.setAttribute('aria-pressed', isDark ? 'true' : 'false');
    icon.textContent = iconName;
  }

  apply(currentTheme());

  function mount() {
    var wrap = document.querySelector('.main-large-header__actions-wrapper');
    if (!wrap) return;
    var spacer = wrap.querySelector('.main-large-header__actions-spacer');
    if (btnContainer.parentNode === wrap && btnContainer.nextElementSibling === spacer) {
      return;
    }
    if (spacer) {
      wrap.insertBefore(btnContainer, spacer);
    } else {
      var profile = wrap.querySelector('.header-profile');
      if (profile) wrap.insertBefore(btnContainer, profile);
      else wrap.appendChild(btnContainer);
    }
  }

  document.addEventListener('click', function (e) {
    var target = e.target && e.target.closest && e.target.closest('#vks-theme-toggle-btn, #vks-theme-toggle-container');
    if (target) {
      var next = root.classList.contains('theme-dark') ? LIGHT : DARK;
      safeWrite(next);
      apply(next);
    }
  });

  var rafId = null;
  function scheduleMount() {
    if (rafId) return;
    rafId = requestAnimationFrame(function () {
      rafId = null;
      mount();
    });
  }

  if (typeof MutationObserver === 'function') {
    var observer = new MutationObserver(function () {
      if (!btnContainer.isConnected || !btnContainer.parentNode) {
        var wrap = document.querySelector('.main-large-header__actions-wrapper');
        if (wrap) scheduleMount();
      }
    });
    var targetNode = document.getElementById('root') || document.body || root;
    if (targetNode) {
      observer.observe(targetNode, { childList: true, subtree: true });
    }
  }

  mount();
  document.addEventListener('DOMContentLoaded', mount, { once: true });

  window.addEventListener('storage', function (e) {
    if (e.key === KEY) {
      apply(e.newValue === LIGHT || e.newValue === DARK ? e.newValue : currentTheme());
    }
  });

  if (window.matchMedia) {
    var mql = window.matchMedia('(prefers-color-scheme: dark)');
    if (mql && mql.addEventListener) {
      mql.addEventListener('change', function (e) {
        try {
          if (!localStorage.getItem(KEY)) {
            apply(e.matches ? DARK : LIGHT);
          }
        } catch (_) {}
      });
    }
  }
})();

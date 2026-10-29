/*
 * prostor:project-layer — сохранение deep-link при входе.
 *
 * Вендор-SPA при холодном открытии /pnm/monitoring/sessions/<id> без токена
 * уводит на /pnm/authorization без return-URL, а после логина всегда
 * открывает /pnm/monitoring/sessions — конкретная сессия теряется. Скрипт
 * грузится ДО бандла и:
 *   1) запоминает целевой URL глубокой ссылки (sessionStorage, TTL 10 минут);
 *   2) на странице авторизации, как только появляется токен, возвращает на
 *      запомненный URL (poll — вендор меняет location после логина сам);
 *   3) на hard reload/unload сохраняет цель через pagehide — страховка
 *      разлогина с перезагрузкой. Ограничение: вендорский SPA-логаут с
 *      истёкшим токеном идёт через History API (pagehide не срабатывает),
 *      поэтому cold deep-link с протухшим токеном цель не сохраняет — это
 *      принятое ограничение T07 (см. review B P2-1), а не «покрытый 401».
 * Проектный слой, CSP-safe (внешний файл, без inline-скриптов).
 */
(function () {
  'use strict';

  var TOKEN_KEY = 'TrueConfMCU_AccessToken';
  var RETURN_KEY = 'vks_deeplink_return';
  var TTL_MS = 10 * 60 * 1000;
  var AUTH_PATH = '/pnm/authorization';
  var DEEP_RE = /^\/pnm\/monitoring\/sessions\/[^/?#]+/;

  function storage(kind) {
    try {
      return kind === 'session' ? window.sessionStorage : window.localStorage;
    } catch (_) {
      return null;
    }
  }

  function token() {
    var store = storage('local');
    try {
      return store ? store.getItem(TOKEN_KEY) || '' : '';
    } catch (_) {
      return '';
    }
  }

  function isAuthPage() {
    return location.pathname.replace(/\/+$/, '') === AUTH_PATH;
  }

  function isDeepLink() {
    return DEEP_RE.test(location.pathname);
  }

  function remember(target) {
    var store = storage('session');
    if (!store) return;
    try {
      store.setItem(RETURN_KEY, JSON.stringify({ url: target, at: Date.now() }));
    } catch (_) {}
  }

  function recall() {
    var store = storage('session');
    if (!store) return '';
    try {
      var raw = store.getItem(RETURN_KEY);
      if (!raw) return '';
      var saved = JSON.parse(raw);
      if (!saved || typeof saved.url !== 'string' || typeof saved.at !== 'number') return '';
      if (Date.now() - saved.at > TTL_MS) {
        store.removeItem(RETURN_KEY);
        return '';
      }
      var url = saved.url;
      if (!DEEP_RE.test(url) || url.indexOf('//') === 0) return '';
      return url;
    } catch (_) {
      return '';
    }
  }

  function forget() {
    var store = storage('session');
    try {
      if (store) store.removeItem(RETURN_KEY);
    } catch (_) {}
  }

  function currentTarget() {
    return location.pathname + location.search + location.hash;
  }

  // 1. Холодный deep-link без токена: запомнить цель и уйти на авторизацию,
  //    пока вендорский guard не потерял id. С токеном цель запоминается только
  //    на pagehide — это страховка 401-разлогина, а не постоянная метка (иначе
  //    после успешного возврата она оставалась бы в sessionStorage).
  if (isDeepLink()) {
    if (!token()) {
      remember(currentTarget());
      location.replace(AUTH_PATH);
      return;
    }
    window.addEventListener('pagehide', function () {
      remember(currentTarget());
    });
  }

  // 2. Страница авторизации: после появления токена вернуть deep-link.
  if (!isAuthPage()) return;
  var target = recall();
  if (!target) return;
  if (token()) {
    forget();
    location.replace(target);
    return;
  }
  var tries = 0;
  var poll = window.setInterval(function () {
    tries += 1;
    if (tries > 240) {
      window.clearInterval(poll);
      return;
    }
    if (token()) {
      window.clearInterval(poll);
      forget();
      location.replace(target);
    }
  }, 250);
})();

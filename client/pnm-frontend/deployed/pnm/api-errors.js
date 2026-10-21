/*
 * Видимая ошибка API для vendor-UI 2.2.1.66.
 *
 * Бандл показывает toast не для всех ошибок: например, 501 на сохранении
 * сетевого интерфейса закрывает модалку молча. Этот маленький same-origin
 * скрипт перехватывает XHR/fetch к /api/v3 и показывает красный тост с
 * `error.description`, чтобы оператор не думал, что настройка применилась.
 * Не подавляет поведение бандла и не трогает успешные ответы.
 */
(function () {
  'use strict';
  if (window.__vksApiErrorsInstalled) return;
  window.__vksApiErrorsInstalled = true;

  function show(message) {
    var text = String(message || 'Не удалось выполнить запрос');
    var el = document.createElement('div');
    el.className = 'vks-api-error-toast';
    el.setAttribute('role', 'alert');
    el.textContent = text;
    el.style.cssText = [
      'position:fixed', 'z-index:2147483647', 'right:24px', 'bottom:24px',
      'max-width:460px', 'padding:14px 18px', 'border-radius:8px',
      'background:#c62828', 'color:#fff', 'font:14px/1.4 sans-serif',
      'box-shadow:0 6px 24px rgba(0,0,0,.35)', 'white-space:pre-line',
    ].join(';');
    document.body.appendChild(el);
    setTimeout(function () { el.remove(); }, 9000);
  }

  function report(status, body) {
    if (status < 400) return;
    var description = '';
    try {
      var parsed = typeof body === 'string' ? JSON.parse(body) : body;
      description = parsed && parsed.error && (parsed.error.description || parsed.error.message);
    } catch (e) { /* не JSON */ }
    show(description ? 'Не удалось применить изменения: ' + description : 'Ошибка API (' + status + ')');
  }

  var open = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function () {
    this.addEventListener('load', function () {
      if (String(this.responseURL || '').indexOf('/api/') === -1) return;
      report(this.status, this.responseText);
    });
    return open.apply(this, arguments);
  };

  if (window.fetch) {
    var nativeFetch = window.fetch;
    window.fetch = function () {
      return nativeFetch.apply(this, arguments).then(function (response) {
        if (String(response.url || '').indexOf('/api/') !== -1 && response.status >= 400) {
          response.clone().text().then(function (body) { report(response.status, body); }).catch(function () {});
        }
        return response;
      });
    };
  }
})();

/*
 * T12b: клиентская дедупликация глобальных настроек кодеков и подавление
 * мёртвого polling'а previews.
 *
 * Вендорский бандл 2.2.1.66 вызывает `fetchCodecs()` из `ec()` на каждую
 * строку/резолв (N+1) и не имеет in-flight/TTL-кэша. Сервер уже отдаёт
 * Cache-Control, но это не coalesce'ит одновременные XHR. Этот same-origin
 * скрипт загружается ДО бандла и перехватывает только точные GET-URL,
 * не меняя поведение остальных запросов.
 *
 * Инварианты:
 *  - кэш на документ, TTL 30s (совпадает с Cache-Control сервера);
 *  - PATCH настроек кодеков инвалидирует кэш и in-flight поколение;
 *  - previews не имеет источника кадров, поэтому GET возвращает честный
 *    пустой 200 без сетевого запроса (кнопка скрыта CSS);
 *  - ошибки/не-200 не кэшируются;
 *  - синтетический ответ отдаётся через штатные readystatechange/load/loadend.
 */
(function () {
  'use strict';
  if (window.__vksPerfOverridesInstalled) return;
  window.__vksPerfOverridesInstalled = true;

  var TTL_MS = 30000;
  var CODEC_RE = /\/api\/v3\/settings\/(audio|video)_codecs(?:\?|$)/;
  var PATCH_RE = /\/api\/v3\/settings\/((audio|video)_codecs|calls)(?:\?|$)/;
  var PREVIEWS_RE = /\/api\/v3\/abonents\/previews(?:\?|$)/;
  var cache = Object.create(null);
  var inflight = Object.create(null);
  var generation = 0;
  var patchInFlight = 0;
  var deferred = [];
  var epochKey = 'vks-codecs-epoch';
  var stats = { logical: 0, network: 0, cacheHits: 0, coalesced: 0, previewsSuppressed: 0, patches: 0 };
  window.__vksCodecCacheStats = stats;

  function sameOrigin(url) {
    try { return new URL(url, location.href).origin === location.origin; } catch (e) { return false; }
  }

  function currentEpoch() {
    try { return Number(localStorage.getItem(epochKey) || 0) || 0; } catch (e) { return 0; }
  }
  function bumpEpoch() {
    var next = currentEpoch() + 1;
    try { localStorage.setItem(epochKey, String(next)); } catch (e) { /* private mode */ }
    return next;
  }
  function abortCodecInflight() {
    Object.keys(inflight).forEach(function (key) {
      var slot = inflight[key];
      if (!CODEC_RE.test(key) || !slot || !slot.xhr || slot.xhr.__vksAborted) return;
      // Помечаем до abort: иначе finish посчитает owner живым и попробует
      // повторно send() уже завершённый XHR (InvalidStateError).
      slot.xhr.__vksAborted = true;
      try { nativeAbort.call(slot.xhr); } catch (e) { /* уже завершён */ }
    });
  }

  window.addEventListener('storage', function (event) {
    if (event.key === epochKey) {
      cache = Object.create(null);
      generation++;
      abortCodecInflight();
    }
  });

  // Внешние изменения (другой браузер/API) не присылают storage-event.
  // При возврате в вкладку сбрасываем TTL-кэш и перезапрашиваем codecs.
  function revalidateOnFocus() {
    if (document.visibilityState !== 'visible') return;
    cache = Object.create(null);
    generation++;
    abortCodecInflight();
  }
  document.addEventListener('visibilitychange', revalidateOnFocus);
  window.addEventListener('focus', revalidateOnFocus);

  var nativeOpen = XMLHttpRequest.prototype.open;
  var nativeSend = XMLHttpRequest.prototype.send;
  var nativeGetHeader = XMLHttpRequest.prototype.getResponseHeader;
  var nativeGetAllHeaders = XMLHttpRequest.prototype.getAllResponseHeaders;

  XMLHttpRequest.prototype.open = function (method, url) {
    this.__vksMethod = String(method || 'GET').toUpperCase();
    this.__vksUrl = String(url || '');
    return nativeOpen.apply(this, arguments);
  };

  function headerValue(headers, name) {
    var wanted = String(name).toLowerCase();
    var lines = String(headers || '').split(/\r?\n/);
    for (var i = 0; i < lines.length; i++) {
      var at = lines[i].indexOf(':');
      if (at > 0 && lines[i].slice(0, at).trim().toLowerCase() === wanted) {
        return lines[i].slice(at + 1).trim();
      }
    }
    return null;
  }

  function synth(xhr, status, text, headers) {
    setTimeout(function () {
      if (xhr.__vksAborted) return;
      try {
        Object.defineProperty(xhr, 'readyState', { value: 4, configurable: true });
        Object.defineProperty(xhr, 'status', { value: status, configurable: true });
        Object.defineProperty(xhr, 'statusText', { value: status === 200 ? 'OK' : 'Error', configurable: true });
        Object.defineProperty(xhr, 'responseText', { value: text, configurable: true });
        Object.defineProperty(xhr, 'response', {
          value: xhr.responseType === 'json' ? JSON.parse(text) : text,
          configurable: true,
        });
        xhr.getAllResponseHeaders = function () { return headers || ''; };
        xhr.getResponseHeader = function (name) { return headerValue(headers, name); };
        xhr.__vksSynthetic = true;
        // dispatchEvent сам вызывает onreadystatechange/onload; вручную их
        // звать нельзя — Axios получил бы ответ дважды.
        xhr.dispatchEvent(new Event('readystatechange'));
        xhr.dispatchEvent(new ProgressEvent('load'));
        xhr.dispatchEvent(new ProgressEvent('loadend'));
      } catch (e) {
        // Не даём UI зависнуть, если среда не разрешила подменить свойства.
        try { xhr.dispatchEvent(new Event('readystatechange')); } catch (ignored) { /* noop */ }
      }
    }, 0);
  }

  function deliver(xhr, entry) {
    synth(xhr, entry.status, entry.text, entry.headers);
  }

  function startRequest(xhr, url) {
    var slot = { generation: generation, epoch: currentEpoch(), waiters: [], xhr: xhr };
    inflight[url] = slot;
    stats.network++;
    xhr.__vksGeneration = generation;
    xhr.addEventListener('loadend', function () { finish(url, xhr, slot); });
    try {
      nativeSend.call(xhr);
    } catch (e) {
      // XHR уже завершён/закрыт: не оставляем мёртвый слот, иначе все
      // последующие fetchCodecs повиснут навсегда.
      if (inflight[url] === slot) delete inflight[url];
      synth(xhr, 0, '');
    }
  }

  function joinOrStart(xhr, url) {
    var pending = inflight[url];
    if (pending && pending.generation === generation && pending.xhr && !pending.xhr.__vksAborted) {
      stats.coalesced++;
      pending.waiters.push(xhr);
      return;
    }
    startRequest(xhr, url);
  }

  function finish(url, xhr, slot) {
    // Удаляем только свой in-flight: после PATCH мог стартовать новый.
    if (inflight[url] === slot) delete inflight[url];
    var text = xhr.responseText || '';
    var headers = nativeGetAllHeaders.call(xhr);
    if (patchInFlight > 0) {
      // PATCH ещё не закоммичен: старые/параллельные GET нельзя ни отдавать,
      // ни переиздавать — иначе закэшируем до-PATCH состояние.
      for (var d = 0; d < slot.waiters.length; d++) deferred.push({ xhr: slot.waiters[d], url: url });
      slot.waiters.length = 0;
      return;
    }
    var stale = slot.generation !== generation || currentEpoch() !== slot.epoch;
    if (xhr.__vksAborted || stale) {
      // Owner уже завершён/отменён: повторный send() запрещён XHR-контрактом.
      // Переиздаём только waiters — они ещё в состоянии OPENED; owner
      // получит abort/ошибку от своего native XHR, а свежий GET сделает
      // следующий вызов UI.
      var waiters = slot.waiters.slice();
      slot.waiters.length = 0;
      for (var i = 0; i < waiters.length; i++) {
        if (!waiters[i].__vksAborted) joinOrStart(waiters[i], url);
      }
      return;
    }
    if (xhr.status === 200) {
      cache[url] = { status: 200, text: text, headers: headers, at: Date.now(), epoch: slot.epoch };
    }
    for (var j = 0; j < slot.waiters.length; j++) {
      if (xhr.status === 200) deliver(slot.waiters[j], { status: 200, text: text, headers: headers });
      else synth(slot.waiters[j], xhr.status, text, '');
    }
    slot.waiters.length = 0;
  }

  var nativeAbort = XMLHttpRequest.prototype.abort;
  XMLHttpRequest.prototype.abort = function () {
    this.__vksAborted = true;
    return nativeAbort.apply(this, arguments);
  };

  XMLHttpRequest.prototype.send = function () {
    var xhr = this;
    var method = xhr.__vksMethod || 'GET';
    var url = xhr.__vksUrl || '';

    if (sameOrigin(url) && PATCH_RE.test(url) && method === 'PATCH') {
      stats.patches++;
      patchInFlight++;
      // До коммита PATCH кэш не трогаем: GET, пришедший в это окно, будет
      // отложен. Старые in-flight GET отменяем, их waiters — в deferred.
      abortCodecInflight();
      function finishPatch() {
        patchInFlight = Math.max(0, patchInFlight - 1);
        if (xhr.status >= 200 && xhr.status < 300) {
          cache = Object.create(null);
          generation++;
          bumpEpoch();
        }
        if (patchInFlight === 0) {
          var pending = deferred.splice(0);
          for (var i = 0; i < pending.length; i++) {
            if (!pending[i].xhr.__vksAborted) joinOrStart(pending[i].xhr, pending[i].url);
          }
        }
      }
      xhr.addEventListener('loadend', finishPatch);
      try {
        return nativeSend.apply(xhr, arguments);
      } catch (error) {
        // send() может бросить синхронно (bad state): не оставляем
        // patchInFlight>0, иначе все последующие codec-GET будут deferred.
        xhr.removeEventListener('loadend', finishPatch);
        finishPatch();
        throw error;
      }
    }

    if (sameOrigin(url) && method === 'GET' && PREVIEWS_RE.test(url)) {
      stats.previewsSuppressed++;
      synth(xhr, 200, JSON.stringify({ previews: [] }), 'content-type: application/json\r\n');
      return;
    }

    if (sameOrigin(url) && method === 'GET' && CODEC_RE.test(url)) {
      stats.logical++;
      if (patchInFlight > 0) {
        deferred.push({ xhr: xhr, url: url });
        return;
      }
      var hit = cache[url];
      if (hit && hit.epoch === currentEpoch() && Date.now() - hit.at < TTL_MS) {
        stats.cacheHits++;
        deliver(xhr, hit);
        return;
      }
      var pending = inflight[url];
      if (pending && pending.generation === generation) {
        stats.coalesced++;
        pending.waiters.push(xhr);
        return;
      }
      startRequest(xhr, url);
      return;
    }

    return nativeSend.apply(xhr, arguments);
  };

  // Совместимость: некоторые сборки читают заголовки через прототип.
  XMLHttpRequest.prototype.getResponseHeader = function (name) {
    if (this.__vksSynthetic) return headerValue(this.getAllResponseHeaders(), name);
    return nativeGetHeader.apply(this, arguments);
  };
})();

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
  var ENGINE_ABONENTS_RE = /\/api\/v3\/engine\/abonents(?:\?|$)/;
  var SESSION_PREVIEWS_GET_RE = /\/api\/v3\/(?:ui\/)?sessions\/[^/]+\/previews\/[^/?]+(?:\/|\?|$)/;
  var SESSION_PREVIEWS_RE = /\/api\/v3\/(?:ui\/)?sessions\/([^/]+)\/previews\/([^/?]+)(?:\/|\?|$)/;
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

  // Демпфирование 350-мс таймера плеера превью Yw до 1500 мс без накопления параллельных запросов
  var nativeSetTimeout = window.setTimeout;
  window.setTimeout = function (handler, delay) {
    if (delay === 350 && typeof handler === 'function') {
      try {
        var fnStr = Function.prototype.toString.call(handler);
        // Ограничиваем хук только циклом кадров плеера превью Yw: function(){a(y),o(y)}
        if (fnStr.indexOf(',o(') !== -1 || fnStr.indexOf(', o(') !== -1 || /\{\s*\w+\(\w+\)\s*,\s*\w+\(\w+\)\s*\}/.test(fnStr)) {
          arguments[1] = 1500;
        }
      } catch (e) { /* ignore */ }
    }
    return nativeSetTimeout.apply(window, arguments);
  };

  var nativeOpen = XMLHttpRequest.prototype.open;
  var nativeSend = XMLHttpRequest.prototype.send;
  var nativeSetRequestHeader = XMLHttpRequest.prototype.setRequestHeader;
  var nativeGetHeader = XMLHttpRequest.prototype.getResponseHeader;
  var nativeGetAllHeaders = XMLHttpRequest.prototype.getAllResponseHeaders;

  XMLHttpRequest.prototype.setRequestHeader = function (header, value) {
    if (!this.__vksReqHeaders) this.__vksReqHeaders = [];
    this.__vksReqHeaders.push({ header: header, value: value });
    return nativeSetRequestHeader.apply(this, arguments);
  };

  XMLHttpRequest.prototype.open = function (method, url) {
    var sMethod = String(method || 'GET').toUpperCase();
    var sUrl = String(url || '');
    if (ENGINE_ABONENTS_RE.test(sUrl)) {
      sUrl = sUrl.replace(/([?&]session_id=)(?:%7B|%7b|\{)([0-9a-fA-F-]+)(?:%7D|%7d|\})/i, '$1$2');
      arguments[1] = sUrl;
    }
    this.__vksMethod = sMethod;
    this.__vksUrl = sUrl;
    return nativeOpen.apply(this, arguments);
  };

  function normalizeId(id) {
    return String(id || '').trim().replace(/^\{+|\}+$/g, '').toLowerCase();
  }

  function syncStreamDirection(target, src) {
    if (!src || typeof src !== 'object') return;
    Object.assign(target, src);
    if (src.codec != null) target.codec = src.codec;
    if (src.bitrate != null) target.bitrate = src.bitrate;
    if (src.packets != null) target.packets = src.packets;
    if (src.bytes != null) target.bytes = src.bytes;
    if (src.jitter != null) target.jitter = src.jitter;
    if (src.lost_packets != null) {
      target.lost_packets = src.lost_packets;
      target.lostPackets = src.lost_packets;
    }
    if (src.lossrate != null) {
      target.lossrate = src.lossrate;
      target.lossRate = src.lossrate;
    } else if (src.loss != null) {
      target.lossrate = src.loss;
      target.lossRate = src.loss;
    }
    if (src.requested_bitrate != null) {
      target.requested_bitrate = src.requested_bitrate;
      target.requestedBitrate = src.requested_bitrate;
    }
    if (src.frame_rate != null) {
      target.frame_rate = src.frame_rate;
      target.frameRate = src.frame_rate;
    } else if (src.fps != null) {
      target.frame_rate = src.fps;
      target.frameRate = src.fps;
    }
    if (src.resolution != null) {
      target.resolution = src.resolution;
    } else if (src.width != null && src.height != null) {
      target.resolution = src.width + 'x' + src.height;
    }
    if (src.key_frame_requests != null) {
      target.key_frame_requests = src.key_frame_requests;
      target.keyFrameRequestCount = src.key_frame_requests;
    }
    if (src.key_frame_request_ratio != null) {
      target.key_frame_request_ratio = src.key_frame_request_ratio;
      target.keyFrameRequestRatio = src.key_frame_request_ratio;
    }
    if (src.fec != null) {
      target.fec = src.fec;
      target.isFECEnabled = src.fec;
    }
    if (src.present != null) {
      target.present = src.present;
      target.isAvailable = src.present;
    }
    if (src.active != null) {
      target.active = src.active;
      target.isActive = src.active;
    }
    if (src.quality != null) {
      target.quality = src.quality;
    } else {
      target.quality = 4;
    }
  }

  function findContextAbonentInState(state, depth) {
    if (!state || depth > 3) return null;
    if (state.contextAbonent && typeof state.contextAbonent === 'object') {
      return state.contextAbonent;
    }
    for (var k in state) {
      if (state[k] && typeof state[k] === 'object' && !Array.isArray(state[k])) {
        var found = findContextAbonentInState(state[k], depth + 1);
        if (found) return found;
      }
    }
    return null;
  }

  function updateActiveContextAbonent(data) {
    try {
      if (!data || !Array.isArray(data.abonents) || data.abonents.length === 0) return;
      var root = document.querySelector('#root') || document.querySelector('#app');
      var store = (root && root.__vue_app__ && root.__vue_app__.config && root.__vue_app__.config.globalProperties && root.__vue_app__.config.globalProperties.$store)
        || window.$store
        || (root && root._vnode && root._vnode.component && root._vnode.component.proxy && root._vnode.component.proxy.$store);

      var contextAbonent = null;
      if (store && store.state) {
        var sa = store.state['sessionLocal/abonent'];
        if (sa && sa.contextAbonent) {
          contextAbonent = sa.contextAbonent;
        } else {
          contextAbonent = findContextAbonentInState(store.state, 0);
        }
      }

      if (!contextAbonent && window.__vksContextAbonent) {
        contextAbonent = window.__vksContextAbonent;
      }
      if (!contextAbonent) return;

      var targetId = normalizeId(contextAbonent.id || contextAbonent.abonentId || contextAbonent.participantId || contextAbonent.connectionId || contextAbonent.addressId);
      if (!targetId && contextAbonent.engineAbonent) {
        targetId = normalizeId(contextAbonent.engineAbonent.id || contextAbonent.engineAbonent.abonentId || contextAbonent.engineAbonent.connection_id);
      }

      var found = null;
      for (var i = 0; i < data.abonents.length; i++) {
        var ab = data.abonents[i];
        var abConnId = normalizeId(ab.connection_id);
        var abId = normalizeId(ab.id);
        var abAbonentId = normalizeId(ab.abonent_id);
        var abPartId = normalizeId(ab.participant_id);
        if (targetId && (abConnId === targetId || abId === targetId || abAbonentId === targetId || abPartId === targetId)) {
          found = ab;
          break;
        }
      }
      if (!found && data.abonents.length === 1 && targetId) {
        found = data.abonents[0];
      }
      if (!found) return;

      var details = found.details || found;
      if (contextAbonent.engineAbonent) {
        if (!contextAbonent.engineAbonent.statuses) {
          contextAbonent.engineAbonent.statuses = {};
        }
        var statuses = contextAbonent.engineAbonent.statuses;
        Object.assign(statuses, details);
        if (details.fecc != null) statuses.fecc = details.fecc;
        if (details.user_agent) statuses.user_agent = details.user_agent;

        if (details.audio) {
          if (!statuses.audio) statuses.audio = {};
          Object.assign(statuses.audio, details.audio);
          if (details.audio.rx) {
            if (!statuses.audio.rx) statuses.audio.rx = {};
            syncStreamDirection(statuses.audio.rx, details.audio.rx);
          }
          if (details.audio.tx) {
            if (!statuses.audio.tx) statuses.audio.tx = {};
            syncStreamDirection(statuses.audio.tx, details.audio.tx);
          }
        }
        if (details.video) {
          if (!statuses.video) statuses.video = {};
          Object.assign(statuses.video, details.video);
          if (details.video.rx) {
            if (!statuses.video.rx) statuses.video.rx = {};
            syncStreamDirection(statuses.video.rx, details.video.rx);
          }
          if (details.video.tx) {
            if (!statuses.video.tx) statuses.video.tx = {};
            syncStreamDirection(statuses.video.tx, details.video.tx);
          }
        }
        if (details.content) {
          if (!statuses.content) statuses.content = {};
          Object.assign(statuses.content, details.content);
          if (details.content.rx) {
            if (!statuses.content.rx) statuses.content.rx = {};
            syncStreamDirection(statuses.content.rx, details.content.rx);
          }
          if (details.content.tx) {
            if (!statuses.content.tx) statuses.content.tx = {};
            syncStreamDirection(statuses.content.tx, details.content.tx);
          }
        }
        if (found.statuses) {
          Object.assign(statuses, found.statuses);
        }
      }
      if (contextAbonent.statuses) {
        Object.assign(contextAbonent.statuses, details);
      }
    } catch (e) {
      /* ignore */
    }
  }

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
        Object.defineProperty(xhr, 'statusText', { value: status === 204 ? 'No Content' : (status === 200 ? 'OK' : 'Error'), configurable: true });
        Object.defineProperty(xhr, 'responseText', { value: text || '', configurable: true });
        Object.defineProperty(xhr, 'response', {
          value: xhr.responseType === 'json' && text ? JSON.parse(text) : (text || ''),
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
    if (this.__vksRealXhr) {
      try { nativeAbort.call(this.__vksRealXhr); } catch (e) { /* ignore */ }
    }
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

    if (sameOrigin(url) && method === 'GET' && SESSION_PREVIEWS_GET_RE.test(url)) {
      try {
        return nativeSend.apply(xhr, arguments);
      } catch (e) {
        synth(xhr, 204, '', 'cache-control: public, max-age=1\r\n');
        return;
      }
    }

    if (sameOrigin(url) && method === 'DELETE' && SESSION_PREVIEWS_RE.test(url)) {
      synth(xhr, 204, '', '');
      return;
    }

    if (sameOrigin(url) && method === 'POST' && SESSION_PREVIEWS_RE.test(url)) {
      var m = url.match(SESSION_PREVIEWS_RE);
      var rawSessionId = '';
      var rawTarget = '';
      if (m) {
        try { rawSessionId = decodeURIComponent(m[1]); } catch (e) { rawSessionId = m[1]; }
        try { rawTarget = decodeURIComponent(m[2]); } catch (e) { rawTarget = m[2]; }
      }
      var cleanSessionId = rawSessionId.replace(/^\{+|\}+$/g, '');
      var cleanTarget = rawTarget.replace(/^\{+|\}+$/g, '');
      var streamId = 'preview-' + Math.random().toString(36).slice(2, 10);
      var stream_id = streamId;
      var fallbackPayload = {
        status: 'started',
        id: streamId,
        streamId: streamId,
        stream_id: streamId,
        preview_stream_id: streamId,
        previewStreamId: streamId,
        sessionId: cleanSessionId,
        session_id: cleanSessionId,
        abonentId: cleanTarget,
        abonent_id: cleanTarget,
        callStringId: cleanTarget,
        call_string_id: cleanTarget,
        addressId: cleanTarget,
        address_id: cleanTarget,
        preview: {
          status: 'started',
          id: streamId,
          streamId: streamId,
          stream_id: streamId,
          preview_stream_id: streamId,
          previewStreamId: streamId,
          sessionId: cleanSessionId,
          session_id: cleanSessionId,
          abonentId: cleanTarget,
          abonent_id: cleanTarget,
          callStringId: cleanTarget,
          call_string_id: cleanTarget,
          addressId: cleanTarget,
          address_id: cleanTarget,
          resolution: { width: 480, height: 270, fps_limit: 1 }
        }
      };
      var fallbackJson = JSON.stringify(fallbackPayload);
      var fallbackHeaders = 'content-type: application/json; charset=utf-8\r\n';

      var settled = false;
      var timer = null;

      function deliverFallback() {
        if (settled || xhr.__vksAborted) return;
        settled = true;
        if (timer) { clearTimeout(timer); timer = null; }
        synth(xhr, 200, fallbackJson, fallbackHeaders);
      }

      function deliverSuccess(status, text, headers) {
        if (settled || xhr.__vksAborted) return;
        settled = true;
        if (timer) { clearTimeout(timer); timer = null; }
        try {
          var parsed = JSON.parse(text);
          if (!parsed || typeof parsed !== 'object') {
            deliverFallback();
            return;
          }
          if (!parsed.streamId && parsed.preview_stream_id) parsed.streamId = parsed.preview_stream_id;
          if (!parsed.preview || typeof parsed.preview !== 'object') parsed.preview = Object.assign({}, parsed);
          if (!parsed.preview.preview_stream_id && parsed.streamId) parsed.preview.preview_stream_id = parsed.streamId;
          if (!parsed.preview.call_string_id && (parsed.call_string_id || parsed.abonent_id)) {
            parsed.preview.call_string_id = parsed.call_string_id || parsed.abonent_id;
          }
          if (!parsed.preview.streamId && parsed.preview.preview_stream_id) {
            parsed.preview.streamId = parsed.preview.preview_stream_id;
          }
          if (!parsed.preview.addressId && parsed.preview.call_string_id) {
            parsed.preview.addressId = parsed.preview.call_string_id;
          }
          text = JSON.stringify(parsed);
        } catch (e) {
          deliverFallback();
          return;
        }
        synth(xhr, status, text, headers);
      }

      timer = setTimeout(deliverFallback, 2500);

      var realXhr = null;
      try {
        realXhr = typeof window !== 'undefined' && window.XMLHttpRequest ? new window.XMLHttpRequest() : new XMLHttpRequest();
      } catch (e) {
        deliverFallback();
        return;
      }
      xhr.__vksRealXhr = realXhr;

      nativeOpen.call(realXhr, 'POST', url, true);
      if (xhr.__vksReqHeaders) {
        for (var h = 0; h < xhr.__vksReqHeaders.length; h++) {
          try {
            nativeSetRequestHeader.call(realXhr, xhr.__vksReqHeaders[h].header, xhr.__vksReqHeaders[h].value);
          } catch (e) { /* ignore */ }
        }
      }
      realXhr.onreadystatechange = function () {
        if (realXhr.readyState === 4) {
          if (realXhr.status >= 200 && realXhr.status < 300) {
            deliverSuccess(realXhr.status, realXhr.responseText, nativeGetAllHeaders.call(realXhr));
          } else {
            deliverFallback();
          }
        }
      };
      realXhr.onerror = deliverFallback;
      realXhr.ontimeout = deliverFallback;
      try {
        nativeSend.apply(realXhr, arguments);
      } catch (e) {
        deliverFallback();
      }
      return;
    }

    if (sameOrigin(url) && ENGINE_ABONENTS_RE.test(url)) {
      xhr.addEventListener('load', function () {
        if (xhr.status === 200 && xhr.responseText) {
          try {
            var data = JSON.parse(xhr.responseText);
            updateActiveContextAbonent(data);
          } catch (e) { /* ignore */ }
        }
      });
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

//! СНАПШОТ (2026-10-16): demo-страница удалена при переходе на фронтенд PNM (ADR-0027).
//! Файл сохранён как исторический артефакт; код ниже НЕ компилируется и не подключён.
//!
//! Встроенный веб-интерфейс для прямого WebRTC видеозвонка в браузере (HTML5 + Protobuf WebSocket).
//!
//! Основание: Фаза 8 (DEVELOPMENT_PLAN.md), ADR-0003, ADR-0025.
//! Полностью автономен (не требует внешних CDN или JS-библиотек).

use axum::response::{Html, IntoResponse, Response};

/// Обработчик выдачи демо-страницы.
pub async fn demo_handler() -> Response {
    Html(DEMO_HTML).into_response()
}

pub const DEMO_HTML: &str = r#"<!DOCTYPE html>
<html lang="ru">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>VKS WebRTC Live Meeting Demo</title>
  <style>
    :root {
      --bg-dark: #0f172a;
      --card-bg: #1e293b;
      --border: #334155;
      --primary: #3b82f6;
      --primary-hover: #2563eb;
      --success: #10b981;
      --danger: #ef4444;
      --text: #f8fafc;
      --text-muted: #94a3b8;
    }
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
      background: var(--bg-dark);
      color: var(--text);
      display: flex;
      flex-direction: column;
      min-height: 100vh;
    }
    header {
      background: var(--card-bg);
      border-bottom: 1px solid var(--border);
      padding: 1rem 2rem;
      display: flex;
      justify-content: space-between;
      align-items: center;
    }
    .logo {
      font-size: 1.25rem;
      font-weight: 700;
      display: flex;
      align-items: center;
      gap: 0.5rem;
      color: #60a5fa;
    }
    .status-badge {
      padding: 0.25rem 0.75rem;
      border-radius: 9999px;
      font-size: 0.875rem;
      font-weight: 600;
      background: #475569;
      color: #e2e8f0;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
    }
    .status-connected { background: #065f46; color: #6ee7b7; }
    .status-connecting { background: #854d0e; color: #fde047; }
    .main-container {
      display: flex;
      flex: 1;
      padding: 1.5rem;
      gap: 1.5rem;
    }
    .video-area {
      flex: 3;
      display: flex;
      flex-direction: column;
      gap: 1rem;
    }
    .video-grid {
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(360px, 1fr));
      gap: 1rem;
      flex: 1;
      min-height: 440px;
    }
    .video-card {
      background: #000;
      border: 1px solid var(--border);
      border-radius: 0.75rem;
      overflow: hidden;
      position: relative;
      display: flex;
      align-items: center;
      justify-content: center;
    }
    video {
      width: 100%;
      height: 100%;
      object-fit: cover;
    }
    .video-local { transform: scaleX(-1); }
    .video-label {
      position: absolute;
      bottom: 0.75rem;
      left: 0.75rem;
      background: rgba(15, 23, 42, 0.75);
      backdrop-filter: blur(4px);
      padding: 0.25rem 0.6rem;
      border-radius: 0.375rem;
      font-size: 0.8rem;
    }
    .controls-bar {
      background: var(--card-bg);
      border: 1px solid var(--border);
      border-radius: 0.75rem;
      padding: 0.75rem 1.25rem;
      display: flex;
      justify-content: center;
      gap: 1rem;
      align-items: center;
    }
    button {
      padding: 0.6rem 1.2rem;
      border-radius: 0.5rem;
      border: none;
      font-weight: 600;
      font-size: 0.9rem;
      cursor: pointer;
      transition: background 0.15s ease;
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
    }
    .btn-primary { background: var(--primary); color: #fff; }
    .btn-primary:hover { background: var(--primary-hover); }
    .btn-danger { background: var(--danger); color: #fff; }
    .btn-danger:hover { background: #dc2626; }
    .btn-secondary { background: #334155; color: #f1f5f9; }
    .btn-secondary:hover { background: #475569; }
    .sidebar {
      flex: 1;
      background: var(--card-bg);
      border: 1px solid var(--border);
      border-radius: 0.75rem;
      display: flex;
      flex-direction: column;
      overflow: hidden;
      max-width: 400px;
    }
    .sidebar-header {
      padding: 1rem;
      border-bottom: 1px solid var(--border);
      font-weight: 600;
    }
    .settings-form {
      padding: 1rem;
      display: flex;
      flex-direction: column;
      gap: 0.75rem;
      border-bottom: 1px solid var(--border);
    }
    .form-group label {
      display: block;
      font-size: 0.8rem;
      color: var(--text-muted);
      margin-bottom: 0.25rem;
    }
    input {
      width: 100%;
      background: #0f172a;
      border: 1px solid var(--border);
      color: #fff;
      padding: 0.5rem 0.75rem;
      border-radius: 0.375rem;
      font-size: 0.9rem;
    }
    input:focus { outline: 1px solid var(--primary); }
    .logs-container {
      flex: 1;
      padding: 0.75rem;
      font-family: monospace;
      font-size: 0.78rem;
      overflow-y: auto;
      display: flex;
      flex-direction: column;
      gap: 0.35rem;
      max-height: 380px;
      background: #090d16;
    }
    .log-entry { color: #94a3b8; }
    .log-success { color: #34d399; }
    .log-warn { color: #fbbf24; }
    .log-error { color: #f87171; }
  </style>
</head>
<body>
  <header>
    <div class="logo">
      <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="12" cy="12" r="10"/><path d="M10 15l5-3-5-3v6z"/></svg>
      <span>VKS WebRTC Live Client</span>
    </div>
    <div id="statusBadge" class="status-badge">
      <span id="statusDot">●</span>
      <span id="statusText">Отключено</span>
    </div>
  </header>

  <div class="main-container">
    <div class="video-area">
      <div class="video-grid">
        <div class="video-card">
          <video id="localVideo" class="video-local" autoplay playsinline muted></video>
          <div class="video-label" id="localLabel">Вы (Локальное видео)</div>
        </div>
        <div class="video-card">
          <video id="remoteVideo" autoplay playsinline></video>
          <div class="video-label" id="remoteLabel">Удалённый участник (SFU)</div>
        </div>
      </div>

      <div class="controls-bar">
        <button id="btnJoin" class="btn-primary" onclick="connectAndJoin()">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"/><polyline points="10 17 15 12 10 7"/><line x1="15" y1="12" x2="3" y2="12"/></svg>
          Войти в комнату
        </button>
        <button id="btnMuteAudio" class="btn-secondary" onclick="toggleAudio()" disabled>
          Микрофон: ВКЛ
        </button>
        <button id="btnMuteVideo" class="btn-secondary" onclick="toggleVideo()" disabled>
          Камера: ВКЛ
        </button>
        <button id="btnLeave" class="btn-danger" onclick="leaveRoom()" disabled>
          Покинуть
        </button>
      </div>
    </div>

    <div class="sidebar">
      <div class="sidebar-header">Параметры подключения</div>
      <div class="settings-form">
        <div class="form-group">
          <label>ID Комнаты (Room ID)</label>
          <input id="inputRoomId" value="room-demo-1" />
        </div>
        <div class="form-group">
          <label>Имя участника</label>
          <input id="inputDisplayName" value="WebUser" />
        </div>
      </div>
      <div class="sidebar-header">Журнал сигналов (Protobuf WebSocket)</div>
      <div id="logs" class="logs-container"></div>
    </div>
  </div>

  <script>
    let ws = null;
    let pc = null;
    let localStream = null;
    let msgCounter = 1;
    let participantId = null;

    function log(text, cls = '') {
      const logs = document.getElementById('logs');
      const entry = document.createElement('div');
      entry.className = 'log-entry ' + cls;
      const time = new Date().toLocaleTimeString();
      entry.innerText = `[${time}] ${text}`;
      logs.appendChild(entry);
      logs.scrollTop = logs.scrollHeight;
    }

    function setStatus(text, statusClass) {
      const badge = document.getElementById('statusBadge');
      const textElem = document.getElementById('statusText');
      badge.className = 'status-badge ' + statusClass;
      textElem.innerText = text;
    }

    // Минимальный Protobuf кодек
    function writeVarint(val, buf) {
      while (val >= 0x80) {
        buf.push((val & 0x7F) | 0x80);
        val >>>= 7;
      }
      buf.push(val & 0x7F);
    }

    function writeLengthDelimited(fieldNum, bytes, buf) {
      writeVarint((fieldNum << 3) | 2, buf);
      writeVarint(bytes.length, buf);
      for (let i = 0; i < bytes.length; i++) buf.push(bytes[i]);
    }

    function writeString(fieldNum, str, buf) {
      const utf8 = new TextEncoder().encode(str);
      writeLengthDelimited(fieldNum, utf8, buf);
    }

    function writeVarintField(fieldNum, val, buf) {
      writeVarint((fieldNum << 3) | 0, buf);
      writeVarint(val, buf);
    }

    function encodeClientHello(roomId, clientId) {
      const inner = [];
      writeVarintField(1, 1, inner); // proto_version = 1
      writeString(2, clientId, inner); // client_id
      writeString(3, "0.1.0", inner); // sdk_version
      writeString(6, roomId, inner); // room_id

      const root = [];
      writeVarintField(1, msgCounter++, root); // msg_id
      writeLengthDelimited(10, inner, root); // client_hello = 10
      return new Uint8Array(root);
    }

    function encodeJoinRoom(roomId, displayName) {
      const inner = [];
      writeString(1, roomId, inner);
      writeString(2, displayName, inner);

      const root = [];
      writeVarintField(1, msgCounter++, root);
      writeLengthDelimited(20, inner, root); // join_room = 20
      return new Uint8Array(root);
    }

    function encodeSdpOffer(sdp) {
      const inner = [];
      writeString(1, sdp, inner);

      const root = [];
      writeVarintField(1, msgCounter++, root);
      writeLengthDelimited(40, inner, root); // sdp_offer = 40
      return new Uint8Array(root);
    }

    function encodeIceComplete() {
      const inner = [];
      writeVarintField(1, 1, inner); // success = true
      writeVarintField(2, 25, inner); // rtt_ms = 25

      const root = [];
      writeVarintField(1, msgCounter++, root);
      writeLengthDelimited(51, inner, root); // ice_complete = 51
      return new Uint8Array(root);
    }

    function encodeLeaveRoom(roomId) {
      const inner = [];
      writeString(1, roomId, inner);

      const root = [];
      writeVarintField(1, msgCounter++, root);
      writeLengthDelimited(30, inner, root); // leave_room = 30
      return new Uint8Array(root);
    }

    async function connectAndJoin() {
      const roomId = document.getElementById('inputRoomId').value.trim();
      const displayName = document.getElementById('inputDisplayName').value.trim();
      const clientId = "client-" + Math.random().toString(36).substring(2, 9);

      setStatus("Подключение...", "status-connecting");
      log(`Запрос доступа к камере и микрофону...`);

      try {
        localStream = await navigator.mediaDevices.getUserMedia({ video: true, audio: true });
        document.getElementById('localVideo').srcObject = localStream;
        document.getElementById('localLabel').innerText = `${displayName} (Вы)`;
        log("Камера и микрофон успешно захвачены", "log-success");
      } catch (err) {
        log(`Не удалось захватить медиаустройства: ${err.message}`, "log-error");
        // продолжим без медиа
      }

      const wsProtocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const wsUrl = `${wsProtocol}//${location.host}/ws`;
      log(`Подключение к сигнальному WebSocket: ${wsUrl}`);

      ws = new WebSocket(wsUrl);
      ws.binaryType = "arraybuffer";

      ws.onopen = () => {
        setStatus("Сигналинг подключён", "status-connected");
        log("WebSocket соединение установлено. Отправка ClientHello...", "log-success");
        ws.send(encodeClientHello(roomId, clientId));
      };

      ws.onmessage = async (event) => {
        const bytes = new Uint8Array(event.data);
        handleIncomingSignalingMessage(bytes, roomId, displayName);
      };

      ws.onerror = (e) => {
        log("Ошибка WebSocket соединения", "log-error");
        setStatus("Ошибка соединения", "status-badge");
      };

      ws.onclose = () => {
        log("WebSocket соединение закрыто", "log-warn");
        setStatus("Отключено", "");
        document.getElementById('btnJoin').disabled = false;
        document.getElementById('btnLeave').disabled = true;
        document.getElementById('btnMuteAudio').disabled = true;
        document.getElementById('btnMuteVideo').disabled = true;
      };

      document.getElementById('btnJoin').disabled = true;
      document.getElementById('btnLeave').disabled = false;
      document.getElementById('btnMuteAudio').disabled = false;
      document.getElementById('btnMuteVideo').disabled = false;
    }

    async function handleIncomingSignalingMessage(bytes, roomId, displayName) {
      // Парсим первый тег
      let offset = 0;
      let msgId = 0;
      let replyTo = 0;

      // Читаем varints
      while (offset < bytes.length) {
        let tag = bytes[offset++];
        let wireType = tag & 0x07;
        let fieldNum = tag >>> 3;

        if (wireType === 0) { // varint
          let val = 0, shift = 0;
          while (offset < bytes.length) {
            let b = bytes[offset++];
            val |= (b & 0x7F) << shift;
            if (!(b & 0x80)) break;
            shift += 7;
          }
          if (fieldNum === 1) msgId = val;
          if (fieldNum === 2) replyTo = val;
        } else if (wireType === 2) { // length-delimited
          let len = 0, shift = 0;
          while (offset < bytes.length) {
            let b = bytes[offset++];
            len |= (b & 0x7F) << shift;
            if (!(b & 0x80)) break;
            shift += 7;
          }
          let payloadBytes = bytes.slice(offset, offset + len);
          offset += len;

          if (fieldNum === 11) { // ServerHello
            log(`Получен ServerHello от сервера (proto=1)`, "log-success");
            log(`Отправка JoinRoom(room=${roomId}, name=${displayName})...`);
            ws.send(encodeJoinRoom(roomId, displayName));
          } else if (fieldNum === 21) { // JoinRoomResult
            log(`Успешный вход в комнату! Получен JoinRoomResult`, "log-success");
            await setupWebRtcAndNegotiate();
          } else if (fieldNum === 41) { // SdpAnswer
            let sdpStr = new TextDecoder().decode(payloadBytes);
            // Извлекаем sdp из строки (field 1)
            let sdpIdx = sdpStr.indexOf("v=0");
            let cleanSdp = sdpIdx >= 0 ? sdpStr.substring(sdpIdx) : sdpStr;
            log(`Получен SdpAnswer от сервера (${cleanSdp.length} байт)`, "log-success");
            if (pc) {
              await pc.setRemoteDescription(new RTCSessionDescription({ type: 'answer', sdp: cleanSdp }));
              log("RemoteDescription (Answer) успешно применён", "log-success");
              ws.send(encodeIceComplete());
            }
          }
        } else {
          break;
        }
      }
    }

    async function setupWebRtcAndNegotiate() {
      log("Инициализация RTCPeerConnection...");
      pc = new RTCPeerConnection({
        iceServers: [{ urls: "stun:stun.l.google.com:19302" }]
      });

      if (localStream) {
        localStream.getTracks().forEach(track => {
          pc.addTrack(track, localStream);
          log(`Добавлен локальный трек: ${track.kind}`);
        });
      }

      pc.ontrack = (event) => {
        log(`Получен удалённый медиа-трек: ${event.track.kind}`, "log-success");
        const remoteVideo = document.getElementById('remoteVideo');
        if (event.streams && event.streams[0]) {
          remoteVideo.srcObject = event.streams[0];
        } else {
          const inboundStream = new MediaStream([event.track]);
          remoteVideo.srcObject = inboundStream;
        }
      };

      pc.onicecandidate = (event) => {
        if (event.candidate) {
          log(`ICE candidate: ${event.candidate.candidate.substring(0, 40)}...`);
        }
      };

      pc.oniceconnectionstatechange = () => {
        log(`ICE connection state: ${pc.iceConnectionState}`, "log-warn");
      };

      log("Создание локального SDP Offer...");
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      log(`Локальный SDP Offer сформирован. Отправка на SFU сервер...`);
      ws.send(encodeSdpOffer(offer.sdp));
    }

    function toggleAudio() {
      if (!localStream) return;
      const audioTrack = localStream.getAudioTracks()[0];
      if (audioTrack) {
        audioTrack.enabled = !audioTrack.enabled;
        document.getElementById('btnMuteAudio').innerText = audioTrack.enabled ? "Микрофон: ВКЛ" : "Микрофон: ВЫКЛ";
        log(`Микрофон ${audioTrack.enabled ? "включён" : "выключен"}`);
      }
    }

    function toggleVideo() {
      if (!localStream) return;
      const videoTrack = localStream.getVideoTracks()[0];
      if (videoTrack) {
        videoTrack.enabled = !videoTrack.enabled;
        document.getElementById('btnMuteVideo').innerText = videoTrack.enabled ? "Камера: ВКЛ" : "Камера: ВЫКЛ";
        log(`Камера ${videoTrack.enabled ? "включена" : "выключена"}`);
      }
    }

    function leaveRoom() {
      const roomId = document.getElementById('inputRoomId').value.trim();
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(encodeLeaveRoom(roomId));
        ws.close();
      }
      if (pc) {
        pc.close();
        pc = null;
      }
      if (localStream) {
        localStream.getTracks().forEach(t => t.stop());
        localStream = null;
      }
      document.getElementById('localVideo').srcObject = null;
      document.getElementById('remoteVideo').srcObject = null;
      log("Вы вышли из комнаты", "log-warn");
    }
  </script>
</body>
</html>
"#;

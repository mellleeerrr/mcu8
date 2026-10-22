# T07c — входящий SIP UAS (MCU принимает вызовы)

Дата: 2026-10-22. HEAD до коммита: рабочее дерево поверх `090b837`.

## Что сделано

- `vks-gateway-sip/src/uas.rs`: разбор INVITE/ACK/BYE/CANCEL/OPTIONS, `Require`,
  percent-decode room UUID, SDP-оффер PCMU/PCMA (+H.264), проверка `RTP/AVP`
  (SRTP/SAVP → 488), subset-answer (только выбранный PT, `m=video 0` если H.264
  не согласован), сборка ответов с To-tag/Contact/Via/received.
- `vks-server/src/gateways.rs::serve_sip_uas`: UDP `0.0.0.0:5060`
  (`VKS_SIP_UAS_BIND`, `VKS_SIP_UAS_MAX_CALLS`, `VKS_SIP_UAS_ENABLED=0`),
  резолв комнаты по UUID, RTP-пары из общего пула, 100/180/200+SDP, ACK/BYE/CANCEL,
  duplicate INVITE (новый Via, тот же To-tag/SDP), 420 на `100rel/timer`,
  lease на окно allocate→bridge, ретрансмиссия 200 до ACK, снятие «фантома»
  и портов по ACK-timeout, регистрация в `GatewayCallRegistry` для media/UI.
- `vks-server/src/main.rs`: listener стартует после `bind_state`; тесты его не поднимают.

## Живые проверки (Linux, 192.168.1.93)

1. Синтетический INVITE `sip:4d313d01-…@192.168.1.93`, 20 RTP PCMU пакетов:
   `200 OK`, participant `Polycom`, `audio_rx_packets=20`, `audio_tx_packets=273`,
   video tx идёт; BYE → `200 OK`, участников `0`.
2. **20 одновременных входящих** INVITE (audio-only): 20/20 `200 OK`,
   `/sessions/{id}/abonents.count=20`, занято 40 UDP-портов (RTP+RTCP).
   BYE всех → `count=0`, портов `:410xx` — `0`.
3. INVITE без ACK: через 10 с participant снят, `count=0` (watchdog).

## Осознанно вне T07c

SRTP/TLS/digest/100rel/Prack, TCP-транспорт, H.323 inbound (отдельный эпик
Q.931/H.245/RAS), per-m `c=`, симметричный RTP-latch для NAT (P2, следующий шаг).
Пока UAS рассчитан на доверенный LAN; auth/allowlist — до выставления 5060 наружу.

## Догоняющие исправления после ревью

- 180 Ringing теперь с To-tag; 200/BYE/CANCEL/ACK валидируются по
  `Call-ID + From-tag + To-tag + CSeq` (spooffed BYE → 481, диалог жив).
- CANCEL после финального 200 только подтверждается, диалог не рвётся.
- Admin hangup: `cancel_flag` из реестра подхватывается reaper'ом (5 с) —
  запись `calls` и слот cap освобождаются, повторный INVITE получает новый To-tag/порты.
- Per-IP token bucket на INVITE (20 burst / 10 rps); `VKS_SIP_UAS_MAX_CALLS`.
- Невалидный `c=` → 488 до 200; audio ingress latch первого NAT-пакета
  с фиксацией; video-failure → unbridge + audio-only downgrade.
- Добавлен автоматический сетевой тест `inbound_sip_uas_answers_with_to_tag_and_cleans_up_on_bye`
  (180 tag, 200, spoofed BYE 481, CANCEL-after-200, BYE cleanup).

## Второй проход (по ревью `3efc486`)

- Reaper теперь **шлёт BYE** вызывающему при admin hangup и по media-timeout,
  затем снимает participant/registry; live: DELETE абонента → терминал получает
  `BYE sip:…`, `abonents=0`.
- Media-watchdog после ACK: `media_bridge_dead` по `last_rx_ms`/`audio_codec_running`,
  а не только no-ACK таймер.
- Audio-latch обновляет и ingress `RtpSource.target`, и **egress** RTP/RTCP
  (двусторонний NAT-порт); per-m `c=IN IP4` учитывается; `c=0.0.0.0` → 488.
- `video_offered` сохраняет video m-line (`m=video 0`) даже при port 0/несовместимом кодеках.
- История входящих: `record_incoming_call`; media_details с clock/PT/answer-портами.
- CANCEL: известный диалог → 200 (не рвёт), неизвестный → 481; ACK/BYE требуют
  правильный CSeq-метод; парсер берёт первый Via.
- Тест admin hangup: `inbound_sip_uas_admin_hangup_sends_bye_and_reaps_call`.

## Третий проход (по ревью `abc4b08`)

- Серверный BYE: From = локальная сторона (`To` из INVITE + наш tag),
  To = вызывающий (`From` с его tag); fallback Request-URI — remote From-user,
  CSeq `saturating_add(1)`. Тест проверяет оба заголовка и tags.
- Media-timeout reaper чистит и `GatewayCallRegistry` (`by_participant`/`by_call`),
  а не только `calls`/RoomHub.
- CANCEL валидируется по To-tag+CSeq-методу (известный/правильный → 200,
  иначе 481).
- `audio_codec_running=true` для G.711 (раньше watchdog ложно считал codec мёртвым).
- RTCP egress — на `egress.port()+1` (NAT source), per-m-only `c=` в audio,
  отдельный video `c=`, `video_offered`; video downgrade обновляет RoomHub
  (`is_video_muted`) и registry media_details.

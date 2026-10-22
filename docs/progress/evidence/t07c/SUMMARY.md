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

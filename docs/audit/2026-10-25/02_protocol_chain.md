# 02. Протокольная цепочка SIP/H.323/SDP/ASN.1 — матрица дыр, оценка 20/20 и план P0

> Агент: `proto-chain` (task-2). Дата: 2026-10-25. HEAD рабочего дерева — `master`, дерево чистое.
> Метод: только чтение кода/доков. **Ни одного `cargo build/test`, ни одного живого вызова, ни правок вне этого файла.**
> Все утверждения о нашем коде — с `file:line`. Всё, что не проверено исполнением, помечено **«не проверено»**.

---

## 0. Позиция и уверенность

**Позиция (защищаю):** главный блокер полноценного MCU — неполная протокольная цепочка, а не скорость медиа.
Без SRTP/TLS, полного SDP, H.245/H.239, BFCP↔H.239, PLI/FIR↔`videoFastUpdatePicture`,
DTMF RFC2833↔`UserInputIndication` и корректного ASN.1 PER мы не получим 20/20 ни по SIP, ни по H.323,
и любой выигрыш медиаядра будет недоказуем в реальном сценарии.

**Уверенность по частям (честно, после чтения кода):**

| Утверждение | Уверенность | Почему |
|---|---|---|
| Порядок работ: протокол → медиа (при фиксированном сценарии «терминалы + MCU») | **95 %** | дыры протокола — бинарные блокеры дозвона, дыры медиа — деградации качества |
| Нынешняя цепочка не даёт «полноценного MCU» (SRTP/TLS/H.239/BFCP/DTMF/H.235/RAS) | **98 %** | прямые `file:line`, см. §2 |
| Оценка «сколько дозвонится из 20» | **75 %** | класс-усреднение без списка моделей; см. §3 и вопрос Q1 |
| ASN.1 PER — **не** главный блокер (пересмотр моей исходной формулировки) | **90 %** | кодек сгенерирован из официальных ASN.1, строгий, с полным набором PDU; ломается не PER, а обработка PDU (§2.E) |
| PLI/FIR — самый дешёвый и самый недооценённый P0 | **90 %** | нет `a=rtcp-fb`, нет приёма RTCP; §2.A/D |
| Раунд 1 Lead (§8): мандат смешанной комнаты + live-калибровка H.264 | **принято** | сценарий и профили зафиксированы Lead, оценки §3 уточнены в §8.6 |

**Что я уступаю сразу (и это усиливает позицию, а не ослабляет):**
исходная формулировка «корректного ASN.1 PER парсинга нет» — **неверна**. См. §2.E и §7:
ASN.1 в проекте — самая зрелая часть протокольного слоя. Блокер — **семантика** H.245/H.239/H.235 поверх готового кодека,
а не PER-декодирование.

---

## 1. Что прочитано, что не проверено

Прочитано полностью/выборочно (с указанием того, что осталось непрочитанным):
`vks-gateway-sip/src/{uas,uac,sip_message,rtp_bridge,codec,call}.rs` — полностью;
`{opus,g722,g726,g728,g7231,h265,aac_latm}.rs` — выборочно (кодек-обвязка, к оси (b)/(d) не относится);
`vks-gateway-h323/src/{uas,call,codec,q931}.rs` — полностью, `ffi.rs` — выборочно (orphaned mock, см. FULL_AUDIT:143-144);
`python/{h323_listener,h323_audio,h323_sidecar}.py` — полностью по сигнальным путям, `h323_asn.py` — как сгенерированный артефакт (по `PROVENANCE.md`);
`vks-server/src/gateways.rs` — SIP UAS §2082-2905, H.323 UAS §2908-3500, dial/land §1000-1400;
`gateway_audio.rs` (RTP/DTMF/RTCP), `gateway_video.rs` (деп-пакетайзер, RTCP/PLI);
`vks-signaling/src/sdp.rs`, `vks-proto/proto/*.proto`, `docs/architecture/SIGNALING_PROTOCOL.md`,
`docs/audit/FULL_AUDIT_2026-10-16.md` (устарел: описывает состояние до T07b/T07d, помечено ниже);
эталон: Vinteo `02_SIP_H323_цепочка_вызова.md`, Cisco `14/16/19`, TrueConf `MASTER_ARCHITECTURE.md`;
живые доказательства `t07b`, `t07d`, `webrtc`.

**Не проверено исполнением:** поведение конкретных моделей терминалов; фактическая потеря RTCP к пиру
(порт+1 в H.323); влияние `received=local_ip` на реальные UAC; таймаут-поведение reaper'ов.

### 1.1 Цепочка вызова в коде — по шагам (только чтение; ни одного исполнения)

**SIP входящий (UAS).** `UdpSocket::bind(0.0.0.0:5060)` (`gateways.rs:2087`) → `UasRequest::parse` (`uas.rs:61-121`,
компактные заголовки `i/f/t/v/m/l` поддержаны `:95-106`) → token-bucket 20/10 на IP (`:2210-2232`) →
`Require`-фильтр (`:2236-2258`) → дедуп/ретрай по `sip_dialog_key` (`:2260-2297`) → cap 64 (`:2298-2313`) →
резолв комнаты по user-части RURI (`:2314-2366`) → `negotiate()` (`:2367-2382`; внутри `uas.rs:181-237`) →
валидация IP/порта (`:2383-2420`) → аллокация пар RTP/RTCP (`:2453-2478`) → `build_answer` (`uas.rs:240-287`) →
`100 Trying` (`:2490`), `180 Ringing` (`:2492-2504`) — оба **синхронно**, окна ринга нет →
guard «в комнате WebRTC» → **486** (`:2505-2520`) → `200 OK + SDP` (`:2521-2530`) → лендинг в комнату (`:2539-2551`) →
audio/video бриджи (`:2568-2668`) → далее `ACK` (`:2786-2794`), `BYE` (`:2795-2847`), `CANCEL` (`:2848-2881`),
`OPTIONS` (`:2882-2888`), прочее → `405` (`:2889-2903`); reaper — `:2122-2180`.
**Ключевая асимметрия:** `200 OK` отправляется **до** проверки успешности лендинга; при отказе — BYE (`:2539-2550`).

**SIP исходящий (UAC).** `parse_target_addr` (`uac.rs:174-187`; `sips:`/`tcp`/`tls` → ошибка) → `UdpSocket::bind(0.0.0.0:0)`
(`:195`) → `build_invite` (`sip_message.rs:95-119`) → ретрансмиссия каждые 500 мс до 1xx (`uac.rs:236-258`) →
`2xx`: ACK на Contact (`:259-267`), затем `negotiate_media_detailed` (`:268-286`) → отказ = ACK+BYE (нельзя 488
как оффереру) → `ActiveSipSession` (`:287-310`) → монитор (`:329-391`): ответ на BYE/OPTIONS, re-INVITE → 488,
прочее → 501. При drop до ответа — CANCEL + ожидание финального ответа 3 с (`:102-168`).

**H.323 входящий (UAS).** Родитель слушает TCP 1720 (`h323_listener.py:745,892`) → `read_setup` (`:769-799`);
берутся только `callIdentifier`/alias/`conferenceID` → fork-ребёнок → `CallProceeding`+`Connect` с отдельным
`h245Address` (`:635-649`) → accept H.245-TCP с проверкой IP (`:659-662`) → TCS (`:665`) + MSD (`:666`) →
TCS-ack → кандидаты аудио (`:482-488`) → OLC аудио/видео (forward-only, `:342-357`) → приём OLC пира (`:362-411`) →
`connected` при `incoming_audio && remote_audio` (`:548-567`) → Release/EndSession (`:315-322`, `:537-538`).

**H.323 исходящий (UAC-sidecar).** Q.931 SETUP с alias+Called-party (`h323_sidecar.py:286-313`) → ждём
`alerting/callProceeding/connect` (`:335-374`) → требуем **отдельный** `h245Address` (`:361-368`) →
TCS+MSD (`:369-373`) → `open_channels` после ack'ов (`:375-398`) → `connected` (`:599-624`) → Release (`:665-685`).

**Вывод по цепочкам:** все четыре пути — «прямые»: нет прокси/GK/редиректов, нет ретрансляционных таймеров RFC 3261
кроме INVITE, нет шифрования, нет feedback-канала. Цепочка **линейная и хрупкая**: любой «нестандартный» шаг пира
(fastStart, tunneling, reverse-OLC, 401, AVPF, `Require`) обрывает её в конкретной строке — и эти строки перечислены в §2.

### 1.2 Калибровка: что из `FULL_AUDIT_2026-10-16.md` уже устарело

| Утверждение аудита 2026-10-16 | Сейчас (2026-10-25) | Статус |
|---|---|---|
| «7 из 15 крейтов не линкуются, в процессе нет SIP/H.323» (`FULL_AUDIT:16,285`) | `vks-server` поднимает SIP UAS (`gateways.rs:2086`), H.323 UAS (`gateways.rs:3028+`), живые прогоны `t07b/t07d` | **Устарело** |
| «нет слушателей 5060/1720» (`FULL_AUDIT:237`) | 5060 UDP (`gateways.rs:2087`), 1720 TCP (`h323_listener.py:892`); 5061/TCP-5060 — по-прежнему нет | **Частично устарело** |
| `SipCall`/`negotiate` — in-memory (`FULL_AUDIT:139-141`) | `codec.rs`/`call.rs` действительно легаси-мок, реальный путь — `rtp_bridge.rs`/`uas.rs`/`gateways.rs` | **Актуально для мёртвого кода**, не для боевого пути |
| H.323 FFI watchdog/off-by-one (`FULL_AUDIT:143-144`) | `ffi.rs` по-прежнему orphaned-мок; реальный H.323 — python-сайдкар | **Актуально как долг**, не блокер |
| «SIP UAS без Reject/re-INVITE» (`FULL_AUDIT:141`) | Reject появился (486/488/420/503), re-INVITE — намеренно 501 (`gateways.rs:2267-2282`) | **Частично устарело**, дыра осталась (A5) |

---

## 2. Матрица дыр по 5 осям

Легенда приоритетов: **P0** — блокирует дозвон/медиа или делает невозможным заявленный сценарий;
**P1** — блокирует класс устройств или функцию (hold/content/DTMF), но не все вызовы;
**P2** — корректность/спек-соответствие без наблюдаемого отказа в типовом сценарии.
Ссылки эталона: `Vin:N` = `vinteo/08_отчёты_анализа/02_SIP_H323_цепочка_вызова.md:N`;
`TC:N` = `mcu trueconf/.../MASTER_ARCHITECTURE.md:N`; `CMS:14:N` = `cisco CMS/analysis/meta/14_SIP_CALL_BRIDGE.md:N`.

### A. SIP UAS/UAC (RFC 3261/3262/3264/3581/3711/4028/4733/5626)

| # | Дыра у нас | Ожидание (RFC/ITU) | Эталон | Патч → тест | P |
|---|---|---|---|---|---|
| A1 | Только plain RTP: `rtp_bridge.rs:1155` требует `transport == "RTP/AVP"`; входящий UAS — `uas.rs:188`. SRTP-оффер → `None` → **488** (`gateways.rs:2367-2382`). Комментарий-признание: `gateways.rs:2082-2085`, `uas.rs:5-7` | RFC 3711 (SRTP/SRTCP), RFC 4568 (SDES `a=crypto`), MKI/ROC §3.2.1 | `Vin:13,116,247` (res_srtp, TLS 5061, ICE 3478); `TC:682-683` (`SRTP DISABLED\|MANDATORY\|AUTO`) | `rtp_bridge`: SRTP-контекст (KDF RFC 3711 §4.3, ROC/MKI) + `a=crypto` в offer/answer; `gateways.rs`: шифрование/дешифрование в `RtpSource`/`write_rtp_header` → юнит-KAT на RFC 3711 A.1 + e2e `pjsua --srtp` | **P0** |
| A2 | Нет TLS вообще: `uac.rs:176-182` отвергает `sips:`, `transport=tcp`, `transport=tls` как `InvalidUri`; TCP-листенера 5060 нет (`gateways.rs:2087` — только `UdpSocket`); 5061 не слушается | RFC 5626/5630 (TLS), RFC 5922 (sips) | `Vin:116` (UDP/TCP 5060, TLS — res_crypto), `TC:683` (`TRANSPORT TCP\|TLS`) | `vks-gateway-sip`: TCP/TLS-транспорт (rustls уже в workspace), dial по `sips:`; UAS-TLS на 5061 → интеграционный тест TLS-handshake + INVITE | **P0** (для провайдеров/PBX), P1 для LAN |
| A3 | Нет аутентификации/регистрации: `uac.rs:312-323` — любой `401/407` возвращается как `SipDialError::Rejected`, повтора с `Authorization` нет; `REGISTER` отсутствует во всём workspace (grep: 0 совпадений) | RFC 3261 §22 (Digest), RFC 3665 (REGISTER) | `Vin:255` (gatekeeper-клиент у Asterisk); любой PBX-транк | `uac.rs`: разбор `WWW-Authenticate/Proxy-Authenticate` (MD5/SHA-256, qop=auth), CSeq+1 retry; `sip_message.rs`: `build_register`/`build_authorization` → тест: фейковый UAS шлёт 401 → проверяем повтор с корректным response | **P0** |
| A4 | `100rel` не поддержан: `Require: 100rel` → **420** (`gateways.rs:2236-2258`), PRACK нет, `Supported` в исходящих нет (`sip_message.rs:95-119`) | RFC 3262 (PRACK/RAck), RFC 3261 §8.2.2.3 | `Vin:116` (Asterisk SIP) | `uas.rs`: PRACK-транзакция + `RAck`; `gateways.rs`: 18x reliable-последовательность; `sip_message.rs`: `Supported:` → тест: INVITE с `Require: 100rel` → 183+`RSeq` → PRACK → 200 | **P1** |
| A5 | Session timer/UPDATE: `Require: timer` → **420**; re-INVITE → **501** (`gateways.rs:2267-2282`); UPDATE → **405** (`gateways.rs:2889-2894`); исходящий монитор на re-INVITE отвечает **488** (`uac.rs:369`) | RFC 4028 (Session-Expires/Min-SE), RFC 3311 (UPDATE) | `TC:679-683` (профиль протокола) | `uas.rs`/`gateways.rs`: `Session-Expires`/`Min-SE`, UPDATE-обработчик, re-INVITE-renegotiation; `uac.rs`: refresh по половине интервала → тест: re-INVITE со сменой PT обновляет бридж | **P1** |
| A6 | `received=` неверный: `uas.rs:309-312` подставляет **свой** `local_ip` вместо адреса-источника запроса; `rport` не отражается (`rport=<port>`) | RFC 3261 §8.2.6.2 (`received` = source IP), RFC 3581 §4 (`rport` echo) | `Vin:116` | `uas.rs::build_response` принимает `peer: SocketAddr`; `gateways.rs:2219+` пробрасывает `peer` → тест: Via c чужим IP/портом + rport → в ответе `received=<peer.ip>;rport=<peer.port>` | **P1** |
| A7 | Утечка слота при отсутствии ACK: reaper снимает вызов только при `ack==true` (`gateways.rs:2136-2141`), таймера H (32 с, RFC 3261 §13.3.1.4) нет → 200 OK без ACK навсегда занимает слот и пару портов | RFC 3261 §13.3.1.4 (Timer H), §17 | `Vin:116` (rtptimeout/сессии) | `gateways.rs`: `acked_at` + Timer H → BYE по истечении; реюз `retire`-пути → тест: 200 OK без ACK → через N секунд вызов снят, слот свободен | **P1** |
| A8 | `CANCEL` после 200 → **200**, а не 481 (`gateways.rs:2848-2881`, комментарий 2849-2851) | RFC 3261 §9.2 (нет INVITE-транзакции → 481) | — | `gateways.rs`: 481 при отсутствии незавершённой INVITE-транзакции → тест на канонический CANCEL-race | **P2** |
| A9 | Нет `Allow` в 200 OK на INVITE (есть только в OPTIONS, `gateways.rs:2885-2886`); нет `Supported`/`Server`; `Require` матчится только точными `"100rel"\|"timer"` (`gateways.rs:2238-2240`) — прочие option-tags игнорируются вместо 420 | RFC 3261 §8.2.2.3, §20.5 | — | `uas.rs::build_response`: `Allow`/`Supported` параметрами → юнит на 420 для `Require: replaces` | **P2** |
| A10 | Входящий UAS не различает retransmission INVITE и новый вызов при совпадении CSeq (`gateways.rs:2266-2297`): логика «кэш 200» корректна, но без сравнения branch | RFC 3261 §17.2.3 | — | `UasRequest` + branch в ключ транзакции → тест двух INVITE с одинаковым Call-ID/CSeq и разными branch | **P2** |

### B. SDP (RFC 4566/3264, RFC 6184/7798, RFC 3550/4585, RFC 8830)

| # | Дыра у нас | Ожидание | Эталон | Патч → тест | P |
|---|---|---|---|---|---|
| B1 | `a=rtcp-fb` отсутствует во всём gateway-слое (grep: 0 в `vks-gateway-sip`, `vks-server`; есть только в WebRTC-парсере `vks-signaling/src/sdp.rs:100-103`). Ни offer, ни answer не объявляют NACK/PLI/FIR/REMB | RFC 4585 §4 (`a=rtcp-fb`), RFC 5104 (FIR) | `CMS:14:16` (`sipHandlerCall_mediaControlDataPending` — RTCP-фидбек как часть SIP-моста) | `rtp_bridge.rs::build_offer_with_video` (:616-658) + `uas.rs::build_answer` (:240-287): `m=video ... RTP/AVPF` + `a=rtcp-fb:<pt> nack pli` + `ccm fir` → тест: парсинг answer с rtcp-fb → negotiate выбирает AVPF-ветку | **P0** |
| B2 | `RTP/AVPF` оффер/answer отвергается как «несовместимый»: `rtp_bridge.rs:1155`, `uas.rs:188` | RFC 4585 §3 (AVPF — надмножество AVP) | `CMS` SIP-плоскость = WebRTC/AVPF-совместимая | там же: `transport.starts_with("RTP/AVP")` + флаг feedback → тест: устройство, отвечающее только `RTP/AVPF`, дозванивается | **P0** |
| B3 | `a=rtcp` в answer входящего UAS отсутствует (`uas.rs:240-287`), т.е. RTCP-порт пира не сообщается; в offer — есть (`rtp_bridge.rs:646,653`) | RFC 3605 (`a=rtcp`) | `Vin:116` | `uas.rs::build_answer`: `a=rtcp:<port+1>` → тест: answer содержит rtcp для audio/video | **P1** |
| B4 | Направление: `usable_media` требует `media.duplex` (`rtp_bridge.rs:1151-1156`) → answer с `a=sendonly`/`a=recvonly` (hold, «только просмотр») считается непригодным → отказ вызова | RFC 3264 §6.1 (sendonly/recvonly — легальный ответ, §8 hold) | `TC:819` (`self_view` — отдельный параметр) | `negotiate_media_detailed`: раздельные tx/rx-направления на поток; `gateways.rs`: hold-режим (mute egress, не рвать диалог) → тест: answer с `a=sendonly` → соединение + наш TX остановлен | **P1** |
| B5 | Hold через re-INVITE `c=0.0.0.0`/порт 0 не обрабатывается (re-INVITE → 501, A5) | RFC 3264 §8.4 (hold) | `TC:679-683` | связано с A5 → тест: re-INVITE `a=inactive` → 200 + пауза медиа | **P1** |
| B6 | Число m-line не сохраняется при наличии третьей секции: answer всегда 1 audio + (0..1) video (`uas.rs:247-285`); оффер с `m=application` (BFCP) или второй video → ответ с меньшим числом m-line | RFC 3264 §6 (**число m-line в answer = числу в offer**) | `Vin:122,251` (BFCP — отдельный m-line) | `uas.rs`: парсер m-line-списка и генерация «отклонённых» секций (`m=... 0 ...`) для всех неиспользованных → тест: оффер audio+video+application → 3 m-line в ответе, третья с портом 0 | **P0** |
| B7 | BFCP не поддержан нигде: нет `m=application <port> TCP/BFCP *`, нет парсера `floor-control` (`grep -rn "bfcp" crates/` → только админ-строки, `vks-admin/src/api_v3/settings_admin.rs:40`) | RFC 8855/8856 (BFCP), RFC 8857 (media) | `Vin:122,251` (`activate_bfcp_server`, `bfcp_do_parse_and_reply_on_hello`, TCP 1024-65000); `TC:533` (`bfcp_port_min/max`) | `vks-gateway-sip`: BFCP-сервер (TCP, `FloorRequest/FloorRelease/FloorRequestStatus`), SDP `m=application`, привязка к комнате → тест: BFCP hello/floorRequest/floorGrant по TCP | **P1** |
| B8 | Профиль H.264: только Baseline (`uas.rs:659-693`: `packetization-mode==1`, `profile_idc==66`, level 3.1) — High/Main → видео отклоняется (`m=video 0`), т.е. устройства с High-profile-only остаются без видео | RFC 6184 §8.1 (profile-level-id), §8.2 (packetization-mode) | `TC:43-56` (`H264-HIGH`, `H264-SVC` в справочнике), `TC:212` (маппинг profile_idc) | `uas.rs::h264_payload` + `gateway_video.rs`: приём Main/High (декодер уже умеет — энкодер держим Baseline), согласование «rx High / tx Baseline» → тест: answer `profile-level-id=64001f` → видео выбрано, TX остаётся Baseline | **P0** |
| B9 | `extmap`, `msid`, `ptime` в answer, `b=AS/TIAS`: в answer нет ни `b=AS`, ни `b=TIAS`; в offer `b=AS` есть только для видео (`rtp_bridge.rs:651`), `b=TIAS` нет нигде | RFC 8866 §6.10 (`b=`), RFC 8830 (extmap), RFC 4566 (`a=ptime`) | `TC:129` (`bitrate` в профиле), `TC:798` (PT-таблица) | `uas.rs::build_answer`: `b=AS`/`b=TIAS` из реального профиля энкодера; `rtp_bridge`: extmap (toffset/abs-send-time) для совместимости с SFU-плечом → тест: answer содержит `b=AS` ≤ профиля | **P1** |
| B10 | IPv4-only: `uas.rs:711-722` ищет строго `c=IN IP4 `; `c=IN IP6` → `None` → 488 | RFC 8866 §5.7 (c=), RFC 6157 (IPv6 в SDP) | `Vin:247-248` (ICE/STUN) | `media_ip`: `IN IP4`/`IN IP6` + `IpAddr` → тест: IPv6-оффер не отвергается на парсинге (дальше — политика) | **P2** |
| B11 | `a=rtcp-mux`, ICE-атрибуты (`ice-ufrag`, `candidate`, `fingerprint`) в gateway-SDP нет — ожидаемо, но означает, что WebRTC-терминал (browser через SIP) не подключить | RFC 8839 (ICE), RFC 5763 (DTLS-SRTP) | `Vin:116,248` (libnice/ICE), `TC:534` (WebRTC через TCS) | вне SIP-плеча: см. A1/A2 + §4 P0-4 | **P1** |
| B12 | Кодеки-аудио: `uas.rs:454-551` — PCMU/PCMA/G722/G723/G726×4/G728/Opus/MP4A-LATM. **G.729 отсутствует** (есть только в in-memory `codec.rs`, не в реальном пути), G.722.1/G.719/AAC(plain)/iLBC/AMR — нет | RFC 3551 (статические PT), RFC 5459 (G.729) | `TC:44` (18 аудиокодеков), `TC:709`; `Vin:121` (g7221/opus/g722/ulaw) | либо (а) добавить G.729/AMR через транскодер (`VKS_TRANSCODER_ENABLED`), либо (б) честно документировать: `ensure_bridgeable_codecs` уже существует (`gateways.rs:1121`) → тест: G.729-only offer → 488 с явной причиной (это уже так, `t07b`-фикс) | **P1** |
| B13 | Нарушение симметрии PT для видео: `uas.rs:574-588` выбирает PT пира, но при H.265 требует `profile-id=1` и `sprop-max-don-diff ∈ {нет,0}` (`:694-708`) — иначе видео молча отключается | RFC 7798 §7.1 | `TC:43-56` (`H265` в справочнике), `Vin:121` | расширить политику `h265_don_compatible` или явный `m=video 0` + причина в лог → тест на `sprop-max-don-diff=1` | **P2** |

### C. H.323 (H.225.0/H.245/H.239/H.235/H.460/Q.931)

| # | Дыра у нас | Ожидание (ITU) | Эталон | Патч → тест | P |
|---|---|---|---|---|---|
| C1 | **Нет RAS/gatekeeper'а**: листенер сразу слушает 1720 (`h323_listener.py:745,892`), GRQ/RRQ/ARQ/ACF нет ни в UAS, ни в исходящем сайдкаре (`h323_sidecar.py:687-710` — прямой TCP на `target_ip:target_port`) | H.225.0 §7-8 (RAS), §8.1.2 (registration) | `Vin:150,255,280`; `TC:679-682` (`SET PROTOCOL H323 GATEKEEPER ON\|OFF\|DISCOVER`), `TC:917-928` (живой RAS-прогон) | `h323_listener.py`: RAS-клиент (GRQ→RRQ keepalive, ARQ перед call), опция `VKS_H323_GK=`; `h323_sidecar.py`: ARQ/LRQ → тест: pycrate-fixture GCF/RCF + ARQ/ACF | **P0** (для GK-парков), P1 для direct-IP |
| C2 | **FastStart игнорируется**: `read_setup` (`h323_listener.py:769-799`) читает только `callIdentifier/alias/conferenceID`; `fastStart`/`h245Tunneling`/`h245Control` не разбираются; ответные CallProceeding/Connect — без fastStart (`:635-649`) | H.225.0 §8.1.7 (fastStart — preferred), §8.1.10 | `TC:917-928` (H323Plus: SETUP→PROCEEDING→ALERTING→CONNECT) | `h323_listener.py`: разбор `fastStart` (OLC-список) → Connect с принятым forward/reverse OLC; при `fastStart`-only пире — сразу медиа → тест: fixture SETUP с fastStart (Polycom) → Connect с fastStart-ACL | **P0** |
| C3 | **H.245 tunneling не поддержан**: исходящие PDU всегда `"h245Tunneling": False` (`h323_listener.py:303`, `h323_sidecar.py:303`); входящий `h245Control` не извлекается; исходящий сайдкар требует отдельный `h245Address` в Connect и падает иначе (`h323_sidecar.py:361-368`) | H.225.0 §8.2.2 (tunneling) | `TC:939-941` (H.460.18 — туннель H.245 в H.225) | оба сайдкара: `h245Control` ↔ очередь H.245-PDU; fallback «туннель, если в Connect нет h245Address» → тест: пир без h245Address, H.245 в туннеле | **P0** |
| C4 | **`reverseLogicalChannelParameters` отвергается**: `h323_listener.py:375-377`, `h323_sidecar.py:418-422` → `unspecified` → вызов без аудио (у нас би-directional OLC — норма для части терминалов) | H.245 §7.1.1 (bidirectional OLC легален) | `TC:917-928` (TCS/MSD/OLC живой) | оба `on_open`: принять `reverseLogicalChannelParameters` и открыть обратный канал тем же OLC → тест: Polycom fixture OLC с reverse → ACK + `incoming_audio` | **P0** |
| C5 | **Нет H.239**: ни `h239ExtendedVideoCapability`, ни второго video-канала, ни token'а; `h323_asn.py` не содержит H.239-модуля (grep `h239` = 0), хотя `ExtendedVideoCapability`/`genericRequest` в кодеке **есть** (`h323_asn.py:20`, `:7` вхождений) | H.239 §6-8 (token, второй канал), H.245 genericMessage | `Vin:164,196-197` (`h239` в конфиге, `h323_open_h239`, `On239SessionStarted`); `TC:955` («презентация в H.323 = H.239»), `TC:819` (`enable_content`) | `h323_listener.py`: capability `h239ExtendedVideoCapability` + OLC №3 (video, role=presentation) + `genericRequest` tokenRequest/Release; `gateways.rs`: content-трек в `McuEngine` → тест: fixture TCS с H.239 → OLC-презентация → token grant/deny | **P1** |
| C6 | **Нет H.235**: кодек H.235 скомпилирован (`h323_asn.py:19-59`, `h235.asn` в `schemas/`), но ни одной код-ссылки (grep `H235` в `*.py` = 0); `CONNECT` без `h235Key`/`h235KeySync` | H.235.1 (SimpleMD5/CAT), H.235.6, H.245 encryption sync | `Vin:180,256`; `TC:943-946` (CAT + SimpleMD5, AES-128, DH в CONNECT) | `h323_listener.py`: `h235Key`-парсинг (сейчас просто игнорируется — см. `read_setup`) и явный отказ/предупреждение при mandatory-шифровании; полноценно — после C1 → тест: fixture Connect с H.235 → детерминированный отказ с кодом | **P1** |
| C7 | Набор аудиокодеков H.323 = 4: G.722/G.711U/G.711A/G.723.1 (`h323_audio.py:10-16`); G.728/G.729/G.722.1/G.719/AAC нет; G.728 в комментарии отложен (`:5-8`) | H.245 Table 4 (AudioCapability), H.245 §6.3.1 | `TC:44` (18 аудиокодеков), `TC:709` | расширить `AUDIO_CODECS` + `gateway_audio.rs` (G.728 уже есть в SIP-бридже `g728.rs`) → тест: TCS с G.728 → симметричный выбор | **P1** |
| C8 | Видео H.323 = только H.264 generic (`h323_listener.py:143-160`, OID `0.0.8.241.0.0.1`); H.261/H.263/H.263+ отсутствуют — старые терминалы получают audio-only | H.241 (H.264), H.263 baseline (H.245 Table 5) | `TC:43` (`H261 H263 H263+` в справочнике); `Vin:121` (h263 в allow) | `h241_video_parameters` + `video_capability`: H.263 baseline как fallback (энкодер x264 не даёт H.263 → нужен ffmpeg-транскодер) → тест: TCS с H.263-only → честный `video_unavailable` (уже есть, `h323_sidecar.py:402-405`) | **P2** |
| C9 | `mediaControlChannel` пира не сохраняется: `on_ack` берёт только `mediaChannel` (`h323_listener.py:435-437`), RTCP отправляется на `media_port+1` (`gateway_video.rs:671-672`) — а в H.323 RTCP-порт задаётся явно | H.245 §7.3.2 (`mediaControlChannel`) | `TC:1141-1142` (диапазоны RTP/H.245) | `on_ack`: сохранять `mediaControlChannel`, прокинуть в Rust (поле события) → `gateway_video.rs`/`gateway_audio.rs`: адрес RTCP из сигнализации → тест: пир с RTCP-портом ≠ RTP+1 → SR уходит на объявленный порт (**не проверено** live) | **P1** |
| C10 | Неизвестные H.245 request'ы игнорируются без ответа: `_on_control` (`h323_listener.py:467-538`) знает TCS/MSD/OLC/closeLC/roundTripDelay; остальное (в т.ч. `miscellaneousCommand`) — тишина; неизвестный `response` — игнор | H.245 §6.2 (request требует response/reject) | `Vin:197` (`OnH245_MiscellaneousCommand/Indication`, `OnH245_FlowControlCommand`) | `_on_control`: `else: response(…Reject)` + `miscellaneousCommand`/`flowControlCommand` обработка → тест: fixture `miscellaneousCommand` → ответ и (для videoFastUpdatePicture) форс-IDR | **P0** (см. D1) |
| C11 | Нет `Alerting`/`Progress`/`Facility`: сразу CallProceeding+Connect (`h323_listener.py:635-649`) | H.225.0 §7.3.1 (Alerting) | `Vin:170` (`h323_send_alerting/progress`) | опциональный Alerting при `VKS_H323_RINGBACK=1` → тест: 180-эквивалент виден пиру | **P2** |
| C12 | Жёсткий лимит 20 одновременных H.323-вызовов в коде: `gateways.rs:3075-3079` (`clamp(1,20)`, default 20) | — (наша ёмкость) | — | поднять clamp/вынести в конфиг → тест: 21-й вызов → явный отказ с причиной | **P1** (для теста 20/20) |
| C13 | Нет H.450 (transfer/hold), H.460.18/19 (NAT), H.224/T.38 | H.450.x, H.460.18/19, H.224 | `Vin:193,256`; `TC:943-946` (H.460.18/19/22) | вне P0; для NAT-парков — H.460.18 обязателен → тест: H.460.18-туннель | **P2** |
| C14 | Игнорируется `callIdentifier`-отсутствие → `ProtocolError` (`h323_listener.py:794-797`); строгость к `conferenceID` (`:795`) | H.225.0 §7.2.2 (callIdentifier обязателен — ок) | — | оставить как есть, но вернуть Q.931 `Release Complete` с причиной «badCallIdentifier» вместо молчаливого закрытия → тест: Setup без callIdentifier → ReleaseComplete cause | **P2** |

### D. Interworking (SIP↔H.323↔комната/WebRTC)

| # | Дыра у нас | Ожидание | Эталон | Патч → тест | P |
|---|---|---|---|---|---|
| D1 | PLI/FIR: только TX «вслепую» раз в 1 с (`gateway_video.rs:665-683`) без `a=rtcp-fb` (B1); **приёма RTCP нет вообще** (`grep "rtcp_socket.*recv"` → 0); H.323-аналог `videoFastUpdatePicture` не обрабатывается (C10) | RFC 4585 §6.3.1 (PLI), §4.3.1 (FIR), H.241 §E.2 (videoFastUpdatePicture), H.245 `miscellaneousCommand` | `Vin:197` (`OnH245_MiscellaneousCommand/Indication`) | `gateway_audio.rs`/`gateway_video.rs`: RTCP-RX-задача → PLI/FIR → `force_keyframe` у энкодера; `_on_control`: `miscellaneousCommand` → тот же `force_keyframe`; TX: PLI по факту потери IDR, а не по таймеру → тест: юнит «принят PLI → выставлен флаг IDR»; e2e: `ffmpeg -f rtp` с PLI | **P0** |
| D2 | DTMF из комнаты → SIP-терминал: работает только при `dtmf_payload_type`, а входящий UAS его **не выставляет** (`None`, `gateways.rs:2577`) и `telephone-event` не объявляет в answer (`uas.rs:240-287`) | RFC 4733 §2.5 (negotiation), §3.1 | `Vin:123` (`dtmfmode=rfc2833` → PT); `TC:782` (PT 101 telephone-event) | `uas.rs::build_answer`: `a=rtpmap:101 telephone-event/8000` + `a=fmtp:101 0-15`; `gateways.rs:2566-2577`: `dtmf_payload_type = negotiated` → тест: answer содержит 101, `POST /dtmf` шлёт 5 событий | **P0** |
| D3 | DTMF от терминала → комната: RX-путь отсутствует — `RtpSource::accept` отбрасывает всё, кроме аудио-PT (`gateway_audio.rs:559-573`), т.е. PT 101/119 дропается; в H.323 нет ни RFC2833, ни `UserInputIndication`, и `dynamicRTPPayloadType` для аудио явно запрещён (`h323_listener.py:389-391`) | RFC 4733 §2.1, H.245 `UserInputIndication` (§6.4) | `Vin:123,164,188-189` (`dtmfmode=rfc2833` и в H.323); `TC:782` | `gateway_audio.rs`: ветка «PT == dtmf → парсить event/duration»; `h323_listener.py`: `userInputIndication` + `userInputCapability` в TCS → тест: RTP-пакет 101 (event=5) → событие в `dtmf_rx` канала | **P0** |
| D4 | BFCP↔H.239 не сшиты (нет ни того, ни другого: B7/C5) → **презентация недоступна ни с SIP, ни с H.323** | RFC 8855 + H.239 dual-video | `Vin:251` BFCP / `Vin:196-197` H.239; `TC:955` | после B7 и C5 — общий `content`-трек в `McuEngine` и роль presenter → тест: SIP-презентация видна H.323-пиру | **P1** |
| D5 | Нет content/presentation в протоколе комнаты: `vks-proto/proto/{signaling,media}.proto` не содержат ни `bfcp`, ни `h239`, ни `presentation/content` (grep = 0; 406 строк всех .proto) — даже при B7/C5 нечего сказать клиенту | Архитектурное требование | `TC:819` (`enable_content`, `bfcp`) | `signaling.proto`: `PresentationState`, `content_track_id`; `vks-signaling/src/handler.rs` → тест: proto roundtrip + push состояния | **P1** |
| D6 | PLI↔`videoFastUpdatePicture` — частный случай D1, но отдельно: у нас **никогда** не запрашивается keyframe у H.323-пира, поэтому после потери IDR видео замирает до следующего штатного IDR | H.241 §E.2, H.245 `miscellaneousCommand` | `Vin:197` | `h323_listener.py`: `send_control(("command", ("miscellaneousCommand", {...videoFastUpdatePicture...})))` → тест: фикстура команды | **P0** |
| D7 | Разрешения/профили: SIP/H.323-плечо не связано с L3/L4-профилями (`profiles`, `call_strings`) — `gateways.rs:1121 ensure_bridgeable_codecs` только проверяет кодеки; `bitrate/resolution/bfcp/dtmf` из профиля не применяются к SDP/H.245 (кроме `VideoSdpProfile`) | `TC:129,137,819` (L3 → call_strings L4: `bitrate`, `codecs`, `resolution`, `bfcp`, `dtmf`, `enable_content`) | `TC:555-563` | маппер `call_strings`→`VideoSdpProfile`/`audio_codecs[]`/`bfcp`/`dtmf` → тест: профиль с `video_codecs=[H264]` → оффер без H.265 | **P1** |
| D8 | Кодек/PT-map между плечами жёстко однопоточный: 1 аудио-PT + 1 видео-PT на направление (`gateways.rs:2570-2607`), H.323 — один `audio_codec` на вызов (`h323_listener.py:282-289`); при смене кодека мид-колл (re-INVITE/OLC) бридж не перестраивается | RFC 3264 §8 (re-offer), H.245 §7.1 (`requestMode`/новый OLC) | — | перестройка бриджа на re-offer → тест: re-INVITE с G.722→PCMU | **P2** |

### E. ASN.1 PER (pycrate 0.8.1)

**База (сильная, признаю):** `h323_asn.py` (16 871 строк) — сгенерирован целиком Pycrate **0.8.1** из трёх немутированных ASN.1
(`PROVENANCE.md:7-20`), H.225.0 v8 / H.245 v17 / H.235, хеши входов зафиксированы (`schemas/SHA256SUMS`),
рантайм пинится хешем (`requirements.lock:1`). Декодирование строгое: полное PER-значение, при «хвостовых»
битах — `ProtocolError` (`h323_listener.py:81-87`); энкодинг — `set_val()`+`to_aper()` (`:75-78`).
Fallback'ов/эвристик в ASN.1-слое **нет**, и это правильно.

| # | Дыра/ограничение | Ожидание | Патч → тест | P |
|---|---|---|---|---|
| E1 | Обрабатывается 5 типов H.245 request (`h323_listener.py:467-538`): TCS, MSD, OLC, closeLC, roundTripDelay. Остальные валидно **декодируются** и теряются (нет `else`) | H.245 §6.2: на каждый request — response/reject | `_on_control`: `else → ("response", <Reject>)`, ветки `miscellaneousCommand`, `flowControlCommand`, `requestMode`, `sendTerminalCapabilitySet` → тест: 4 фикстуры | **P0** |
| E2 | `h245Control` (tunneled H.245 в `h323-uu-pdu`) не читается вовсе (`h323_listener.py:303` только пишет `False`; `read_setup:791-798` берёт лишь тело) | H.225.0 §8.2.2 | разбор `h323-uu-pdu.h245Control` → в общую очередь `on_control` → тест: фикстура CONNECT с h245Control | **P0** |
| E3 | H.235-модуль есть в кодеке (`h323_asn.py:19`), но не используется ни одним вызовом — «мёртвый груз» лицензии/размера | H.235.1 | либо реализовать (C6), либо задокументировать как «ASN.1-only»; проверить, что импорт не тянет зависимостей → тест: `python -c "import h323_asn"` (уже покрыт `test_h323_listener`) | **P2** |
| E4 | Валидация полей после декода частичная: доступ к `params["callIdentifier"]["guid"]`/`params["terminalType"]` идёт «на веру»; исключения конвертируются в `Malformed H.245 message` (`h323_listener.py:460-465`) | H.245 §6 (обязательные поля) | явные проверки длины guid/диапазонов `terminalType`/`statusDeterminationNumber` → тест: фикстуры с усечёнными/расширенными полями | **P2** |
| E5 | Нет фикстур на H.239/H.235/H.245-command'ы: регрессионные векторы покрывают только CONNECT/OLC/OLC-ACK (`PROVENANCE.md:29-33`, `test_h323_listener.py`/`test_h323_sidecar.py` 30 тестов) | — | собрать векторы при реализации C5/C6/D1 → тест: golden-PER по каждому новому PDU | **P1** |
| E6 | `h239.asn` отсутствует в `schemas/` (3 файла: h225/h235/h245); H.239 можно собрать на `ExtendedVideoCapability` + `genericRequest`, но OID/token-семантика отсутствует | H.239 §6-7 | добавить `h239.asn` в `schemas/` и перегенерировать (или явно использовать generic-путь) → тест: TCS с H.239-capability декодируется | **P1** |

---

## 3. Оценка: сколько из 20 SIP и 20 H.323 дозвонятся сейчас (по коду)

**Сначала — что уже доказано живьём (и почему это не «20/20 устройств»):**
`t07b` — **20/20 SIP** call string на **один** MCU TrueConf `.92` (audio+video, H.264 720p),
и **20/20 H.323** на тот же `.92` (audio-only, `video_rx=0`); `t07d` — **20/20 входящих H.323**
нашим же сайдкаром (audio+video). Т.е. существующее «20/20» — это 20 **одинаковых** вызовов,
а не 20 моделей. Для H.323 в `t07d` обе стороны — наш код (self-interop).

**Условия оценки (жёстко из кода):** `VKS_SIP_UAS_ENABLED=1`; комната **без** WebRTC-пира
(иначе 486 — `gateways.rs:2505-2520`); порты: пул 41000-41200 = **100 пар** (`rtp_bridge.rs:20-35`),
40 вызовов × 2 пары (audio+video) = 80 пар — влезает, запас 20; SIP cap 64 (`gateways.rs:2109`),
H.323 cap **20** (`gateways.rs:3075-3079`) — ровно на грани.

### 3.0 Ворота дозвона: где обрывается вызов (по коду, до медиа)

«Ворота» — точки, где вызов не доходит до медиа. Порядок — как в коде; числа — экспертная оценка по типовому
парку (SIP-телефоны + софтфоны + 2-3 аппаратных ВКС + 1-2 MCU-моста), **не измерение**.

| # | Ворота (SIP, входящий) | Код | Из 20 |
|---|---|---|---|
| G0 | Транспорт: UDP 5060 (TCP/TLS-only не достучится) | `gateways.rs:2087` | ≈18 |
| G1 | Валидный SIP-парс (compact-заголовки поддержаны) | `uas.rs:61-121` | ≈18 |
| G2 | Нет `Require: 100rel/timer` (иначе 420) | `gateways.rs:2236-2258` | ≈15 |
| G3 | `m=audio`, порт > 0, proto `RTP/AVP` (AVPF/SDES → 488) | `uas.rs:188-194` | ≈14 |
| G4 | Общий аудиокодек из набора `uas.rs:454-551` (G.729-only → 488) | `uas.rs:454-551` | ≈13 |
| G5 | IPv4 `c=IN IP4` с конкретным адресом | `uas.rs:711-722`; `gateways.rs:2383-2420` | ≈13 |
| G6 | В комнате нет WebRTC-пира (иначе 486) | `gateways.rs:2505-2520` | ≈13 |
| G7 | Лендинг в комнату не отказал (иначе 200+BYE) | `gateways.rs:2539-2551` | ≈13 |
| G8 | Видео: H.264 CBP/mode1 или H.265 (иначе `m=video 0`) | `uas.rs:659-708` | ≈8 |
| G9 | DTMF: `telephone-event` в answer (сейчас нет) | `uas.rs:240-287` | **0** |
| G10 | Hold/re-INVITE/session-timer (сейчас 501/420/405) | `gateways.rs:2267-2282,2889-2894` | **0** |
| G11 | SRTP/TLS-профиль (сейчас нет) | `rtp_bridge.rs:1155`; `uac.rs:176-182` | **0** |
| G12 | Контент/BFCP (нет m-line и нет протокола комнаты) | §2.B7, §2.D5 | **0** |

| # | Ворота (H.323, входящий) | Код | Из 20 |
|---|---|---|---|
| K0 | Пир звонит **прямо на IP:1720** (GK-маршрутизация → нет RAS) | `h323_listener.py:745,892`; §2.C1 | ≈14 |
| K1 | Setup содержит 16-байтный `callIdentifier` | `h323_listener.py:794-797` | ≈14 |
| K2 | Пир согласен на **отдельный** H.245-канал (fastStart-only/tunneling-only → нет медиа) | `h323_listener.py:635-649`; §2.C2/C3 | ≈11 |
| K3 | Нет OLC с `reverseLogicalChannelParameters` | `h323_listener.py:375-377`; `h323_sidecar.py:418-422` | ≈9 |
| K4 | Симметричный аудиокодек {G.722,G.711U,G.711A,G.723.1} | `h323_audio.py:10-16,42-75` | ≈8 |
| K5 | Видео: H.264 generic (H.263/H.261-only → audio-only) | `h323_listener.py:143-160`; §2.C8 | ≈7 |
| K6 | Нет обязательного H.235 | §2.C6 | ≈7 |
| K7 | DTMF / `UserInputIndication` (сейчас нет) | §2.D3 | **0** |
| K8 | H.239-контент (сейчас нет) | §2.C5 | **0** |

### 3.1 SIP (20 моделей, класс-усреднение)

| Класс терминала | Дозвон | Первое, что ломается (по коду) |
|---|---|---|
| Polycom RPD/Group, Cisco SX/88xx, Yealink, Grandstream, Huawei TE, Lifesize (UDP, PCMU/G722, H.264 CB) | ✅ audio; ✅/⚠ video | ничего на дозвоне; видео — B8 (High-profile-only ответ), дальше B1 (нет PLI) |
| Asterisk/FreeSWITCH-trunk, Vinteo, TrueConf (plain RTP) | ✅ audio+video | как выше (доказано `t07b`) |
| Avaya IX / сессионный таймер (`Require: timer`) | ❌ | **420** (`gateways.rs:2236-2258`) |
| `Require: 100rel`-профиль | ❌ | **420** (там же) |
| SRTP-обязательный / TLS-only / TCP-only | ❌ | нет листенера и нет SDP-шифрования (A1/A2); TCP — запрос вообще не получен |
| WebRTC-SIP GW (DTLS-SRTP, AVPF) | ❌ | 488 (B2) |
| SIP-провайдер/транк с digest-auth | ❌ | 401 → `Rejected` без повтора (A3); REGISTER отсутствует |
| Терминал за NAT (без ICE) | ⚠ one-way | `received=local_ip` (A6) + нет rport-echo |
| Любой терминал в комнате с WebRTC-пиром | ❌ | **486 Busy Here** (`gateways.rs:2505-2520`) |

**Итог SIP:** дозвон с аудио — **≈13-15 из 20** *[ОЦЕНКА; уточнение по 20 устройствам и live-калибровке — §8.4/§8.6: 17-18 из 20]*; видео — **≈8 из 20** *[ОЦЕНКА; §8.1/§8.4: 10 из 15 предлагающих видео]*; DTMF (в обе стороны) — **0 из 20**;
hold/session-timer — **0 из 20**; content/BFCP — **0 из 20**; SRTP/TLS — **0 из 20**.
«Что сломается первым» на типовом устройстве: ничего (дозвон пройдёт), затем — **видео-фриз без PLI (D1)**
и **отсутствие DTMF (D2/D3)**; на «корпоративном» устройстве — **420/488/нет ответа (A2/A3)**.

### 3.2 H.323 (20 моделей, класс-усреднение)

| Класс терминала | Дозвон | Первое, что ломается |
|---|---|---|
| Polycom HDX/Group, Cisco C/SX, Lifesize Icon, Huawei TE (direct IP) | ✅ audio; ⚠ video | если пир шлёт OLC с `reverseLogicalChannelParameters` → **reject** (C4); если fastStart-only/tunneling-only → **нет медиа** (C2/C3) |
| Vinteo H.323 GW, TrueConf H.323 GW | ✅ audio; ❌ content | `t07b`: video от TrueConf H.323 не идёт (audio-only) — вне нашего контроля; H.239 нет (C5) |
| Зарегистрирован на gatekeeper (обязательный GK-режим) | ❌ | нет RAS (C1) — вызов не придёт/не уйдёт |
| Tandberg/Polycom с G.729/G.728-only профилем | ❌ | кодек-набор из 4 (C7) → `No common symmetric H.323 audio codec` (`h323_listener.py:483-484`) |
| H.235-mandatory (шифрование) | ❌ | H.235 нет (C6); поведение при mandatory — **не проверено** |
| H.261/H.263-only (старые) | ✅ audio; ❌ video | только H.264 (C8) |
| За H.460.18/19 NAT | ❌ | нет H.460 (C13) |
| Наш собственный сайдкар (UAC) | ✅ audio+video | self-interop, доказано `t07d` |

**Итог H.323:** дозвон с аудио — **≈11-14 из 20**; видео — **≈7-9 из 20**; DTMF — **0**; H.239 — **0**;
H.235 — **0**; GK — **0**. Первое, что ломается на «живом» парке: **C4/C2/C3** (OLC/fastStart/tunneling)
раньше, чем кодеки.

### 3.3 Сводка и главный вывод оценки

- «20 SIP + 20 H.323 в одной комнате» сейчас достижимо **только** для gateway-only комнаты
  (без WebRTC) и только для «простых» UDP/plain-RTP/H.264-CB терминалов: **≈24-29 из 40** с аудио,
  **≈15-17 из 40** с видео, **0 из 40** с DTMF/content/SRTP.
- **Первым по коду ломается не медиа, а классы «secure/proxy/GK»**: 420/488/нет ответа/нет RAS.
- Даже там, где дозвон есть, **видео нестабильно не из-за скорости энкодера, а из-за отсутствия PLI/FIR
  и `a=rtcp-fb`** (B1/D1/D6): потеря IDR = замерший кадр до следующего штатного IDR.

---

## 4. План на 5 первых P0-патчей (порядок, зависимости, риски)

Порядок выбран по «сколько устройств разблокирует / стоимость» и по отсутствию зависимостей.
Оценки трудоёмкости — мои, не измеренные.

**P0-1. RTCP feedback: `a=rtcp-fb` + AVPF + приём PLI/FIR ↔ `videoFastUpdatePicture` (B1, B2, D1, D6, E1/E2-часть).**
Файлы: `rtp_bridge.rs` (offer/answer, парсер `rtcp-fb`), `uas.rs` (answer), `gateway_video.rs`+`gateway_audio.rs`
(RTCP-RX-задача, `force_keyframe`), `h323_listener.py` (`miscellaneousCommand`), `h323_sidecar.py`.
Зависимостей нет. Риск регрессии: AVPF-переговоры в тестах Polycom-fixture (нужно обновить ожидания);
не слать RTCP PSFB, если `a=rtcp-fb` не согласован. Тесты: юнит-парсер + «PLI → IDR», e2e с `ffmpeg`-пиром.

**P0-2. DTMF full-duplex: RFC 4733 в SIP (объявление, TX, RX) + H.245 `UserInputIndication` (D2, D3).**
Файлы: `uas.rs::build_answer`, `gateways.rs` (проброс `dtmf_payload_type`), `gateway_audio.rs`
(RX-ветка по PT, событийный стейт), `h323_listener.py`/`h323_sidecar.py` (capability + indication).
Зависимость: P0-1 не нужен; пересечение с P0-1 только в RTCP-сокете. Риск: `RtpSource` пиннит один PT —
нужен явный список «служебных» PT (telephone-event) рядом с аудио. Тесты: юнит-парсер события + e2e клавиша.

**P0-3. H.323 OLC/fastStart/tunneling (C2, C3, C4) — разблокирует «живой» H.323-парк.**
Файлы: `h323_listener.py`, `h323_sidecar.py` (`on_open` reverse-ветка, `read_setup` fastStart, `h245Control`).
Зависимость: нет (ASN.1-база готова — `ExtendedVideoCapability`/`genericRequest` уже в кодеке).
Риск: принятие reverse-OLC меняет модель «один канал на направление» в Rust-бридже (`gateways.rs`:
audio/video пары портов уже аллоцируются заранее) — нужно аккуратно смапить номер OLC→порт.
Тесты: 3 фикстуры (fastStart-only, tunneling-only, reverse-OLC) + регресс 30 python-тестов.

**P0-4. SRTP/SDES + SIP TCP/TLS + digest-auth/REGISTER (A1, A2, A3).**
Файлы: новый `vks-gateway-sip/src/srtp.rs` (или переиспользовать `vks-media-sfu::dtls`? — нет, там DTLS-SRTP для WebRTC;
для SIP нужен SDES), `uas.rs`/`rtp_bridge.rs` (SDP), `uac.rs` (auth/redirect/TLS-dial), `gateways.rs` (TLS-листенер 5061).
Зависимость: **после** P0-1 (иначе двойная работа по SDP). Риск: SRTP-контекст в hot-path (нулевое копирование
не должно пострадать — использовать in-place `aes-128-cm`), KAT-векторы RFC 3711 обязательны.
Тесты: KAT + e2e `pjsua --srtp --tls`.

**P0-5. Смешанная комната (мандат Lead; *не* «мост SRTP⇄plaintext», см. §8.2): браузерное плечо в MCU-режиме + снятие `room_has_peer` guard (B11 + `lib.rs:756-771` + `gateways.rs:1005-1033, 1116-1120, 2505-2520`).**
Это единственный патч, который делает MCU **смешанным** (browser + терминал), т.е. «полноценным».
Файлы: `webrtc_runtime.rs` (реестр), `gateways.rs` (маршрутизация gateway-участника через SFU-плечо),
`vks-media-sfu` (адаптер plaintext-RTP ↔ SRTP-сессия; либо ре-использовать `SfuEngine` как второй выход).
Зависимость: **требует P0-4** (без SRTP-плеча нечего мостить). Риск: **высокий** — двойное кодирование/лишняя
задержка; ломает инвариант «одна комната — один медиа-движок». Тесты: e2e browser+SIP+H.323 в одной комнате.
Я осознанно ставлю его последним из P0: он самый дорогой, а первые четыре дают измеримый рост дозвона.

**Не входят в первые 5 P0 (и это осознанно):** H.239/BFCP-content (P1 — не влияет на дозвон, влияет на «полноценность»),
H.235 (P1), RAS/GK (P0 для GK-парков, но не для direct-IP 20/20; при подтверждении сценария — поднимается в P0),
session-timer/100rel (P1), `received/rport` (P1, дешёвый — кандидат в «нулевой» патч).

### 4.1 Зависимости, критерии приёмки и «как доказать без ботов»

| Патч | Зависит от | Критерий приёмки (что именно должно стать правдой) | Цена ошибки |
|---|---|---|---|
| P0-1 RTCP feedback | — | (1) answer на H.264 содержит `a=rtcp-fb:<pt> nack pli` и `ccm fir`; (2) принятый PLI выставляет «нужен IDR» у энкодера ≤ 1 кадр; (3) H.323-пир получает `videoFastUpdatePicture` при потере; (4) SR не уходит пиру, не согласовавшему feedback | пере-генерация IDR каждые 1 с (сейчас — фактическое поведение `gateway_video.rs:665-683`) → лишний битрейт и «пульсация» качества |
| P0-2 DTMF | — | `telephone-event` в answer; событие из RTP-пакета доходит до `dtmf_rx` комнаты; `POST /dtmf` даёт 5 корректных RTP-пакетов (0x0a/0x8a, duration inc) | терминал начнёт слать события в PT, который мы объявили, но не парсим → «мёртвые» нажатия |
| P0-3 H.323 OLC/fastStart/tunnel | — (ASN.1 готов) | 3 фикстуры: SETUP+fastStart, Connect без h245Address (туннель), OLC с reverse → все три доходят до `connected` с audio | регресс 30 существующих python-тестов; рассинхрон «номер OLC → порт» |
| P0-4 SRTP/TLS/auth | P0-1 (SDP-поля) | (1) KAT RFC 3711 проходит; (2) SDP с `a=crypto` дозванивается с `srtp`-пиром; (3) 401 → повтор с корректным Digest; (4) TLS-листенер 5061 принимает INVITE по `sips:` | шифрование не в hot-path, а «рядом» → рассинхрон ROC/MKI; утечка ключей в лог |
| P0-5 WebRTC⇄шлюз | P0-4 | browser + SIP + H.323 в одной комнате видят/слышат друг друга; `room_has_peer`-guard удалён; задержка добавленного плеча ≤ +20 мс (**не проверено**, цель) | двойное кодирование; ломка инварианта «один медиа-движок на комнату» |

**Как доказать без ботов (важно для спора с media-core, у которого ботов нет):**

1. **Юнит-фикстуры PER** — в репозитории уже есть реальные PER-векторы от Polycom (`PROVENANCE.md:29-33`):
   добавить по одной фикстуре на fastStart/tunneling/reverse-OLC/`miscellaneousCommand` → `test_h323_sidecar.py`/`test_h323_listener.py`.
2. **SDP-юниты** — «оффер → ответ» таблично (уже так: `uas.rs:735-1089`, `rtp_bridge.rs:754-1769`): добавить строки
   AVPF, `a=rtcp-fb`, `m=application` (B6), `sendonly` (B4), High-profile (B8).
3. **Фейковый UAS/UAC на loopback** — в тестах проекта уже есть приём «UDP-сокет как пир» (`uac.rs:398-428`,
   `gateways.rs:5109-5171`): этим проверяются 401-retry, PRACK, `received/rport` — без сети и без живых устройств.
4. **pjsua/ffmpeg как внешний пир** — только для SRTP/TLS e2e; это единственный пункт, где нужен внешний рантайм
   (`pjsua` сейчас **отсутствует** в системе, `PLAN_ORCHESTRATION_v0.md:13`; установка — отдельное решение Lead).
5. **Живые 20/20 — последними**, когда P0-1..P0-4 закрыты: иначе мы снова получим «20/20 одинаковых вызовов»,
   как в `t07b`, и не отличим фикс от совпадения.

---

## 5. Контраргументы против позиции media-core и мои ответы

**Медиа-позиция (C):** «главный блокер — compositor/mixer: nearest-neighbor, per-viewer encode, монолит `engine.rs`;
нужен shared composite canvas + SIMD + zero-copy».

| # | Аргумент media-core | Мой ответ (с доказательством) |
|---|---|---|
| 1 | «Медиа горячее, CPU упирается в композитор» | Это **не измерено**: по плану Lead (`PLAN_ORCHESTRATION_v0.md:13` — «Ботов нет в системе»), а `perf-lab` прямо заявляет, что без harness/изоляции любые цифры — фикция. Протокольные дыры, наоборот, доказуемы статически: `file:line` в §2. Нельзя выигрывать спор критерием, который нечем измерить, и проигрывать тот, который проверяется чтением |
| 2 | «Оптимизируем композитор — получим 20 участников» | В 20 участников **не с кем соединиться**: комната с WebRTC-пиром отдаёт шлюзу **486/отказ** (`gateways.rs:2505-2520`, `1019-1028`, `1116-1120`). 20 участников «в MCU» = 20 gateway-вызовов без браузеров; это не сценарий MCU, а тест транскодинга |
| 3 | «Замер ускорения композитора докажет пользу» | Любое ускорение энкодера/композитора **ненаблюдаемо** на приёмнике, пока нет `a=rtcp-fb`/PLI: после потери IDR картинка стоит до следующего штатного keyframe (B1/D1). Мы оптимизируем сцену, которую зритель не видит |
| 4 | «Протокол — это «фичи», а медиа — производительность» | Наоборот: у нас протокольные дыры **бинарные** (420/488/486/нет RAS/нет SDP-шифрования), а медиа-проблемы — **градации качества**. При критерии «20/20 устройств» бинарные блокеры доминируют математически: 0 дозвонов ≠ 10 % дозвонов |
| 5 | «Медиа-правки быстрее внедрить» | Не факт: SRTP/SDES + TLS — это 1 новый модуль + SDP-поля (дни), а SIMD-zero-copy композитор — переписывание `engine.rs` (недели) с риском сломать раскладки. По «устройства/человеко-день» P0-1..P0-3 дают больше |
| 6 | «Тогда докажи, что твои патчи дадут 20/20» | Честно: **не дадут сразу** — см. §3: даже после P0-1..P0-4 останется ≈4-6/20 SIP (secure/TLS/GK/AVPF-класс) и ≈5-7/20 H.323 (GK/H.460/H.235/G.729). Но это **адресуемые** пункты с известным списком, а не «неизвестно, где узкое место» |
| 7 | «Медиа всё равно станет блокером №2» | **Согласен и это не противоречит моей позиции.** Моя позиция — про **порядок**, не про важность: сначала снять бинарные блокеры (P0-1..P0-4), затем медиа-рефакторинг. Компромиссная формулировка: «P0 — протокол; P1 — медиаядро; параллельно — `perf-lab` harness, без которого медиа-пункт не закрыть» |

**Где я готов уступить (и при каких условиях):**
- Если сценарий — **только браузерные комнаты** (WebRTC), протокольная цепочка SIP/H.323 не важна вообще, и приоритет media-core верен.
- Если Lead подтвердит, что «20 SIP + 20 H.323» — это 40 вызовов **в gateway-only комнату** без content/DTMF/SRTP
  (т.е. только транскодинг), тогда часть P0 (A1/A2/A3/P0-5) уходит в P1, а media-core/`net-resilience` поднимаются.
  Но тогда «MCU» ≠ то, что описано в позиции (и в ТЗ), и это надо явно зафиксировать в финальном плане.

---

## 6. Открытые вопросы (≤5)

1. **Q1. Есть ли список 20 SIP + 20 H.323 устройств** (модели/прошивки/транспорт/auth/GK)? Без него моя оценка §3 — класс-усреднение
   с уверенностью 75 %; со списком её можно сузить до каждой модели.
2. **Q2. Целевой сценарий: смешанная комната (browser + терминалы) или gateway-only?** От этого зависит,
   P0-5 (мост SRTP⇄plaintext) — обязательный или отложенный.
3. **Q3. Мандат шифрования: SDES (`a=crypto`) для SIP + H.235 для H.323, или DTLS-SRTP, или только TLS-транспорт
   (SRTP не обязателен)?** Это меняет объём P0-4 в разы.
4. **Q4. H.323: достаточно ли direct-IP (`h323:alias@ip`), или нужен GK-режим (RAS registration)?**
   Если GK нужен — C1 поднимается в P0 и сдвигает весь план.
5. **Q5. Входит ли контент (BFCP/H.239) и DTMF в критерий «полноценный MCU» для 20/20,**
   или они отдельная фаза после дозвона? Если входит — D2/D3 остаются P0, а B7/C5/D4/D5 надо планировать в тот же спринт.

---

## 7. Что уже сделано хорошо (честный баланс, чтобы матрица не читалась как «всё плохо»)

- **ASN.1 PER — зрелый**: генерация Pycrate 0.8.1 из официальных H.225/H.245/H.235, хеши входов, пиннинг рантайма,
  строгий контроль «хвостов», 30 python-тестов (`PROVENANCE.md:7-33`, `test_h323_sidecar.py` 24, `test_h323_listener.py` 6).
- **SIP UAS/UAC — настоящая транзакционная работа**, а не мок: `100/180/200`, ACK-ветки 2xx/non-2xx (`sip_message.rs:123-153`),
  CANCEL по drop (`uac.rs:102-168`), монитор диалога с ответом на OPTIONS/BYE (`uac.rs:329-391`).
- **SDP-парсер на стороне шлюза — fail-closed**: конфликтующие `fmtp` не «последний выигрывает», а `!conflict!`
  (`rtp_bridge.rs:1118-1138`); AAC-LATM gate (`uas.rs:409-551`); H.265 DON-diff (`:694-708`) — это уровень выше среднего.
- **H.323 UAS пережил 20 одновременных вызовов** с корректной очисткой процессов (`t07d`), включая graceful shutdown и allowlist.
- **Медиа-мост** умеет G.711/G.722/G.723/G.726/G.728/Opus/H.264/H.265 — по кодекам мы не «пустые».

Вывод: проблема не в том, что «ничего нет», а в том, что **цепочка не замкнута**:
есть транспорт без SRTP/TLS, есть SDP без feedback/полного набора m-line, есть H.245 без H.239/H.235/fastStart/RAS,
есть RTCP-TX без RTCP-RX, есть ASN.1 без обработки половины PDU. Именно это, а не скорость медиа,
определяет, сколько устройств из 20 (и 20) реально окажутся в конференции.

---

*Артефакт подготовлен без сборки/тестов/живых вызовов (правило задачи). Все `file:line` — по текущему `master`
на 2026-10-25; при расхождении после правок — перепроверить по `git diff`.*

---

## 8. Раунд 1 Lead: мандат, live-калибровка H.264 и ответы A–D

### 8.0 Мандат (зафиксировано Lead; меняет приоритеты §4, не меняет матрицу §2)

| Вопрос §6 | Решение Lead | Следствие для плана |
|---|---|---|
| Q1 парк | 20 SIP + 20 H.323 + микс 10/10. Профили: **1080p60 H.265/Opus, 1080p30 H.264 High/G.722, 720p30 H.264 Baseline\|VP8/G.711a, 360p15 Baseline/G.729**. Боты наши, SDP — Polycom/Cisco/Yealink/TrueConf-совместимые | оценки §3 пересчитаны в §8.6; профили 1080p30-High, 720p30-Baseline(3.0) и VP8 **не проходят** видео сегодня (§8.1) |
| Q2 сценарий | **Смешанная комната**: браузер (WebRTC/SFU-плечо) + SIP + H.323, общий композит | P0-5 обязателен, но переформулирован: это не «мост», а **отсутствующее браузерное плечо MCU** (§8.2) |
| Q3 шифрование | **Мандат**: SRTP (SIP, мин. SDES) + H.235 (H.323); DTLS-SRTP для WebRTC уже есть (str0m). TLS для SIP — **P1**, не блокер | A1 остаётся P0; A2 понижен до P1; C6 (H.235) поднимается до **P0** |
| Q4 GK/RAS | **Не мандат**: прямой H.323 (Q.931 1720) достаточно; GK/RAS — **P2** | C1 понижен до P2; «20/20 H.323 direct-IP» достижим без RAS |
| Q5 content/DTMF | **BFCP↔H.239 — мандат M5** (P1, не блокер дозвона); **DTMF — P1** (вход в конференцию), не в критерий ёмкости | B7/C5/D4/D5 остаются P1; D2/D3 остаются P0 по влиянию на UX, но не на ёмкость |

**Итоговый порядок P0 (обновлён):** P0-1 (rtcp-fb/PLI/FIR) → P0-2 (DTMF full-duplex) → P0-3 (H.323 OLC/fastStart/tunnel)
→ P0-4 (SRTP+SDES, H.235) → P0-5 (браузерное плечо MCU + смешанная комната). P1: TLS, 100rel/session-timer, H.239/BFCP-content, G.729/VP8-приём. P2: GK/RAS, H.460, H.261/H.263, CANCEL-481.

### 8.1 Live-калибровка H.264: `42c01e` → отказ, `42c01f` → приём (подтверждено Lead на UAS .93)

**Измерение Lead (живой UAS, не статика):** offer `profile-level-id=42c01e` (Baseline, level 3.0) → answer `m=video 0`;
offer `42c01f` (Baseline, level 3.1) → answer `m=video 41006 H264 42c01f`. Аудио-плечо работает: `m=audio 41002 PCMU`.

**Точный механизм (код, 3 независимых условия в одном `if`):** `crates/vks-gateway-sip/src/uas.rs:663-676`:
1. `packetization-mode` должен быть **ровно `"1"`** (`:664`) — оффер без `a=fmtp` (режим по умолчанию 0, RFC 6184 §8.1) отвергается;
2. `bytes[0] != 66` → отказ (`:674`) — Main (77/`0x4d`) и High (100/`0x64`) отвергаются **всегда**;
3. `bytes[2] < 31` → отказ (`:674`) — любой level < 3.1, включая **3.0 (`0x1e`=30)**, 3.0 — самый частый level у SIP-видеотелефонов.
Ответ **эхом повторяет офферный fmtp** (`uas.rs:270-276`, комментарий-обоснование `uas.rs:46-48`), тогда как наш TX-энкодер — Baseline 3.1
(`gateway_video.rs:61-72`: `profile: "baseline"`, `level: "3.1"`). Т.е. при приёме High-оффера мы бы соврали о своём TX.

**Патч из 3 частей (спец-корректный, RFC 6184 §8.2.2):**
(a) **Приём**: принимать любой `profile_idc`, который умеет декодировать наш ffmpeg-декодер (Baseline/Main/High — `gateway_video.rs:304-395`
уже спавнит `ffmpeg -f h264 -i pipe:0`), и любой level ≥ 1.0; убрать условия (2) и (3) в `uas.rs:674`, ослабить (1) — принимать `mode 0/1`, а отправлять всегда FU-A (`mode 1`).
(b) **Ответ**: не эхо, а **свой TX-профиль**: `packetization-mode=1;profile-level-id=42c0<level>` (+ `level-asymmetry-allowed=1`, если он был в оффере). RFC 6184 §8.2.2:
`profile-level-id` в answer описывает поток **отвечающего**; эхо чужого профиля — ложное объявление.
(c) **TX-уровень**: если оффер без `level-asymmetry-allowed` и level < 3.1 → запускать энкодер с `-level:v <level-оффера>` (поле `VideoProfile.level` уже конфигурируемо, `gateway_video.rs:69-71`), иначе отказывать по level осознанно.

**Таблица «SDP-профиль терминала → SIP видео» (live-подтверждение отмечено ★):**

| Профиль в оффере | Сейчас | Причина (file:line) | После патча 8.1 |
|---|---|---|---|
| H.264 **Baseline 3.0** `42c01e` / `42e01e` (массовые видеотелефоны, Yealink/Polycom VVX) | **video=0** ★live | `uas.rs:674` (level 30 < 31) | PASS (ответ `42c01e`, геометрия клампится под level 3.0) |
| H.264 **Baseline 3.1** `42c01f` / `42e01f` | **PASS** ★live (`m=video 41006`) | — | PASS (ответ `42c01f`) |
| H.264 **High** `64001f` (мандат 1080p30) | video=0 | `uas.rs:674` (idc 100 ≠ 66) | PASS (TX остаётся Baseline) |
| H.264 **Main** `4d001f` | video=0 | `uas.rs:674` (idc 77 ≠ 66) | PASS |
| H.264 без `a=fmtp` (mode по умолчанию 0, single-NAL) | video=0 | `uas.rs:664` (требует `mode=1`) | остаётся video=0 **осознанно**: наш TX — FU-A (`mode=1`), а mode-0-приёмник FU-A не примет; отдельная ветка single-NAL — P2 |
| H.264 с `level-asymmetry-allowed=1` | video=0 при любом level <3.1 | `uas.rs:674` | PASS, ответ `42e01f` + asymmetry |
| **VP8** `VP8/90000` (мандат 720p30 VP8) | video=0 | в SDP-пути шлюза нет ветки VP8 — только H264/H265 (`uas.rs:592-708`) | **P1**, не P0: нужен RX-декод VP8; TX остаётся H.264 (браузерное плечо — §8.2) |
| **H.265** `profile-id=1`, tier 0, DON-diff 0 | PASS | `uas.rs:694-708` | PASS |
| H.265 `profile-id=2` (Main10) | video=0 | `uas.rs:696-698` | P2 (нет 10-bit пайплайна) |
| **G.729** (PT 18) — мандат 360p15 профиля | **488** NoCommonAudio | `uas.rs:454-551` (нет G.729), тест `uas.rs:882` | **P1**: decode-only через `codec_worker` (`AudioCodec::G729` уже есть, `codec_worker/mod.rs:38-52`), TX — G.711 |
| **Opus** (мандат 1080p60) | PASS | `uas.rs:409-453`, `opus.rs:1-30` | PASS |

**Вывод по калибровке:** мандат Q1 в части «1080p30 H.264 High» и «720p30 Baseline(3.0)» **сегодня не проходит видео** —
это не оценка, а следствие одной строки `uas.rs:674`; 720p30-VP8 не проходит по отсутствию VP8; 360p15-G.729 не проходит по аудио.
Аудио-плечо live подтверждено (PCMU, порт выдан) — мой §3.1 «дозвон проходит» верен для compliant-профиля.

### 8.2 A) Архитектура: почему «мост SRTP⇄plaintext» не решает — и 3 варианта унификации

**Соглашаюсь с Lead: моя формулировка §2/§4 P0-5 была неточной.** Факты (проверено кодом):

1. **Два разных представления данных.** `MediaEngine` — общий трейт (`vks-core/src/ports/media_engine.rs:1-20`: «две независимые реализации … клиент не знает о режиме»),
   но SFU (`vks-media-sfu/src/webrtc_room.rs:416`) **пересылает RTP-пакеты**, а MCU (`vks-media-mcu/src/engine.rs:1087` `composite_frame`, `:790` `tick_video_for_viewer`) **композитит пиксели**. Мост «SRTP⇄plaintext» переносит байты, но не превращает пакеты в кадры — Lead прав.
2. **Реальный WebRTC в MCU-комнате отвергается явно**: `lib.rs:756-762` → `webrtc_mcu_room_unsupported` («Real WebRTC is not bridged into MCU-mode rooms»); комната с участником шлюза — `lib.rs:763-771` → `webrtc_gateway_room_unsupported`. Т.е. «MCU» сегодня **по конструкции** браузеров не принимает.
3. **Композит не отдаётся браузеру нигде**: единственный production-потребитель `tick_video_for_viewer` — `gateway_video.rs:644` (SIP/H.323 egress);
   `frame_for_viewer`/`composite_frame` — только тесты (`gateways.rs:4757`, `gateway_video.rs:1330/1431`). Аудиомикс — аналогично (`gateway_audio.rs:359-367`, `gateway_aac.rs:293-298`).
4. **Что уже есть и переиспользуется** (это и делает задачу «дни»): ffmpeg-субпроцесс-паттерн `gateway_video.rs:487-560`;
   приём кадра в композитор `gateway_video.rs:549` (`set_participant_frame`); композит `gateway_video.rs:644`; str0m-действия — `Media(MediaData)` (`webrtc.rs:92`),
   `write_media` (`:299`), `write_media_matched` (`:235`), `request_keyframe` (`:251`), событие `KeyframeRequest` (`:98`);
   **Opus in-process** через `rusty-opus` (`vks-gateway-sip/src/opus.rs:1-15`, `Cargo.toml:27`) — для браузерного аудио ничего нового не нужно.

| Вариант | Что меняется (файлы) | Объём | Риск | ADR | Дни |
|---|---|---|---|---|---|
| **(a) Браузеры через McuEngine, str0m = транспорт** | новый `crates/vks-server/src/webrtc_mcu.rs` (~700–1000 стр): decode-воркер на браузера (ffmpeg VP8/H264 → yuv420 → `set_participant_frame`), audio Opus→f32→`set_audio_slice` / микс→Opus→`write_media`; egress: `tick_video_for_viewer` → ffmpeg encode (H264 CBP) → `write_media`; PLI: `request_keyframe` ↔ `KeyframeRequest`; SDP answer в MCU-режиме = 1 аудио + 1 видео (композит); снять `lib.rs:756-771` | ~700–1000 стр + 2 правки-выключателя | средний (CPU, согласование PT/SSRC, порядок кадров) | новый ADR: «MCU: единый медиапуть, str0m как транспорт; SFU-режим — только browser-only» | **5–8** + 2–3 стабилизация |
| **(b) Мост SFU-track→McuEngine (сохранить SFU-пересылку)** | (a) + переключение режима на лету: `Room::switch_mode` (`vks-core/src/room.rs:452`), renegotiation всех браузеров при входе шлюза, два egress-пути одновременно | (a) + ~400–600 стр оркестрации | **высокий**: mid-call renegotiation, рассинхрон «браузер видит N треков, шлюз — композит», двойная нагрузка | ADR: «гибридный режим» | 10–14 |
| **(c) Отказ от SFU-плеча в MCU-режиме = (a) + политика** | (a) + правило «комната с call_strings/шлюзом создаётся в MCU-режиме», SFU остаётся для browser-only/1:1 | (a) + ~100 стр политики | **низкий** (один опыт в комнате) | тот же ADR, что (a) | **5–8** |

**Рекомендация: (a) как техническое ядро + (c) как политика; (b) отклонить.** Причина: (b) даёт два разных пользовательских опыта в одной комнате
(браузер видит «сырые» треки, терминал — композит), что противоречит мандату Q2 «общий композит», и стоит вдвое дороже. (a)/(c) укладываются
в **дни, не недели**, потому что все четыре нужных примитива (ffmpeg-воркер, кадр-в-композитор, композит-в-энкодер, Opus in-process) уже существуют;
нового кода — только «клей» между str0m-действиями и композитором.
Ограничение, которое надо принять явно: браузер получает **один композитный поток** (не N треков) и **H.264 CBP на выходе** (VP8-tx — +3–5 дней).

### 8.3 B) H.323: минимальный набор PDU, concurrency-модель и CPU-арифметика

**Concurrency (проверено кодом):**
- **Входящий UAS**: один процесс-родитель, неблокирующий `select`-цикл, **`os.fork()` на вызов** (`h323_listener.py:2`, `:1077`);
  лимиты: `max_children=64` (`:763`, env `VKS_H323_UAS_MAX_CHILDREN`, `uas.rs:138-139`), **`max_children_per_ip=8`** (`:764`, env `VKS_H323_UAS_MAX_PER_IP`, `uas.rs:140-141`),
  и **жёсткий Rust-лимит 20 вызовов** (`gateways.rs:3075-3079`, `clamp(1,20)`, default 20).
  → Для 20 входящих **с одного IP** (наш бот-хост) нужно поднять `VKS_H323_UAS_MAX_PER_IP ≥ 20`, иначе соединение закрывается **до сигнализации** (`h323_listener.py:1071-1073`) — тихий отказ.
- **Исходящий UAC**: **один процесс на вызов** (`h323_sidecar.py:1-4`: «One H.323 call per process; stdin commands and stdout JSON events only»), 20 вызовов = 20 процессов.
- **GIL не проблема**: процессы (fork), не потоки. Медиа через Python не идёт вообще («Python/Pycrate never handles media samples», `h323_sidecar.py:4-5`).
- **Пул сайдкаров/SO_REUSEPORT не нужен**: один listener + fork-дети; узкое место — не приём H.225, а ffmpeg-транскоды (ниже).

**ОКРУЖЕНИЕ (проверено, ранее мною ошибочно записано в блокеры):** ASN.1-runtime **есть** — в venv
`/home/pro/.local/share/vks-mcu/h323-venv/lib/python3.14/site-packages` лежат `pycrate_*` пакеты, и
`venv/bin/python3 -c "import h323_asn, pycrate_core"` → OK; живой сайдкар в текущем запуске
(`H.323 UAS listening bind=0.0.0.0:1720`, дочерний процесс vks-server) это подтверждает.
Моя ошибка: проверял `import pycrate` системным `python3` (и в venv — несуществующий top-level модуль `pycrate`:
поставлены плоские `pycrate_core/pycrate_asn1rt/...`, а `h323_listener.py:47-55` импортирует именно их).
`pip` внутри venv отсутствует — это ограничение установки, а не блокер запуска.
**Следствие:** H.323-сигнализация доступна; для 20 входящих нужен бот-генератор и `VKS_H323_UAS_MAX_PER_IP ≥ 20`
(`h323_listener.py:764`, `uas.rs:140-141`), иначе тихий `conn.close()` (`:1071-1073`).

**Минимальный набор PDU/хендлеров для 20 H.323 audio+video+H.239 (что есть → что добавить):**

| Слой | Есть | Добавить (P) |
|---|---|---|
| Q.931/H.225 CS | Setup (`:769-799`), CallProceeding+Connect (`:635-649`), ReleaseComplete (`q931.rs:39-44`) | разбор `fastStart`/`h245Tunneling`/`h245Control` в Setup (**P0**), `Alerting`/`Progress` (P2), Connect с принятым fastStart (**P0**) |
| H.225 RAS | — | **не нужно** (GK — P2 по мандату Q4) |
| H.245 MSD | request/response/Ack (`:489-499`) | — |
| H.245 TCS | request/ack/reject (`:324-340`, `:467-538`) | `userInputCapability` (**P1**), H.239-capability, G.728/G.729/H.263 (P1), H.235 (P0 по мандату Q3) |
| H.245 OLC | forward-only (`:342-357`), ack/reject (`:435-457`), closeLC | **`reverseLogicalChannelParameters`** (**P0**), `dynamicRTPPayloadType` для аудио (**P1**, DTMF RFC2833 в H.323) |
| H.245 команды | roundTripDelay | **`miscellaneousCommand`→`videoFastUpdatePicture`** (**P0**), `UserInputIndication` (**P1**) |
| H.239 | — | второй video-OLC (sessionID 2/3 + `mediaControlChannel`) + token через `GenericMessage` (**P1**, мандат M5); `ExtendedVideoCapability.videoCapabilityExtension` уже в кодеке (`schemas/h245.asn:783-788`, `h323_asn.py`: 20 вхождений) → **перегенерация ASN.1 не нужна**, нужны только OID H.239 как константы |
| H.235 | ASN.1 есть (`h323_asn.py:17-24` — класс `H235_SECURITY_MESSAGES`, `schemas/h235.asn`) | `h235Key` в Connect/TCS + `encryptionSync` (**P0** по мандату Q3) |

**CPU-арифметика (измерено локально: ffmpeg 8.0.1, `-threads 1`, `testsrc2`, 5 с, 36 vCPU; это НЕ замер нашего сервиса, но даёт потолок):**

| Операция (1 поток) | user-время на 1 с видео | Ядер на поток |
|---|---|---|
| H.264 1080p30 encode (libx264 ultrafast zerolatency) | 0.54 с | **0.54** |
| H.264 1080p60 encode | 0.98 с | **0.98** |
| H.264 720p30 encode | 0.31 с | **0.31** |
| H.264 360p15 encode | 0.08 с | 0.08 |
| H.264 1080p decode | 0.18 с | 0.18 |
| **H.265 1080p30 encode (libx265 ultrafast)** | **5.84 с** (6.65 с wall на 5 с — не real-time) | **5.84 → непригодно per-viewer** |
| VP8 720p30 encode (libvpx realtime, cpu-used 8) | 0.02 с | 0.02 |

Арифметика: один видео-участник в MCU-комнате = **2 ffmpeg** (decode+encode, `gateway_video.rs:487-510`) ≈ 0.49 ядра @720p30, 0.72 @1080p30, 1.16 @1080p60.
На 36 ядрах потолок: **≈70 участников @720p30**, **≈48–50 @1080p30**, **≈31 @1080p60** — до аудио, композита и ОС.
Мандат Q1 = 40–50 видео-участников ⇒ **потолок по CPU наступает**, а «20/20 в 1080p60 H.265» **невозможен** при per-viewer энкоде (5.84×N ядер).
**Следствие для мандата:** H.265 — **только ingress** (транскод 265→264/VP8 на egress); это и есть тезис media-core — но он становится верным **после** P0-1..P0-5, а не вместо них.
Кандидат в P1 media-core: **shared-composite** (один энкод на всех при одинаковой раскладке) — меняет арифметику в разы; сейчас каждый зритель получает свой энкод (`tick_video_for_viewer`, `gateway_video.rs:637-660`).

### 8.4 C) 20 SIP-устройств: по каждому — причина отказа `[ОЦЕНКА]`

Основание: §3.0 (ворота) + live-калибровка §8.1. Список моделей гипотетический (манджат Q1 — «боты наши»), поэтому **вся таблица — [ОЦЕНКА]**,
кроме отмеченных ★ (код) и ★live (замер Lead). Сценарий: UDP/RTP, direct `sip:<room>@.93:5060`, комната без WebRTC-пира (иначе 486 — §3.0 G6).

| # | Устройство | Дозвон | Причина ровно по коду |
|---|---|---|---|
| 1 | Polycom RPD 3.5 (fixture) | ✅ audio+video ★live | compliant PCMU + `42c01f` |
| 2 | Polycom Group 500 | ✅ audio+video | `42e01f`/CBP |
| 3 | Polycom VVX 1500 (видеотелефон) | ✅ audio, ❌ video | level 3.0 < 3.1 (`uas.rs:674`) |
| 4 | Cisco SX20 | ✅ audio+video | CBP 3.1 |
| 5 | Cisco 8865 | ✅ audio+video | CBP 3.1 |
| 6 | Cisco 7821 (audio-only) | ✅ audio | video m-line нет |
| 7 | Yealink T58A | ✅ audio, ❌ video | `42e01e` (level 30) |
| 8 | Yealink VC800 | ✅ audio+video | CBP 3.1 |
| 9 | Grandstream GVC3220 | ✅ audio+video | CBP 3.1 |
| 10 | Grandstream GRP2616 (audio-only) | ✅ audio | — |
| 11 | Avaya IX Room | ❌ | `Require: timer` → **420** (`gateways.rs:2236-2258`) |
| 12 | Avaya J189 | ✅ audio | — |
| 13 | Huawei TE30 | ✅ audio+video | CBP |
| 14 | Lifesize Icon 450 | ✅ audio, ❌ video | High 64001f (`uas.rs:674`) |
| 15 | Sony PCS-XG80 | ✅ audio, ❌ video | Baseline 3.0 / H.263 |
| 16 | Asterisk/FreeSWITCH trunk | ✅ audio+video | plain RTP (`t07b`-паттерн) |
| 17 | Vinteo MCU | ✅ audio+video | plain RTP |
| 18 | TrueConf MCU .92 | ✅ audio+video | ★live `t07b` |
| 19 | Secure-профиль (SRTP/SDES mandatory) | ❌ | `RTP/SAVP` → 488 (`uas.rs:188`) |
| 20 | SIP-транк провайдера (digest 401) | ❌ | 401 → `Rejected` без повтора (`uac.rs:312-323`); REGISTER нет |

**Находка live-прогона Lead (не протокол, но блокер сценария «20+»):** при 20 участниках в комнате
с дефолтной раскладкой `preset_default_13` (13 окон) MCU пишет `layout has no free geometry slot;
participant stays unassigned` (`vks-media-mcu/src/engine.rs:605`) → **7–8 участников не попадают в композит**.
Поэтому «дозвонились» и «попали в микс/видео» — разные метрики: даже 20/20 дозвонов на дефолтной
комнате дают максимум 13 видимых окон. Авто-геометрия — P0 в зоне media-core; в бенчмарке фиксировать
отдельно `sessions_connected` vs `sessions_rendered`.

**Итог [ОЦЕНКА]:** аудио **17–18/20** (отказы: 11, 19, 20 — все «корпоративные»); среди дозвонившихся видео предлагают 14
(у 6, 10, 12 видео нет; 11/19/20 не дозвонились) — видео проходит **10 из 14 (71 %)**, т.е. **10/20** в терминах всех 20.
DTMF 0/20, content 0/20, SRTP 0/20.
**Ранжирование причин отказов по частоте:** (1) level<3.1 в оффере — 3–4 из 20; (2) `Require: timer|100rel` — 1–2; (3) digest/TLS/SRTP — 2–3; (4) High-only — 1–2.
Комплианс-оффер (`42c01f`, PCMU, UDP) проходит **с первого раза** — live это подтвердил; мой прежний диапазон 13–15 был занижен из-за недооценки доли видео-отказов по level и переоценки доли secure-устройств.

### 8.5 D) DTMF RX: где именно теряется — и минимальный full-duplex патч

**Подтверждаю file:line (три независимых места):**
1. **Входящий UAS вообще не объявляет RFC 4733:** answer собирается из `InboundMedia` без поля DTMF (`uas.rs:240-287`; структура `InboundMedia` `uas.rs:29-52` — поля DTMF нет);
   мост получает `dtmf_payload_type: None` (`gateways.rs:2577`) ⇒ TX-путь тоже мёртв (`gateway_audio.rs:418-433` отправляет только при `Some`).
2. **Если бы объявил — RX всё равно дропался бы:** `RtpSource::accept` (`gateway_audio.rs:559-573`) принимает только `pt == payload_type` (или G.711-взаимозамену
   для 0/8, `:570-573`); пакет `telephone-event` (PT 101/119) → `None` (дроп). Это ровно та строка, которую видит Lead.
3. **H.323: оба пути недоступны** — исходящий OLC с `dynamicRTPPayloadType` для аудио отвергается (`h323_listener.py:389-391`), `UserInputIndication`
   не обрабатывается (`_on_control` знает только TCS/MSD/OLC/closeLC/roundTripDelay, `:467-538`), capability не объявляется (`:324-340`).

**Минимальный патч full-duplex (порядок = порядок правок):**
1. `uas.rs`: в `InboundMedia` добавить `dtmf_payload_type: Option<u8>`; в `negotiate_with` (`:185-237`) искать `a=rtpmap:<pt> telephone-event/8000` (приоритет 101, затем 96–127), проверять `a=fmtp:<pt> 0-15` как опциональный.
2. `uas.rs::build_answer`: добавить `a=rtpmap:<pt> telephone-event/8000\r\na=fmtp:<pt> 0-15` **только если был в оффере** (RFC 3264 §6.1: answer ⊆ offer).
3. `gateways.rs:2568-2587`: передать `dtmf_payload_type: media.dtmf_payload_type` вместо `None` (и в `controls.dtmf_payload_type`, поле уже есть в `:1131-1140`).
4. `gateway_audio.rs`: `RtpSource` получает `dtmf_payload_type: Option<u8>`; в `accept` — ветка «`pt == dtmf` → вернуть payload как событие» до аудио-проверки (`:566-573`);
   в цикле бриджа — парсер RFC 4733 §2.3 (event 0–15, E/R биты, duration; end-пакет ×3 по §2.5.1.4) → событие в комнату (тот же путь, что `POST /api/v3/abonents/{id}/dtmf`, `api_v3/sessions.rs:2021`).
5. H.323 (P1): в TCS — `userInputCapability: dtmf`; обработчик `userInputIndication` в `_on_control` и отправка при нажатии из комнаты; RFC2833-в-H.323 — отдельно (требует п. «dynamic audio PT», `:389-391`).

**Тесты (без живых устройств):** юнит `negotiate` с `telephone-event` → `Some(101)`, без него → `None`, и answer содержит/не содержит 101;
юнит `RtpSource::accept` с PT 101 → `Some` (не дроп); юнит-парсер события (E=0 → 1 событие, E=1 duration=1600 → end); python-тест TCS с `userInputCapability`;
e2e на loopback-ханессе `uac.rs:398-428` (там уже поднимается фейковый UAS) — отправка PT 101 и проверка события в комнате через admin-API.

### 8.6 Пересчёт 20/20 после калибровки и мандата

| Метрика (мандат Q1) | Было в §3 [ОЦЕНКА] | Уточнено после §8.1–§8.5 |
|---|---|---|
| SIP audio (20) | 13–15 | **17–18** (live: compliant-профиль проходит; отказы — только secure/Require/digest) |
| SIP video (20) | 8 | **10/20 (10 из 14 дозвонившихся с видео, 71 %)** — level<3.1 и High-only отсекают 4 из 20 |
| H.323 audio (20) | 11–14 | **13–16**; сайдкар живой (см. §8.3), нужен бот-генератор 20 входящих и `MAX_PER_IP ≥ 20` |
| H.323 video (20) | 7–9 | **8–11** после P0-3 (fastStart/tunnel/reverse-OLC); H.263/H.261-only (2–3) останутся audio-only |
| H.239/BFCP content | 0 | 0 — мандат M5, P1; ASN.1-база готова (`h245.asn:783-788`), кода нет |
| DTMF (обе стороны) | 0 | 0 — патч §8.5 (P1), не в критерии ёмкости |
| Смешанная комната | невозможно | невозможно; архитектура ответа — §8.2 (a)+(c), 5–8 дней |
| CPU-потолок 36 ядер | не учитывался | **≈70 @720p30 / ≈50 @1080p30 / ≈31 @1080p60** видео-участников; H.265-egress невозможен |

**Что изменилось в моей позиции (честно):**
1. P0-5 переформулирован: не «мост SRTP⇄plaintext», а **отсутствующее браузерное плечо MCU** (Lead прав, §8.2) — но он остаётся обязательным, т.к. мандат Q2 = смешанная комната.
2. Порядок «протокол → медиа» **усилился**: теперь у него есть цифры. Протокольные дыры — 0/1 (бинарно), CPU-потолок — количественный, но он наступает **после** того, как участники вообще смогут войти. При этом **я уступаю в главном споре**: медиа-оптимизация (shared-composite, per-viewer delta) — не «потом когда-нибудь», а **обязательный P1 сразу после P0**: без неё мандат 20+20+20 в 1080p математически не выполняется на 36 ядрах.
3. Понижения по мандату: TLS (A2) → P1, GK/RAS (C1) → P2, DTMF → P1 по ёмкости (но P0 по UX), H.239/BFCP → P1/M5.
4. Новое в матрице: **pycrate отсутствует на VM** (§8.3) — это блокер бенчмарка H.323, не строка кода; и **`MAX_PER_IP=8`** тихо режет 20 входящих — обе строки в план бенчмарка, не в патч.

### 8.7 Статус реализации P0 (по мандату Lead, раунд 2)

| P0 | Файлы (file:line) | Тесты |
|---|---|---|
| **P0-1** H.264 gate relax + level clamp | `vks-gateway-sip/src/uas.rs:719,728,748` (idc 66/77/100, любой level, TX fmtp `42c0XX` = min(offered,3.1), asymmetry); `vks-server/src/gateways.rs:243` (`video_level_limits`, Annex A max-fs/max-mbps), `:264` (`inbound_video_profile` — level в `VideoProfile`, при level<3.1 геометрия клампится, чтобы libx264 не отверг кадр: level 3.0 → 640×360, SDP и `-level:v` согласованы) | `uas.rs:873/913`; `gateways.rs:3832/3856` (таблица уровней + кламп 720p→3.0) |
| **P0-2** RTCP RX + AVPF (PLI исправлен после live-проверки: PSFB 206 FMT=1, не RTPFB 205) | `vks-gateway-sip/src/rtcp.rs` (новый: `parse_feedback:46`, `parse_compound:128`, `build_rr:146`, `build_pli:156`, `build_fir:167`); `uas.rs:216-334` (`RTP/AVPF` только если оффер AVPF, `a=rtcp-fb` только предложенные, `a=rtcp` для audio/video); `gateways.rs:342` (`spawn_video_rtcp`: RR раз в 5 с + RR в ответ на SR, PLI/FIR → `keyframe_requests`), `:2793` (split RTCP-сокета пары: клон читает, оригинал шлёт SR) | `rtcp.rs` 7 тестов; `uas.rs:929` (AVPF/AVP); `gateways.rs` — компиляция + приёмка media-core |
| **P0-3** DTMF full-duplex | `uas.rs:52-58` (`dtmf_payload_type`), `:759` (`telephone_event_payload`, clock = аудио), `:293-312` (answer: формат-лист m-line + `a=rtpmap/a=fmtp 0-15`); `gateways.rs:2745-2758` (pt в controls + relay-таска `:306`), `:422` (`keyframe_requests` в controls); `gateway_audio.rs:544-609` (`RtpSource` принимает DTMF PT), `:647` (парсер RFC 4733), `:304` (дедуп по (event, ts)) | `uas.rs:951`; `gateway_audio.rs:803`; `gateways.rs:3866` (маппинг 0-15→символ) |

**Что осознанно НЕ сделано (решением Lead):** форс-IDR по PLI — P4b (рестарт ffmpeg-воркера, ADR-0028); восстановление сейчас ≤1 с (keyint=30); H.323 `videoFastUpdatePicture`/`userInputIndication` — P1; relay DTMF идёт только шлюзовым плечам (у браузеров нет канала DTMF в сигнализации — P2); NACK-обработка для SIP — P4b (только приём/учёт).
**Live-подтверждение (Lead, новый бинарь 17:44):** `42c01f` → `m=video 41002`, 519 пакетов/8 с; `42c01e` → принято, объём 11 944 B против 26 446 B у 3.1 — **кламп геометрии (640×360) реально работает, x264 level error в логе нет**; `4d001f`/`64001f` → приняты нашим TX-профилем; аудио 408 пакетов/8 с ≈51 pps.
**Известное поведение (не баг):** при быстрых последовательных прогонах пробника с переиспользованием портов старый сокет ещё жив (`source_latch` фиксирует первый источник) → WARN `audio RTP rejected by source validator`; при одиночном прогоне потерь нет. Учитывать при трактовке `rejected_packets` в замерах.

### 8.8 Live-валидация RTCP и DTMF (25.10, вечер)

**На замороженном артефакте `f6446b81…` (production MCU, SIP-плечо, 720p30+PCMU, 16 с, пробник `/tmp/rtcp_probe/sipbot_rtcp.py`):**
- **RR от MCU раз в 5 с:** 5.007 / 9.998 / 15.004 с (в начале вызова первый тик теряется — RTCP-сокет пробника открывается после ACK; не баг).
- **Немедленный RR на наш SR:** SR в 6.017 → RR в 6.038 (+21 мс); SR в 12.016 → RR в 12.036 (+20 мс).
- video rx 4036 пакетов / 3.90 МБ, loss 0 %; аудио 801 пакет / 50.06 pps.
- **Найден баг (в замороженном бинаре):** PLI парсился как RTPFB (PT 205) FMT=1, тогда как на проводе PLI — **PSFB (PT 206) FMT=1** (RFC 4585 §6.3.1) → любой PLI от терминала в `f6446b81…` **игнорируется**, `rtcp_pli`/`keyframe_requests` не растут. FIR (PT 206 FMT=4) обрабатывался корректно. Исправлено в `crates/vks-gateway-sip/src/rtcp.rs` (+2 теста: реальные байты PLI, generic NACK не считается PLI). **Входит в следующую сборку.**

**На debug-инстансе из текущего дерева (`target-proto/debug/vks-server`, порты 18080/15060/11720/46000, `RUST_LOG=debug`):**
- 3 PLI от пробника → 3 строки `RTCP received … pli=1` (всего `rtcp_rx_packets`=5: 2 SR + 3 PLI); `keyframe_requests` вырос на 3 и потреблён O7-фермой: `vks-media-mcu/src/farm.rs:621 group keyframe request consumed … requested=1 keyint=30` ×3.
- RR: 5.023 / 10.012 / 15.021 с (дельты 5.002/5.009 с) + ответы на SR +20 мс.
- **DTMF full-duplex:** SIP-пробник предложил `m=audio … 0 101` → answer `m=audio 41000 RTP/AVP 0 101` + `a=rtpmap:101 telephone-event/8000` + `a=fmtp:101 0-15`; RFC 4733 digits 5,# (общий SSRC/seq/ts с аудиосессией) → `DTMF received from SIP terminal and relayed … delivered=1`; H.323-терминал (H.245 `UserInputIndication.signal`, реальный вызов G.722 PT 9) → `DTMF received from H.323 terminal and relayed … delivered=1`. Дедуп: 3 RTP-пакета одной цифры → одно событие.
- **H.323 interop:** реальный H.225/H.245-вызов через форк sidecar в `/tmp` (оригиналы не тронуты): `MiscellaneousCommand.videoFastUpdatePicture` → INFO `H.245 videoFastUpdatePicture received; keyframe request counted`; `UserInputIndication` → `dtmf_digit`. Офлайн: 17 python-тестов (+miscCommand, signal, alphanumeric, unknown choice).
- **Ограничение:** обратного направления H.323 (DTMF из комнаты → `UserInputIndication` терминалу) нет — P1; relay только приёмный.

**PLI-фикс проверен на production через `/metrics` (бинарь `81d8e509…`, PID 1709372; P4a вывел счётчики, debug-инстанс не нужен):** 3 PLI от SIP-пробника на RTCP-порт MCU (RTP+1) → `encoder_group_keyframe_requests{codec="h264",height="720",room="1e0337c6-…",width="1280"} 3`, при этом `encoder_group_frames` растёт (339 к 12-й секунде), `encoder_group_keyframes 12`, `viewer_rtp_packets 2822`, `viewer_rtp_dropped_frames 0`; RR 5.005/9.997/15.005 с, немедленные RR на SR +20 мс, video rx 4038 пакетов / 3.92 МБ, loss 0 %. Это и есть приёмка PSFB-фикса на production-артефакте.

**Ранее: PLI-фикс проверен на production-артефакте `3353dcb2…` (итерация 3b) через debug-инстанс** (счётчики наружу не отдаются; P4a выведет их в `/metrics`): второй инстанс того же бинаря на 18080/15060/46000, `RUST_LOG=vks_media_mcu=debug`, production PID не тронут. Результат: 3 PLI от SIP-пробника → `RTCP received … pli=1` ×3 → `farm.rs:624 group keyframe request consumed … requested=1 keyint=30` ×3; RR 5.001/10.010/15.001 с, немедленные RR на SR +20/+21 мс; video rx 4340 пакетов / 4.27 МБ, loss 0 %.

**H.323-приёмка на production 3b (живой MCU, PID 1679027, 1 реальный вызов, комната `1e0337c6-…`, 18:35 MSK):** H.323-терминал (форк sidecar, G.722 PT 9) подключился; в живом INFO-логе 3b — `H.323 inbound participant landed in room`, `H.245 videoFastUpdatePicture received; keyframe request counted`, `DTMF received from H.323 terminal and relayed digit=5/#` (delivered=0 — в комнате не было второго шлюзового плеча; сам relay проверен на debug-инстансе с SIP-плечом: delivered=1). Строки видны без debug-лога, т.к. это INFO. Это закрывает H.323-половину P0-2/P0-3 на production-артефакте.

**Production-подтверждение (итерация 3b, артефакт `3353dcb2…`):** все правки этого раздела вошли в production-бинарь. Подтверждено Lead'ом: Baseline/Main/High принимаются с level-clamp, RTCP RR/AVPF/PLI работают, DTMF full-duplex SIP↔H.323, `videoFastUpdatePicture` → групповой keyframe. Итог итерации 3b — **0.360 ядра/устройство** на 20×720p30 (итерация 2: 0.954), см. `BENCHMARK_REPORT.md`. Не входит (P1): обратный DTMF в H.323 (UserInputIndication терминалу), H.323 `userInput` TX.

**Тесты:** `cargo test -p vks-gateway-sip` → 148 passed, 1 ignored; `cargo test -p vks-server` → 118 lib + 31 integration, exit 0. Известный тайминговый флейк: `gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps` (4/6 в изоляции) — не связан с правками (DTMF-ветка при `dtmf_payload_type: None` не выполняется).

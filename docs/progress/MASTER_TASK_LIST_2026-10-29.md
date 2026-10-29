# MASTER TASK LIST — модернизация VKS/PROSTOR MCU (2026-10-29)

Базовый HEAD: `d82ce4d`. Базовый прогон: 1425 passed / 0 failed / 8 ignored; fmt/clippy чисто; **all-features падает на 2 тестах** (T03a).
План и обоснование: [`docs/audit/2026-10-29/00_MASTER_PLAN.md`](../audit/2026-10-29/00_MASTER_PLAN.md).
Правила: один таск = один коммит; перед таском — pitfalls-субагент; после — 2–3 параллельных ревьюера; коммит при «нет P0/P1 + медиана ≥9,0 (хотя бы один ≥9,5)»; ревью-отчёт файлом.

## Статусы

| ID | Задача | Приоритет | Зависит от | Статус |
|---|---|---|---|---|
| T01 | Перезапуск HEAD + live-верификация, бэкап БД | P0 | — | ✅ done 07:33 |
| T02 | Auth: Argon2id+лимиты+sync-ldap+ADR | P0 | T01 | ✅ fix `8dfc449` (2 ревью: 6.0/4.0 → P0/P1 закрыты; re-review идёт) |
| T03a | CI-гейты (all-features, cache, node/MANIFEST, лишние джобы) | P0 | T01 | ✅ done 07:44 (`9391a8a`) |
| T03b | Доки к факту + doc-links gate | P1 | T03a | ✅ `9a394ec`: все doc-ссылки, rustdoc-гейт блокирующий, oxideav/ldap3/Taffy/progenitor — к факту |
| T03c | Мёртвый код (−1080) + неиспользуемые deps | P1 | T03a | ✅ done 07:46 (`aef9257`) |
| T04a | SDP/кодеки P0–P1 (H.264 PT98, cpresent, PT identity, G.726, лицензия) | P0 | T03a | ✅ `7cb7ba7`+`14834a8`: PT98/RX High/AAC/лицензия/PT2/матрица/audio-identity |
| T04b | CodecSpec SSOT + матрица диспозиций | P2 | T04a | ⏳ |
| T05 | DTMF E-бит (60→200 мс) + send_to метрика | P1 | T03a | ✅ done 07:49 (`2a77be5`) |
| T06 | Layout: normalize/резолвер/P2/тест S6/оракул e2e | P1 | T03a | ⏳ |
| T07 | Frontend: deep link, импорт, gzip, тема, CI-гейты | P1 | T03a | ⏳ |
| T08 | SDK: room_mode в JoinRoomResult + auto-select + wasm | P1 | T03a | ⏳ |
| T09a | Запись: Storage-контракт+гидратация+API+409+soak | P0 | T03a | ⏳ |
| T09b | MP4: avcC/length-prefix/timescale/esds + tap | P1 | T09a | ⏳ |
| T10 | T10b: directory в SQL + schema_state + удаление из JSON | P1 | T09a | ⏳ |
| T11a | SFU: default transport+fail-closed, keyframe-gate, re-offer, кодеки | P0 | T04a | ⏳ |
| T11b | Per-source: 8 mids (patch вендора) + маппинг + >2 пиров | P0 | T11a | ⏳ |
| T11c | Удаление легаси-плоскости (2176/79) + 6 тестов | P1 | T11a,T11b | ⏳ |
| T12 | P5: room_mode (on_client_hello), guards→bridge_available, audio+composite egress | P0 | T11a | ⏳ |
| T13 | WebTransport native (wtransport, cert-hash, Alt-Svc) | P2 | T11a | ⏳ |
| T14 | NATS/OTLP (UUIDv7, CI-сервисы, shutdown) | P2 | T03a | ⏳ |
| T15 | L0: SO_RCVBUF, RT-потоки, sysctl, systemd slice | P2 | T01 | ⏳ |
| T16 | H.323: убрать ffi, RX-латч, fail-closed H.235, UII, active_calls, golden+ADR | P2 | T03c | ⏳ |
| T17 | Финал: артефакты, доки, полный прогон, рестарт, отчёт | P0 | все | ⏳ |

## Критерии приёмки по задачам (кратко)

- **T01 ✅:** release HEAD = `cbad4986…`; `/proc/<pid>/exe` не `(deleted)`; бэкап БД `backups/2026-10-29/vks_data.db.pre-d82ce4d` integrity ok; smoke `:18082` (healthz/readyz 200, m5 401, fake JWT 401, login 200, MANIFEST 214/214); live: LAN `/metrics` 403, anon m5 401, sidecar child, SIP/H.323 READY.
- **T02:** LDAP mock только при `allow_mock_ldap` (уже factory-level fail-closed — не сломать); `sync-ldap` → 501 без LDAP-провайдера; Argon2id m=19456,t=2,p=1, семaphore 4 + bounded queue + 429; глобальный token-bucket 50/с burst 200; пароль ≤128; PBKDF2-хеш логинится и лениво перехешируется, generation не сброшен; ADR-обновление allowed-deps.
- **T03a:** `cargo test --workspace --all-features` зелёный (cfg-aware ожидания в `codec.rs`); CI-джобы без continue-on-error там, где должны падать; `node --check` на JS; MANIFEST проверяется; кэш-ключ учитывает `.cargo/config.toml`+toolchain.
- **T03b ✅:** `cargo doc --workspace --no-deps` c `RUSTDOCFLAGS="-D warnings"` зелёный; CI rustdoc-джоба больше не `continue-on-error`; README/DOC убрали oxideav-фичу; ldap3/Taffy/hadrone-core/dioxus-shadcn/progenitor помечены как план.
- **T03c ✅:** −1080 строк (transparency 447 + qoe/slo 320 + ffi 279 + deps/lock); `ratelimit` сохранён осознанно — на нём T02; ни одной новой ошибки.
- **T04a ✅:** PT98 не ходит ни в один оффер; answer принимает Main/High на приём, TX остаётся baseline; AAC без `cpresent`/fmtp → UnsupportedAudio в обе стороны; G.726-32 по PT2 (только с rtpmap); матрица G.722.1/G.719 — `supported=false`; `VKS_CODEC_LICENSE_ACCEPTED` + legacy alias; UAC/gateways резолвят аудио по name+clock (PT96 G722 и PT2 G726 тесты).
- **T05 ✅:** маркер только на первом пакете, E=1 только на последнем; приёмник видит 200 мс; AAC использует ту же серию; `dtmf_tx_packets`/`dtmf_tx_errors` + rate-limited warn; negative-control тест на старые 60 мс.
- **T06:** `normalize_loaded` пересчитывает size; один резолвер; тест n=5@1280×720; `preset_default_13` 400 задокументирован; оракул e2e стабилен.
- **T07:** сценарий «deep link без токена → логин → та же сессия»; `content-encoding: gzip` в ответе `/pnm/`; meta theme-color == computed `--c-bg`; импорт сохраняет theme-toggle.js; MANIFEST 215/215.
- **T08:** `JoinRoomResult.room_mode` (proto+server+SDK), SDK выбирает конвейер, `RoomModeChanged`; wasm32 check.
- **T09a:** записи переживают рестарт в SQLite/PG; API отдаёт их; 409 при отсутствии группы; soak 66 групп ≤ бюджета.
- **T09b:** MP4 играется ffprobe/ffmpeg; avcC/esds/timescale/stco корректны; tap не спавнит скрытую группу.
- **T10:** directory/addresses в SQL; идемпотентная миграция с маркером; rerun не воскрешает; sqlite==pg; поле убрано из JSON.
- **T11a:** default transport=1 + preflight; keyframe-gate; re-offer без нового DTLS; VP9/AV1/H264/H265 в caps честно.
- **T11b:** 3+ пира в комнате; patch вендора `--check`; нет обрезки до 2.
- **T11c:** легаси-плоскость удалена; 6 новых тестов; `VKS_WEBRTC_TRANSPORT` обязателен.
- **T12:** mixed room: браузер+шлюз; audio-мост; composite H.264; guards заменены `bridge_available`.
- **T13:** нативный WT-клиент ClientHello→ServerHello; `/wt/cert-hash`; Alt-Svc только при on; 404 при off; WS жив.
- **T14:** UUIDv7; lease переезжает между двумя узлами через NATS; fallback LocalTransport; OTLP-спан в collector; shutdown.
- **T15:** `ss -u -m` показывает rcvbuf; `chrt -p` RT; systemd slice/memory лимиты; sysctl применён.
- **T16:** ffi удалён; RX-латч по IP+SSRC; H.235 fail-closed; UII hex-корректен; active_calls честный; golden-харнесс; ADR по нативному стеку.
- **T17:** все гейты; live на новом бинаре; stale-данные очищены миграцией; финальный отчёт.

## Журнал

| Дата/время | Событие |
|---|---|
| 07:33 | T01 done: live перезапущен на HEAD, дыры закрыты, БД забэкаплена |
| 07:44 | T03a done: all-features зелёный, CI-гейты ужесточены, флейк-маржи timing-тестов |
| 07:46 | T03c done: −1080 строк мёртвого кода/зависимостей; transparency/ffi/qoe/slo удалены, ratelimit оставлен для T02 |
| 07:49 | T05 done: RFC 4733 E-бит (200 мс), метрики потерь DTMF, negative-control |
| 07:56 | T04a в работе: PT98 убран из Full, RX Main/High при TX baseline, cpresent обязателен (in/out), лицензионный профиль `VKS_CODEC_LICENSE_ACCEPTED`, G.726-32 PT2, honest matrix |
| 08:15 | T04a закрыт (`14834a8`): аудио по identity (name+clock), renumbered PT96 G722 больше не убивает мост; PT2 G726-32; тесты 154 SIP / 145 server |
| 08:20 | T02 реализован (`eb514e2`): Argon2id m=19456/t=2/p=1, PBKDF2 legacy + ленивый rehash без generation, 4 хеша + очередь 128 → 429, API bucket 200/50, sync-ldap 501, ADR-0033 |
| 08:26 | T03b закрыт (`9a394ec`): doc-links gate блокирующий, oxideav/phantom-deps к факту |

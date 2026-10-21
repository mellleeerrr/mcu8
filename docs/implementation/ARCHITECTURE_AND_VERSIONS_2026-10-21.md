# VKS MCU: полная архитектура, стек и план до версий пакетов

Дата: 2026-10-21. Статус: рабочий документ под живой стенд
`http://127.0.0.1:8080/pnm/monitoring/sessions` (admin / admin_password).
Источник задач: `LIVE_MCU_TASKS_2026-10-21.md`.

## 1. Целевая топология

```
┌──────────────┐   HTTPS/WSS (WebRTC)    ┌─────────────────────────────┐
│  Браузер     │◄───────────────────────►│  vks-server (axum 0.8)      │
│  PNM UI      │   /pnm/*, /api/v3/*     │  ├─ vks-admin (REST v3)     │
└──────────────┘                         │  ├─ vks-signaling (WS SDP)  │
                                         │  ├─ vks-storage (SQLite/PG) │
┌──────────────┐   SIP/TLS или UDP 5060  │  ├─ vks-gateway-sip (rvoip) │
│ TrueConf     │◄───────────────────────►│  ├─ vks-gateway-h323        │
│ client .132  │   RTP 40000/udp         │  ├─ vks-media-sfu (str0m)   │
└──────────────┘                         │  └─ vks-media-mcu (oxideav) │
                                         └─────────────────────────────┘
```

Один процесс, один бинарник, одна SQLite/PostgreSQL БД, один каталог
`call_strings` (без отдельного `/addressbook`). UI — вендорский PNM 2.2.1.66,
ребрендированный, с точечными патчами контрактов.

## 2. Инварианты (не нарушать)

1. **Sans-I/O медиа**: парсинг/демукс RTP/RTCP и геометрия раскладки не
   делают I/O и не спят; сеть — только в адаптерах `vks-media-sfu`/
   `vks-gateway-*`.
2. **Прозрачность MCU/SFU**: один и тот же signaling/API; переключение
   `sfu ↔ mcu` не меняет контракт клиента.
3. **Один источник истины**: комната/сессия/раскладка/контакт живут в
   `vks-admin::resources` + storage; UI не хранит второй справочник.
4. **1-based позиции** в `video_layouts/calculate` и PATCH slots; 0-based
   только во внутренней геометрии `vks-layout`.
5. **Никаких эмодзи/стикеров** в подписях плиток; имя центрируется, обрезается.
6. **Секреты только из env**, dev-конфиг не равен prod.

## 3. Крейты и зоны ответственности

| Крейт | Роль | Ключевые файлы |
|---|---|---|
| `vks-core` | домен, порты, Room/Session/Abonent, media policy | `src/room.rs`, `src/ports/media_engine.rs` |
| `vks-proto` | protobuf-контракты media plane | `proto/` |
| `vks-common` | конфиг, ошибки, телеметрия | `src/config.rs` |
| `vks-auth` | JWT, PBKDF2, RBAC | `src/lib.rs` |
| `vks-storage` | SQLite/PostgreSQL, миграции | `src/sqlite.rs`, `src/postgres.rs` |
| `vks-layout` | пресеты, геометрия, 41+ тест | `src/preset.rs` |
| `vks-signaling` | WS signaling, SDP parse/build | `src/sdp.rs`, `src/handler.rs` |
| `vks-media-sfu` | str0m SFU, UDP demux | `src/udp_loop.rs` |
| `vks-media-mcu` | MCU mixing/layout apply | `src/engine.rs` |
| `vks-gateway-sip` | SIP/RTP bridge | `src/rtp_bridge.rs` |
| `vks-gateway-h323` | H.323 sidecar | `src/lib.rs` |
| `vks-admin` | REST `/api/v3/*`, PNM-совместимость | `src/api_v3/*` |
| `vks-recorder` | запись/трансляция (каркас) | `src/lib.rs` |
| `vks-cluster` | кластеризация (каркас) | `src/lib.rs` |
| `vks-server` | сборка, роутинг, системный сервис | `src/main.rs`, `src/gateways.rs` |
| `client/vks-client-sdk` | WASM/TS SDK (каркас) | `src/lib.rs` |

## 4. Стек и точные версии

Проверено `rustc --version`, `cargo metadata`, `Cargo.lock` на 2026-10-21.

### 4.1 Runtime / OS
- Ubuntu **26.04.1 LTS**, kernel **7.0.0-38-generic**
- systemd **259 (259.5-0ubuntu3.4)**, user-unit `vks-mcu.service`
- SQLite **3.46.1** (libsqlite3 через `sqlx`)
- Порты: TCP **8080** (UI+API), UDP **40000** (media), SIP/H.323 — по конфигу.

### 4.2 Rust
- toolchain **1.94.0**, edition **2021**, MSRV **1.94**
- release: `opt-level=3`, `lto=thin`, `codegen-units=1`, `strip=symbols`,
  `panic=abort`

### 4.3 Прямые зависимости (resolved)
| Пакет | Версия | Назначение |
|---|---|---|
| tokio | 1.53.3 | async runtime |
| axum | 0.8.9 | HTTP/WS API |
| tower-http | 0.6.11 | static/fs |
| sqlx | 0.8.6 | SQLite/PostgreSQL |
| jsonwebtoken | 9.3.1 | JWT |
| pbkdf2 | 0.12.2 | пароли |
| prost | 0.13.5 | protobuf |
| str0m | 0.24.1 | WebRTC SFU |
| rvoip | 0.2.5 | SIP stack |
| oxideav-scene | 0.1.4 | MCU compositor |
| utoipa | 5.5.0 | OpenAPI (dev/док) |
| prometheus | 0.14.0 | метрики |
| tracing | 0.1.44 | логи |
| tracing-subscriber | 0.3.23 | JSON-логи |
| serde | 1.0.229 | JSON |
| serde_json | 1.0.154 | JSON |
| uuid | 1.28.0 | ID |
| chrono | 0.4.45 | время |
| chrono-tz | 0.10.4 | таймзоны |
| figment | 0.10.19 | конфиг |
| thiserror | 2.0.21 | ошибки |
| anyhow | 1.0.104 | ошибки |
| bytes | 1.12.1 | буферы |
| futures-util | 0.3.34 | async |
| async-trait | 0.1.92 | порты |
| sha2 | 0.10.9 | CSP-хэши |
| base64 | 0.22.1 | токены |
| subtle | 2.6.1 | constant-time |
| ulid | 1.2.1 | ID |
| proptest | 1.x | property-тесты |

### 4.4 Frontend / E2E
- PNM vendor bundle **2.2.1.66** (Vue 3, минифицирован), путь
  `client/pnm-frontend/deployed/pnm`
- Node.js **24.21.0**
- Playwright **1.64.1**, Chromium **1243** (`~/.cache/ms-playwright`)
- E2E: `tests/e2e/live.js` (Playwright API, 34 проверки)
- Эталон: TrueConf MCU **2.2.1.66**, `https://192.168.1.92/`

### 4.5 Известные дубли версий (P2, кандидаты на унификацию)
- `thiserror` 1.0.69 + 2.0.21
- `rand` 0.8.8 + 0.9.5 + 0.10.3
- `getrandom` 0.2.17 + 0.3.4 + 0.4.3
Причина — транзитивные зависимости `rvoip`/`str0m`/`sqlx`; ломать нельзя,
но для нового кода фиксируем `thiserror=2`, `rand=0.9` (или 0.10 после
проверки API).

## 4.6 Фактическое состояние на 2026-10-21 (после прогона агентов)

| Область | Состояние | Доказательство |
|---|---|---|
| Живой стенд | `vks-mcu.service`, `/healthz` 200, UI `http://127.0.0.1:8080/pnm/monitoring/sessions` | systemd, curl |
| E2E сценарий | 34/34 checks, pageerror 0, console — 6 вендорных TypeError | `artifacts/e2e/result.json` |
| Раскладки | 65 manual-пресетов, сверка 1:1 с эталоном 1280×720 (879 слотов, 0 расхождений); 6 custom-схем; `calculate` 1-based | `crates/vks-layout/tests/fixtures/` |
| UI-матрица | 11 диапазонов: 1/1/5/7/7/3/6/14/3/9/9 схем; 2×2=7; плейсмент 4/8/9/25; clear-all AUTO | `artifacts/e2e-layouts/result.json` |
| SIP | PCMU + H.264, двустороннее медиа на `192.168.1.132` | RTP-счётчики, live E2E |
| H.323 | терминал отвечает `RELEASE COMPLETE adaptiveBusy` (внешний занятый ресурс), не регрессия кода | лог терминала |
| Media | RTCP демукс RFC 7983; 1-based слоты; P1 «замерший кадр» после clear-all исправлен | 51+78 тестов, `engine.rs:733` |
| Статистика | 4/6 виджетов реальные; диск KiB/`df -PkT`; workers теперь массив | `artifacts/e2e/08-statistics.*` |
| Данные | layout 147→29, call_strings 5→3, 0 dangling; default-раскладка переиспользуется, PATCH — copy-on-write | `/tmp/dedup_live.json` |

Осознанные ограничения (не выдаём за готовое):
1. Реальный транскод — только G.711 (PCMU/PCMA) и H.264. Полная SDP-матрица
   (AAC, G.719, G.722.1c, G.726, OPUS, H.261/263/265) объявляется опцией
   `VKS_SDP_CODEC_MATRIX=full`; production default — `Native` (только
   мостуемые), иначе терминал выбирает кодек без транскодера и медиа теряется.
2. H.323 не доказан живым вызовом: терминал `192.168.1.132` занят/отвечает
   `adaptiveBusy`. SIP на том же терминале работает.
3. Нет NACK/RTX/FEC/джиттер-буфера на SFU-пути; композитор игнорирует
   `z_order`.
4. `jsonwebtoken 9.3.1` → минимум 10.3.0: CVE-2026-25537 (medium, type
   confusion nbf/exp); в RustSec БД отсутствует, `cargo-deny` не поймает.
   `rustls-webpki`/`webpki-roots` — только optional TLS-граф.
5. `default_room_mode="sfu"`; MCU-композит включается per-room.

## 5. Пошаговый план доведения до production

### Этап 0. Инвентаризация и заморозка (0.5 дня)
- `cargo metadata`, `cargo tree --workspace --depth 1`, `cargo deny check`.
- Зафиксировать baseline: `artifacts/e2e/result.json`, скриншоты, версии.
- Бэкап `vks_data.db` → `/tmp/vks_data_before_cleanup.db`.

### Этап 1. Домен и storage (1 день)
- Каноническая модель: `Room`, `Session`, `VideoLayout`, `CallString`,
  `Abonent`; один `call_strings` на (protocol, URI).
- Миграция без потери данных; дедупликация; удаление orphan layout.
- Тесты: миграция, конфликт route/display_name, перезапуск.

### Этап 2. Signaling и шлюзы (2 дня)
- SIP: полный SDP offer/answer (матрица кодеков), 488 при отсутствии общего.
- H.323: вызов/завершение, state machine, таймауты, отображение в UI.
- WebRTC: ICE/DTLS/SRTP, renegotiation, simulcast.

### Этап 3. Media plane (2–3 дня)
- SFU: RTCP demux, NACK/PLI, jitter buffer, keyframe.
- MCU: композитор, layout apply, 1-based slots, переключение sfu↔mcu.
- Нагрузка: 25/34 участника, CPU/память, отсутствие sleep в горячем пути.

### Этап 4. Раскладки и UI (2 дня)
- 11 icon-вариантов + 1,2,3,4,5+,8,9,10+,25,34+.
- Подвыбор 2×2 = 7 схем; сохранение preset+size.
- Точная ячейка участника, «Очистить все видеоокна», подписи по центру.
- Паритет с эталоном по сетевым вызовам.

### Этап 5. Статистика и наблюдаемость (1 день)
- `/api/v3/statistics`: CPU/memory/disk/network, Resources/Workers.
- Prometheus `/metrics`, JSON-логи, healthz/readiness.
- UI-страница без зависаний, обновление без sleep.

### Этап 6. Безопасность и эксплуатация (1 день)
- Prod-конфиг: секреты только env, TLS-терминация, CSP, HSTS.
- JWT RS256 или HS256 с ротацией; RBAC; audit.
- systemd hardening: `NoNewPrivileges`, `ProtectSystem`, лимиты.
- Бэкапы SQLite/PG, ротация логов, graceful shutdown.

### Этап 7. Тесты и CI (1 день)
- `cargo test --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
- `NODE_PATH=... node tests/e2e/live.js` + Playwright trace.
- Регрессии: SIP/H.323, 2×2, clear-all, codecs.

### Этап 8. Приёмка (0.5 дня)
- Живой сценарий пользователя целиком, 34+ проверки, скриншоты.
- Code-review ≥ 9.5/10, commit, тег.

## 6. Критерии приёмки

- Комната создаётся и стартует из UI.
- SIP и H.323 участник `192.168.1.132` звонится, виден, говорит/показывает
  видео, завершается.
- Ручная раскладка 4: участник ровно в выбранной ячейке 1, все плитки равны.
- «Очистить все видеоокна» → Применить: все слоты AUTO.
- Матрица аудио/видео кодеков уходит в call_strings и SDP.
- `/api/v3/statistics` и UI-страница живые.
- Нет 501/5xx в сценарии; pageerror = 0.
- `cargo test`, `clippy -D warnings` зелёные.

## 7. Риски

| Риск | Уровень | Митигация |
|---|---|---|
| Вендорский UI кидает 6 TypeError при модалке (как эталон) | P1 | патч null-guard или допустимый фильтр в E2E |
| H.265 вырезан вендором из payload | P0 | патч бандла (сделано), тест |
| Реальный транскод только G711/H264 | P0 | честно документировать; поэтапно добавлять |
| `default_room_mode=sfu` маскирует MCU | P1 | per-room mcu тест |
| Один процесс = SPOF | P1 | cluster-крейт/внешний LB, бэкапы |
| Дубли call_strings/раскладок | P1 | миграция + уникальные индексы |

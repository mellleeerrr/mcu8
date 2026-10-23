# План живого MCU и UI — 2026-10-21

Статус: **исторический, закрыт** (перенесён в архив 2026-10-23). Все задачи
T0–T10 закрыты; живой трекер — [`../MASTER_TASK_LIST.md`](../MASTER_TASK_LIST.md).

## Текущий статус (2026-10-21, вечер)

- T0 — DONE: release собран, сервис `vks-mcu.service` перезапущен, `/healthz` 200.
- T1 — DONE: `tests/e2e/live.js`, 34/34 checks, `docs/progress/evidence/e2e/result.json`.
- T2 — DONE: 23/23 сетевых вызова эталона совпадают; P1-расхождения geometry/лицензии/единицы зафиксированы.
- T3 — SIP DONE (PCMU+H.264, двустороннее медиа); H.323 заблокирован терминалом (`adaptiveBusy`), не нашим кодом.
- T4 — DONE: 11 диапазонов (1/1/5/7/7/3/6/14/3/9/9 схем), 2×2=7, плейсмент 4/8/9/25, clear-all AUTO.
- T5 — DONE: подписи по центру по горизонтали, emoji нет; вендорский аватар над именем не двигаем.
- T6 — DONE: живые дубли схлопнуты (147→29 layouts), 1 `call_string` на адрес; остался root-cause default + COW.
- T7 — SDP-матрица реализована (18 audio PT + 6 video), production default `Native` (только мостуемые); H.323 — терминал.
- T8 — страница статистики живая (4/6 виджетов реальные), диск/workers исправлены; RTP/QoE-метрик пока нет.
- T9 — DONE: конфиги прочитаны полностью, мёртвых tracked-конфигов/модулей нет;
  удалены только регенерируемые `scripts/__pycache__` и
  `crates/vks-gateway-h323/python/__pycache__`; live DB/backups/target/archive/
  venv/legacy-bundles оставлены. Evidence `docs/progress/evidence/t09/`.
- T10 — DONE: release пересобран, сервис перезапущен на `configs/default.toml`;
  `cargo fmt/clippy`, workspace lib 16/16, Python 54/54; live Playwright
  **233/233**, page_errors 0, failure null. Evidence `docs/progress/evidence/t10/`.


## T0. Пересборка и безопасный перезапуск живого сервера

**Зачем:** текущий процесс :8080 запущен со старым бинарником; новые API раскладок
(`video_layout_grids` → 188, `templates` → 18 для 2×2) и `/api/v3/statistics` не
видны UI.

**Шаги:**
1. `cargo build --release -p vks-server` — дождаться успеха.
2. Снять текущий PID и аргументы; сохранить лог `vks_server.log`.
3. Перезапустить с тем же `--config configs/default.toml`; проверить, что процесс
   поднялся, порт 8080 слушается, `GET /pnm/monitoring/sessions` → 200.
4. Проверить API под admin JWT:
   - `/api/v3/video_layout_grids?page=1&page_size=200` → `count >= 180`;
   - `/conferences/{id}/video_layouts/templates?...` → для 2×2 ≥ 18;
   - `/api/v3/statistics` → 200 и `system/engine`;
   - `/api/v3/sessions` → 200.
5. Проверить лог на panic/ERROR после старта.

**Подводные камни:** несовместимая миграция SQLite; занятый порт; потеря
`vks_data.db`; неверный `candidate_addr`; старый формат `pnm-definitions-v1`;
`default_room_mode=sfu` маскирует MCU-путь.

**Критерий:** живой сервер отвечает новыми API, тестовая комната создаётся,
старые 111 сессий не потеряны.

## T1. Playwright-каркас и базовый обход нашего UI

**Зачем:** проверять цепочку вживую, а не только unit-тестами.

**Шаги:**
1. Найти/создать `tests/e2e/live.js` (Node 22, chromium уже
   установлен в `~/.cache/ms-playwright`).
2. Сценарий login → `/pnm/monitoring/sessions` → создать комнату → открыть
   комнату → «Добавить участника» → модалка с SIP/H323 адресом.
3. Снять network-лог запросов, которые UI делает на каждом шаге.
4. Скриншоты и HTML-отчёт в `docs/progress/evidence/e2e/`.

**Подводные камни:** CSP и inline-скрипты; JWT в localStorage; SPA-роутинг;
base path `/pnm/`; модалка рендерится в portal; таймауты на 111 сессиях.

## T2. Реверс-инжиниринг эталонного фронтенда 192.168.1.92

**Шаги:** SSH `pro@192.168.1.92` (пароль 1), найти web-root, `cat` конфиги
nginx/aiohttp, скачать/сравнить JS-бандлы, снять Playwright network trace
эталонных сценариев, сопоставить эндпоинты с нашим API.

**Подводные камни:** эталон TrueConf 2.2.1.66 на Python 3.8; возможен другой
формат `video_layouts`/`templates`; не копировать вендорские ассеты, только
контракты; не ломать лицензионные ограничения.

## T3. «Добавить участника» и вызов SIP/H.323 192.168.1.132

**Шаги:** реализовать/починить модалку, `POST /api/v3/sessions/{id}/participants`
(или фактический endpoint эталона), `call_strings` поиск, dial SIP/H.323,
состояния `StandBy → Calling → Connected`, отображение в списке, hangup.

**Подводные камни:** случайный `pid` автодозвона не совпадает с `contact_id`
раскладки; нет UAS/регистрации; SIP 200 OK без video; H.323 sidecar таймауты.

## T4. Раскладки: все варианты, ячейки и «Очистить все видеоокна»

**Шаги:** 11 icon-вариантов + числовые `1,2,3,4,5+,8,9,10+,25,34+`; подвыбор 2×2;
drag/add в конкретную ячейку; `apply_layout` в MCU; кнопка очистки; проверка
через Playwright, что выбранный пресет открывается и абонент в нужной ячейке.

**Подводные камни:** 1-based/0-based `position`; auto-раскладки без slots;
дубли 120 default-раскладок; шаблоны manual/auto; `preset_default_13`.

## T5. Подписи в плитке

**Шаги:** рендер имени по центру ячейки, без emoji/стикеров, обрезка/эллипсис;
санитизация уже есть в `resources.rs`; нужен растеризатор (`fontdue`) или
клиентский overlay — выбрать по факту эталона.

## T6. Единый справочник контактов

**Шаги:** один источник users/contacts/call_strings; убрать 3 ID на один адрес;
миграция; `/addressbook` не вводить, использовать `ui:call_strings:search`.

## T7. Кодеки и качество SIP/H.323

**Шаги:** матрица AAC/G711/G719/G722/G722.1(C)/G723/G726/G728/G729/OPUS и
H261/H263/H264/H264-high/H265; SDP offer/answer, fmtp, bitrate, 488 + cause;
переключение профиля в комнате.

## T8. Статистика

**Шаги:** `/api/v3/statistics` + отсутствие sleep; Playwright проверяет
обновление CPU/network/disk/memory и Resources/Workers.

## T9. Гигиена и удаление мёртвого кода/конфигов

**Шаги:** `cat` всех конфигов; удалить неиспользуемые `configs/*`, мёртвые
модули, дубли; `cargo machete`/`cargo udeps` при наличии; проверить `deny.toml`.

## T10. Полный E2E и commit

**Шаги:** Playwright-прогон «создать комнату → добавить SIP/H.323 → вызов →
аудио/видео → раскладка → статистика»; `cargo test`; `cargo clippy -D warnings`;
commit с описанием.

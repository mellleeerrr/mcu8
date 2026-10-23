# R6 — мастер-список задач (выровнен после ревью r6-tasks-reviewer, 6/10)

Дата: 2026-10-23. База: `7841ecc` + T1 (бейджи, не закоммичена на момент выравнивания).
Ревью списка: `r6-tasks-reviewer` — 6/10, правки P0-1…P0-8/P1-11/P1-12 учтены.
Порядок отличается от рекомендации ревьюера осознанно: T1–T3 идут первыми, потому
что это прямой пункт мандата владельца (UI 1:1 + наш конструктор + бейджи), а
backend-P0 (T7–T10) — известный долг, он не блокирует UI-приёмку. Регрессии/безопасность
не откладываются: T7/T8/T9 — сразу после T3.

## Правила выполнения (для каждой задачи)

1. Перед стартом — саб-агент «подводные камни» (P0/P1/P2, что упускаем).
2. Реализация минимальным кодом (приоритет: меньше кода/элегантность).
3. После — 2 саб-агента код-ревью: оценка 1–10 + необходимые правки; цикл, пока
   хотя бы один не поставит ≥9.5/10.
4. Правки, с которыми согласен; commit; отметка «выполнено» с артефактом
   (`Артефакт: <commit>` + `Ревью: <агент> <оценка>`); следующая задача.
5. Задача считается закрытой только при наличии live/тестового доказательства
   в `docs/progress/evidence/<task>/`.

## Вне скоупа R6 (явно, чтобы не терять)

- Мост WebRTC→браузер (композит в браузере) и per-viewer self-view — R7/ADR-0029
  (крупнейший P0 по r5-chain-media; в R6 не делаем).
- Пагинация/«+N more», pinOnly-страницы, PiP `vks_pip_*`, spring-motion — R7/R9.
- Реальный рендер текста (бейдж «+N») — после WebRTC-моста.
- qwen3.8-27b-uncensored-code: endpoint в сети не обнаружен (11434/1234/8000
  закрыты, ollama/lmstudio не установлены); в R6 делаем только мост-скрипт и
  инструкцию (T4a), T4b (реальная совместная работа) — после адреса от владельца.

## T1. UI-паритет: бейджи числа окон на всех карточках (В РАБОТЕ)
Сделано: `appendBadge` в `codian-layouts.js`, вендорский класс, 54 Codian + 5
конструктора, live: у каждой ровно 1 бейдж с верным числом, 0 ошибок.
Осталось: ревью ≥9.5, commit. Подводные камни: дубли при перерендере, observer,
aria, темы, MANIFEST.
Критерии: live 59/59 бейджей; `data-windows` == текст; один источник count.

## T2. Вкладка «Конструктор» 1:1 + наш конструктор (meeting-grid-layout)
Сделать: при выборе «Конструктор» — наш конструктор: карточки grid/pin(l/r/t/b),
число окон из вендорского ряда «Количество видеоокон», сторона пина, gap/margin;
вендорские классы/структура; auto/dynamic/manual/constructor не пишут чужие поля;
save→reopen сохраняет `vks_*`.
Подводные камни: `main-form__select` — MDC/Vue, 1:1 нативным select невозможен
(использовать вендорские чипы/сегменты); `editor.mode='manual'` не должен
дёргать mode-endpoint; `preset` обязан быть `vks_*`; `editValueField` только для
полей формы; регрессия auto→manual; дебаунс calculate.
Критерии: live auto→конструктор→grid/pin→сохранение→reopen; в auto секции нет.

## T3. Полная модель библиотеки: контролы и границы
Сделать: «Окон» 1..64 (pin 2..64), gap/margin 0..64, валидация, live-пересчёт;
64-схемная матрица id → валидная геометрия.
Подводные камни: gap/margin только в id (payload вендор отбросит); промежуточные
невалидные id; pin N<4 — продуктовое; дебаунс.
Критерии: тесты parse/geometry; live 64 схемы.

**Статус: закрыта решением (минимум кода + 1:1).** Число окон 1..64 уже даёт
вендорский ряд «Количество видеоокон»: конструктор пересчитывает `vks_*_<N>`
при каждом клике (проверено live 25→1→25). UI для gap/margin **не добавляем**:
вендорский save отбрасывает эти поля, контрол вводил бы в заблуждение и ломал
1:1; грамматика `g`/`m` остаётся в API (calculate/apply) и покрыта тестами
`vks-layout`: 54+4+9 = 67 passed, 0 failed (23:00).

## T4a. qwen-мост (скрипт + инструкция), T4b — вне раунда
Сделать: `scripts/qwen.sh` (OpenAI-compatible, `QWEN_BASE_URL`/`QWEN_MODEL`/
`QWEN_API_KEY`, таймаут 120 с), `docs/implementation/QWEN_INTEGRATION.md`,
формат «второе мнение» для агентов; понятная ошибка без env.
Критерии: `scripts/qwen.sh "ping"` → осмысленный ответ при заданном endpoint.

## T5. Мёртвый код (после T3; только после T10b, чтобы не конфликтовать)
Сделать (0 потребителей, риск≈0): `vks-signaling/src/reconnect.rs` (166),
`vks-client-sdk/src/webrtc_glue.rs` (170), `vks-signaling/src/{ws,wt}.rs` + фичи
(≈187), мёртвая половина `core_layouts` (≈150, impl Storage остаётся),
`vks-layout` мёртвые (plan_viewer/assign_by_role/rotation_state/any_overlap/
total_area — 113), `vks-media-mcu` `frame_for_viewer`/`set_self_view` (48),
`tower`→dev-deps, `CONFIGURATION_KEY` в `vks-common`, sqlx `_unstable-all-types`.
НЕ удалять: `subtle`/`sha2` (живы в api_key/local_db), `core_adapter.rs` (реализует
Storage), ветку self-view (до ADR-0029), `wasm-sdk` CI (job поймал реальный баг).
Подводные камни: правка трейта `vks-core` (4 impl'а: core_adapter×3 + mock rooms),
`gateway_video.rs:644` hot-path, публичный API admin.
Критерии: clippy --all-features, тесты, grep без ссылок, LOC −≈980.

### Статус T5a (выполнено)
- [x] T5a — удалено 552 строки (12 файлов, +17/−552; Cargo.lock −118):
  `vks-signaling/reconnect.rs` (166, + `pub use Backoff/ResumeNonce`),
  no-op заглушки `tungstenite_impl`/`wtransport_impl` + фичи `ws`/`wtransport`,
  dev-dep `proptest`; legacy standalone `layouts` в `vks-storage` (rows
  `Layout`/`Tile`, `LayoutRow`, insert/get/list в memory/sqlite/postgres, 2 теста);
  `Layout::total_area/any_overlap` (+тест); `tower` → dev-deps `vks-admin`;
  sqlx `_unstable-all-types`. Гейты: fmt 0, clippy -D warnings 0, workspace
  tests 1308→1301 passed/0 failed (7 запускавшихся тестов удалено; ещё 2
  feature-gated), wasm-sdk 0. Evidence: `docs/progress/evidence/r6-t5a/`.
  Перечень снятых `pub use` — в `result.json`. T5b (core_layouts trait-половина)
  — после T10b; T5c (viewer/self-view/webrtc_glue) — только по отдельному решению.

### Решения по T5 (после анализа подводных камней, HEAD 82d93b8)
T5 делится на T5a (сейчас) / T5b (после T10b) / T5c (только по отдельному решению):
- T5a ≈ 490 LOC, только реально мёртвое: `vks-signaling/reconnect.rs` + re-export
  и dev-dep proptest; заглушки `tungstenite_impl`/`wtransport_impl` + пустые
  фичи `ws`/`wtransport` (трейты/моки `WsSession`/`MockWs`/`WebTransportSession`/
  `MockWt` остаются — их используют тесты handler); legacy standalone `layouts`
  в vks-storage (НЕ `core_layouts`); `Layout::any_overlap/total_area`;
  `tower` → dev-dependencies; `sqlx` без `_unstable-all-types`.
- НЕ удаляем: `viewer.rs`/`plan_viewer`, `frame_for_viewer`/`set_self_view`/
  `tick_video_for_viewer` (ADR-0029, живой egress-fallback), трейты/моки ws/wt,
  `SlotRole`/Codian, `assign_by_role`/`RotationState` (S12), `webrtc_glue`
  (wasm-CI/задел R7), `core_layouts` trait-половину (T5b), `subtle`/`sha2`,
  `core_adapter`, миграции, `CONFIGURATION_KEY` (T6).
- Критерий LOC: фактический `git diff --shortstat` + зелёные гейты + отсутствие
  ссылок; «−980» не гоняем. Публичные API-брейки в непубликуемых крейтах
  допустимы, перечень снятых `pub use` — в evidence.
- Тесты удаляемого кода удаляются вместе с ним; новые числа (signaling,
  layout, storage) передаются в T6 для README.
- Коммит T5a — только свои Rust-файлы; мастер-док/frontend/MANIFEST/БД/логи
  не включать.

## T6. Конфиги и директории (полный cat), после T5
Прочитать полностью: configs/*.toml, deploy/**, .cargo/config.toml,
rust-toolchain.toml, Cargo.toml (workspace+crates), .gitignore, CI.
Исправить: preflight empty-ветка/комментарий default.toml:15; TEST-NET в README;
H.323-allowlist трижды (конфликт) — свести к одному источнику; README:81 «970
тестов» → фактическое число; README:58 «0001–0027» → до 0028; dev-IP в unit
(вынести в drop-in); `.work-tmp` не в git (ок), но 211 МБ на диске — почистить;
пустые features/дубли; SIP UAS release-note.
Критерии: каждый конфиг прочитан cat; 0 противоречий; чистый запуск default.toml
≠78, TEST-NET → 78.

Выполнено (2026-10-24):
- прочитаны cat: Cargo.toml (workspace + 16 crate-манифестов), rust-toolchain.toml,
  .cargo/config.toml, .gitignore, configs/*.toml, deploy/** (README, оба unit,
  vks.env.example), .github/workflows/ci.yml;
- default.toml: комментарий про preflight приведён к факту (поле читается только
  при VKS_WEBRTC_TRANSPORT=1; при выключенном — legacy SDP), убран хардкод IP
  стенда из комментария;
- H.323-allowlist: единственный источник значений — deploy/vks.env.example
  (безопасный loopback-only 127.0.0.1/32); deploy/README больше не дублирует
  конкретные CIDR (ссылается на переменные и «сузьте до LAN»); dev-юнит
  помечен DEV override;
- dev-IP и все машинные пути вынесены из deploy/systemd/vks-mcu.service:
  `%h/mcu`, candidate/allowlist — через `systemctl --user set-environment`
  или drop-in (описано в deploy/README); systemd-analyze verify — 0;
- SIP UAS release-note: deploy/README + vks.env.example честно фиксируют, что
  входящий SIP off by default, без auth/TLS и без IP-allowlist (только
  firewall, VKS_SIP_UAS_MAX_CALLS и token bucket 20/10 rps);
- README: ADR 0001–0031, тесты 1302 (0 сбоев, 8 ignored) по фактическому
  прогону, candidate/TEST-NET уточнены (RFC 5737, exit 78 при включённом WebRTC);
- пустые `[features] default = []` удалены (vks-admin, vks-gateway-sip);
  дубли dev-dependencies tokio удалены (admin/h323/sip/storage), sha2/subtle/
  serde_json переведены на workspace-наследование;
- .work-tmp почищен (47 МБ → 9 МБ; agents-отчёты удалены), /tmp освобождён
  от старых reviewer-чекаутов и playwright-профилей;
- критерии проверены: чистый запуск default.toml с изолированной SQLite —
  exit 124 (timeout, сервер жив, не 78); TEST-NET candidate при
  VKS_WEBRTC_TRANSPORT=1 — exit 78 с «documentation (TEST-NET) address».

## T7. Admin auth и контракт
Роль-гейт `GET /conferences/{id}` и `/ui/conferences/{id}` (Viewer без PIN),
списки с ролевым гейтом, `PATCH /profile` и `/sessions/{id}` (501→200/405 в
envelope), 409 дублей, mode-endpoint не перезаписывает preset/type/size.
Подводные камни: RBAC уже есть — переиспользовать; envelope `api-errors.js`;
не менять DTO; live-вызовы UI.
Критерии: тесты 401/403/409/200; Viewer PIN недоступен.

### Статус T7 (2026-10-24, коммит d7ddd4e)
- `require_moderator` — единая ролевая точка (Moderator+); карточка конференции
  и все 4 списка гейчены, у Viewer 403 (PIN недоступен), anon 401, admin 200.
  Решение Lead: списки — Moderator+, т.к. payload содержит PIN/полный конфиг;
  Viewer-дашборд при этом 403 (его `sessions:view` — не право на PIN).
- WS `sessions:started`: pin/has_pin вырезаны из broadcast (подписки не
  гейтятся по роли); UI перезапрашивает данные по событию.
- Дубли имён комнат: 409 envelope вместо 500 с SQL-текстом (create+rename,
  `Rooms::name_taken`); автосоздаваемые сессии получают `Quick Session N`.
- `PATCH /api/v3/profile[/]` — 200; пишет display_name/description/locale/
  timezone/emails/phones (общий helper с `update_user`), self-роль/группы/
  view_own_only игнорируются; GET /profile отдаёт эти поля из directory.
- `PATCH /api/v3/sessions/{id}[/]` и `/ui/sessions/{id}[/]` — 200 (reuse
  update_conference); раньше exact 405 без envelope, slash 501.
- mode-only не затирает preset/type/size; явный topology-payload работает.
- Тесты `t7_rbac_patches_and_duplicate_names` (401/403/200/409, обе формы URL),
  `t7_mode_only_preserves_preset_type_size`; gейты vks-admin 296/0.

## T8. Шлюзы: abonent_id, статус, allowlist
Приоритет `abonent_id` над `contact_id` (fallback), реальный статус вместо
фиктивного (тесты `network_integration.rs:766-771` закрепляют фиктивность —
править их), H.323-allowlist из одного источника, source_latch для всех сторон.
Подводные камни: phantom window у SIP; не сломать inbound SIP; allowlist конфликт
(см. T6).
Критерии: тест «оба id → окно по abonent_id»; live-звонок без фантома.

### Статус T8 (2026-10-24, коммит 12f2983)
- Приоритет id: `abonent_id → connection_id → contact_id → call_string_id`
  (было contact/call_string первыми). Тест «оба id»: окно 0 у реального
  `sip-...@sip-gateway`, `cs-<uuid>` фантома нет; live-данные t06b1
  подтверждали, что contact/call_string — адресная книга.
- Статус шлюзов честный: READY только при живом listener'е (RAII ActiveFlag
  SIP/H.323) или реальном health-факте, `FAILED` при транспортном отказе,
  иначе `DISABLED`; `registration` = listener, `connected` = ready.
  In-memory `register_uas()` остался только gate входящих INVITE. Тесты
  обновлены (fresh DISABLED, register/unregister не меняют статус, живой
  `serve_sip_uas` → READY → shutdown → DISABLED).
- H.323 allowlist: из dev-unit убраны `VKS_H323_UAS_ALLOW/MAX_*` — единственный
  источник `deploy/vks.env.example` (drop-in/EnvironmentFile).
- source_latch: sip/in = LatchFirst, sip/out/h323/in/h323/out = Pinned
  (H.323 inbound pinned — security-инвариант T07d, ослаблять нельзя);
  «все стороны» = зафиксированная политика, а не механическое true.
- Гейты: vks-server unit 108 passed/3 ignored, network_integration 22/0.

## T9. PG-типы и CI postgres
`rooms.created_at TEXT` vs `Uuid` (миграция/биндинг), PG-тест не `#[ignore]`,
CI service postgres, `--locked`.
Подводные камни: sqlite-путь; необратимые миграции — сначала тест; Uuid decode.
Критерии: PG-тест зелёный в CI; sqlite зелёный.

Выполнено (2026-10-24):
- корень бага: `created_at` в PG-схеме — `TEXT` (portable с SQLite), а Rust
  биндил `Uuid` → PG ожидал `uuid`, тест был `#[ignore]`, поэтому дефект не
  ловился. В миграции тип оставлен `TEXT` (необратимую миграцию не делаем),
  а биндинг исправлен: INSERT `created_at.to_string()`, SELECT
  `created_at::uuid AS created_at` (значения пишем только мы, UUIDv7-строки),
  `ORDER BY created_at` — лексикографический порядок UUIDv7 = хронологический;
- `init_roundtrip_live_db` больше не `#[ignore]`: с `VKS_STORAGE_PG_URL`
  проверяет migrations + User/Room roundtrip, без переменной честно пишет
  skipped (локально PostgreSQL нет);
- CI: новый job `test-postgres` (postgres:16, healthcheck, `--locked`,
  `VKS_STORAGE_PG_URL=postgres://postgres:postgres@localhost:5432/vks`);
  sqlite-путь: `cargo test -p vks-storage --locked` — 24 passed, 0 failed;
- локальная live-проверка PG невозможна (нет сервера/докера) — критерий
  «PG-тест зелёный в CI» закрывается первым запуском job'а; sqlite зелёный.

## T10a. Proto-поля `layout_id/preset/mode` + LayoutModeChanged (ADR)
## T10b. id-only apply (без `payload.slots`) — не требует proto
Реализовано (f9b0a1b): `payload.slots` необязателен; без него assignments
пусты, геометрия — из `payload.preset` (grids/Codian/ручной пресет), окна
заполняет MCU; auto без назначений/геометрии по-прежнему доходит до движка
пустым дескриптором (сброс на авто-сетку). Тест `id_only_apply_uses_preset_
geometry_without_slots` (vks_grid_9→9, codian_07→13).
## T10c. Прод-потребитель LayoutChanged (PNM WS / SDK) — после T10a/b

## T11. Evidence/golden
`codian_table.json:1227/1381/1591` → SUPERSEDED.md, фиксация 49 (0.4),
независимый fixture для неизменённых 43/46 по тексту владельца, golden для
`grids` (прецедент `codian_geometry_golden.rs`) — обязательный первый шаг перед
сокращением геометрии (r6-elegance).

### Статус T11 (2026-10-24)
- `docs/progress/evidence/codian-layouts/SUPERSEDED.md`: 43/46/49 — факты
  «таблица vs текст vs production»; 49 = `2/5` (0.4, сетка 4×5), источник —
  `ROUND4_PLAN_2026-10-23.md:56-57` / коммит `328c943`, а не измерение.
- `tests/fixtures/codian_spec_43_46_1280x720.json` + `tests/codian_spec_43_46.rs`
  (5 тестов): независимо от таблицы/чертежей; production совпал.
- `tests/fixtures/grids_geometry_1280x720.json` + `tests/grids_geometry_golden.rs`
  (3 + ignored-генератор): 174 id / 2178 окон, эталон-снимок production;
  `gen_grids_golden.py` воспроизводит.
- README codian-layouts обновлён (файлы, метод, CI). Проверки:
  `cargo test -p vks-layout --locked` — 84 passed, 0 failed, 1 ignored
  (ignored — только ручной генератор golden); `cargo fmt --all -- --check` и
  `cargo clippy -p vks-layout --all-targets --locked -- -D warnings` — чисто.

## T13. Счётчик «Количество видеокон» + перестройка всех вариантов (НОВОЕ, мандат R6 п.7)
Приоритет: сразу после T2 (прямой пункт мандата владельца). Сделать:
- вендорская разметка 1:1 `.dynamic-layout-configurator-counter` (`.counter`,
  `.counter__button` «мин.»/«макс.», `.counter-center-segment`, −/`+` с иконками
  `remove`/`add`, `input[type=number][max=100]`, `.tc-text-field*`) — внутрь нашей
  секции конструктора; видна при «Конструкторе» и «Ручной настройке».
- поведение: мин. 1, макс. 100, шаг 1, −/+ и input clamp, значение синхронизировано
  с текущим N; изменение N перестраивает ВСЕ наши карточки (grid/pin_l/r/t/b) под
  `vks_*_<N>` с сохранением выбранной стороны; превью — через calculate.
- N>25: серверная грамматика/геометрия до 100 (`vks-layout::grids::MAX_WINDOWS`
  64→100; опции g/m остаются 1..64), calculate/apply/compositor принимают 100,
  тест на 100 слотов.
Подводные камни: MDC-кнопки и ripple-классы; max=100 vs 64; вендорский fallback
`preset_equal_25`; не сломать вендорские чипы (reAdopt); a11y/disabled на границах;
payload 100 слотов в БД/движке.
Критерии: live 1→100 в счётчике перестраивает 5 карточек, активна `vks_<side>_<N>`;
save/reopen сохраняет N=100; тесты vks-layout; MANIFEST; 0 ошибок.

## T12. Тесты/CI-гигиена
Тесты приоритета id-геометрии и create_room-adopt pending, G722-флейк, calculate
error перечисляет grid, release-note SIP UAS, «970→факт».
### Статус T12 (2026-10-24, коммит 9efd5be)
- `create_room_adopts_pending_layout` — pending-раскладка (apply до create)
  подхватывается create_room: id/version/geometry сохранены, окно доступно
  сразу, после destroy не воскресает.
- calculate 400 перечисляет id конструктора (`vks_grid_<n>`,
  `vks_pin_{l,r,t,b}_<n>` с g/m, 1..100); тест проверяет текст.
- G722-флейк воспроизвести не удалось (полный прогон зелёный) — оставлен
  в бэклоге как неподтверждённый.
- Тесты приоритета id-геометрии закрыты в T8 (`gateway_layout_prefers_
  abonent_id_over_contact_and_call_string`); release-note SIP UAS и «970→факт»
  сделаны в T6.

### Статус T13 (финальное ревью T13.2)
- [ ] T13 — реализовано: вендорская разметка `.dynamic-layout-configurator-counter`
  (`__text`, `.counter`, `.counter-center-segment`, `counter__button` как
  TcButton-контейнер, `input[type=number][max=100][min=1]`), кнопки
  мин./−/+/макс., clamp 1..100, disabled на границах; изменение N перестраивает
  все 5 карточек через calculate (`vks_<side>_<N>`), сторона сохраняется;
  backend `grids::MAX_WINDOWS=100` (опции g/m остаются 1..64, `MAX_OPTION`),
  тесты `vks-layout` (100 окон, pin 100, g64/m64, 101→None), admin calculate
  покрывает `vks_grid_100`/`vks_pin_b_100`. Live evidence
  `t13-counter-1-100.json`: 13→+→14→−→13→input42→pin_r_42→+→43→input100→
  PATCH `vks_pin_r_100` (100 слотов)→reopen 100, «макс.» disabled;
  `t13-counter-100.png` (счётчик 100, 5 карточек с бейджем 100).
  Ревью T13 на `4c36a6c` (7/10) нашло P1: number-input терял фокус на каждой
  цифре (`onCounterInput` на `input` перестраивал секцию через innerHTML).
  Исправлено в T13.1: карточки перестраиваются без пересоздания счётчика,
  `input`-событие применяет N с дебаунсом 300 мс, `change`/Enter применяют
  сразу и клампят; разметка TcButton/TcTextField приведена к вендорской
  (`tc-button-container--base`, `mdc-button--text`, `tc-button__content`,
  `tc-button__icon`/`tc-button__text`, `tc-notched-outline`), добавлены
  aria-label/title для −/+; лимит `payload.size` в admin поднят с 64 до
  `grids::MAX_WINDOWS` (100); добавлены тесты apply 100 окон (gateways) и
  mode с size=100 (admin). Live: набор «100»/«42» с клавиатуры без потери
  фокуса (`t13-keyboard-100-42.json`), «+»/«−»/мин./макс./fill работают,
  save/reopen 100 (`t13-counter-1-100.json`, `t13-counter-100.png`).
  Повторное ревью T13.1 (`ebce17a`): 9.0/10, P0/P1 нет; P2 — aria-label/title
  только на wrapper-div (AX: «remove»/«add»), notched-outline не 1:1 вендору,
  кламп <1 откатывался к прежнему N. Исправлено в T13.2:
  - aria-label/title продублированы на настоящие `button`, иконки
    `aria-hidden="true"` → AX-имена «Уменьшить/Увеличить количество видеокон»;
  - outline 1:1 вендору: `div.tc-notched-outline.mdc-notched-outline` >
    `div.tc-notched-outline__{leading,notch,trailing}.mdc-notched-outline__*`
    (без `--upgraded`);
  - кламп как TcTextField(min=1): пусто/0/−5 → 1 (не откат к прежнему N).
  Live evidence `t13-a11y-and-clamp.json`/`.png`: AX-имена на button,
  outline-классы, typing 100 с сохранением фокуса (trace 1→10→100,
  `vks_grid_100`), 0/−5/empty→`vks_grid_1`, 42→`vks_grid_42`, focus сохранён.
  Финальное ревью T13.2 (`91dd55c`): 9.3/10, P0/P1 нет; P2 — клик
  «Ручная настройка» после конструктора не выходил из него (preset оставался
  vks_*, наш тайл снова active; pre-existing T13.1). Исправлено в T13.3:
  - document-level capture-listener на вендорские тайлы auto/dynamic/manual:
    сбрасывает modeTileEngaged/pendingAdopt; для manual — явный
    `preset_equal_25`/`individual` (вендорский дефолт);
  - с wrapper-div счётчика сняты aria-label/title (дублировали имя кнопки как
    named generic в AX) — атрибуты только на `<button>`;
  - evidence `t13-manual-tile-exit.json`/`.png`: ctor-100 → тайл «Ручная
    настройка» → `preset_equal_25/manual/25`, active tile manual, ctor active
    нет; возврат в конструктор → `vks_grid_25`; 0+blur → `vks_grid_1`,
    active=false. `t13-a11y-and-clamp.json` перегенерирован (wrapper aria=null).
  Известное ограничение (P3, не регрессия T13.3): у `vks_pin_*_100` 99
  миниатюр — полосы 144×7 (1 крупное окно + лента), документированный 16:9
  действует для pin до 16 окон; контракт T13 — количество/перестройка/save.
  Осталось: ре-ревью T13.3 (цель ≥9.5).

Из T2 (P1): session-route тест «auto→Конструктор не дёргает POST
/api/v3/ui/video_layouts/mode» (в конференц-редакторе switchLayoutMode нет,
0 POST; на session-route watchEffect может успеть — проверить и подавить);
захват payload сохранения в evidence; чипы «Количество видеоокон» сбрасывают
preset на вендорский — зафиксировать поведение в тесте.

## Отметки выполнения

- [x] T1 — commit `c36f973`: бейджи 59/59, live 0 ошибок. Ревью: r6-t1-review-elegance
  9/10 → после P1 (aria-label с числом, единый источник count/data-windows) 9.5/10.
  P1 применены, MANIFEST согласован.
- [ ] T2 — ЗАКРЫВАЕТСЯ после полного цикла ревью (5/10 `f3a006a` → 6/10 →
  7/10 `82d93b8`; все P0/P1/P2 закрыты в текущем WIP, ожидает финального
  ревью на коммите). Что исправлено по замечаниям ревью:
  - повторный клик чипа и reopen+чип: `reAdopt` больше не гейтится transient-
    флагом — сохранённая `vks_*` сразу даёт engaged=true и сторону из preset,
    `adoptPending` удалён, дедупликации нет;
  - чип 25/повторный чип: `constructorTarget` и мёртвый `onChipPointer` удалены;
    первичный выбор N — событийный one-shot (`pendingAdopt` в `reAdopt` на
    первом вендорском `preset_*`), без таймерной гонки; fallback 400 мс только
    снимает заморозку;
  - Codian/`type=default`: `reAdopt` реагирует только на `preset_*`,
    `select()` снимает engaged; имя/type восстанавливаются через
    `identityName`+`restoreIdentity` в отложенном adopt; `type` конструктора —
    `individual`;
  - клик «Конструктор» из manual/codian: немедленный `vks_<side>_<N>` (без
    вендорского сброса), сторона pin_r/l/t/b сохраняется;
  - `serverCount` инвалидируется после PATCH (обёртка fetch) — Конструктор
    после save+auto берёт актуальное N.
  Raw evidence (браузерный кеш отключён, реальные Playwright-клики):
  `t2-final-matrix-chips-pin-reopen.json` (auto/13→ctor13→чип9→повтор9→чип25→
  карточка pin_r→чип9→PATCH vks_pin_r_9→reopen→чип25=vks_pin_r_25),
  `t2-final-type-default-codian-autocycle.json` (type=default: имя/type целы;
  Codian после конструктора; auto→ctor берёт сохранённое N),
  `t2-final-reopen-tile-chip25.json` (reopen vks_grid_13→тайл→чип25→
  PATCH vks_grid_25→reopen), `t2-constructor-13.png` (13 окон, бейджи 13,
  счётчик 13). Ревью T2 на `4c36a6c`: 9/10, P0/P1 нет, P2: adopt без guard
  N>0/400-мс fallback, дублирование initialAdopt/pendingAdopt, после Codian
  нет active mode-тайла, невоспроизводимый шаг в evidence, T2 смешан с T13.
  Исправлено (T2.9): `initialAdopt` удалён (pendingAdopt — единственный
  источник), guard `startCount>0` в adoptNow, fallback 400 мс сам выбирает N
  и `adoptTarget` возвращает его на позднем вендорском сбросе; после ухода
  из конструктора вендорский тайл «Ручная настройка» снова active; save/auto
  берут актуальное N (баг разбора `video_layout.payload` в loadLayoutCount:
  читался внешний объект, serverCount всегда 0; плюс инвалидация после
  вендорского «Сохранить» — fetch/XHR/клик); evidence перегенерировано
  (save/reopen vks_grid_13 в matrix, c2 с кликом чипа, c4=vks_grid_9).
  MANIFEST 214/214. Повторное ревью T2.9 (`ebce17a`): **9.5/10, ПРИНИМАЮ**,
  P0/P1/P2 нет. T2 закрыта. По P3 ревьюера: мёртвая ветка `adoptTarget` удалена
  (T2.10) — поведение эквивалентно, подтверждено live probe2 (a2 vks_grid_13,
  b3 codian_07 + manual active, c4 vks_grid_9); evidence-маркеры кликов —
  в бэклоге T12 (P3, не блокер). T13.2 — на финальном ревью.
- [ ] T2 … T12

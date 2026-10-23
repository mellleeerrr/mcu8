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

## T7. Admin auth и контракт
Роль-гейт `GET /conferences/{id}` и `/ui/conferences/{id}` (Viewer без PIN),
списки с ролевым гейтом, `PATCH /profile` и `/sessions/{id}` (501→200/405 в
envelope), 409 дублей, mode-endpoint не перезаписывает preset/type/size.
Подводные камни: RBAC уже есть — переиспользовать; envelope `api-errors.js`;
не менять DTO; live-вызовы UI.
Критерии: тесты 401/403/409/200; Viewer PIN недоступен.

## T8. Шлюзы: abonent_id, статус, allowlist
Приоритет `abonent_id` над `contact_id` (fallback), реальный статус вместо
фиктивного (тесты `network_integration.rs:766-771` закрепляют фиктивность —
править их), H.323-allowlist из одного источника, source_latch для всех сторон.
Подводные камни: phantom window у SIP; не сломать inbound SIP; allowlist конфликт
(см. T6).
Критерии: тест «оба id → окно по abonent_id»; live-звонок без фантома.

## T9. PG-типы и CI postgres
`rooms.created_at TEXT` vs `Uuid` (миграция/биндинг), PG-тест не `#[ignore]`,
CI service postgres, `--locked`.
Подводные камни: sqlite-путь; необратимые миграции — сначала тест; Uuid decode.
Критерии: PG-тест зелёный в CI; sqlite зелёный.

## T10a. Proto-поля `layout_id/preset/mode` + LayoutModeChanged (ADR)
## T10b. id-only apply (без `payload.slots`) — не требует proto
## T10c. Прод-потребитель LayoutChanged (PNM WS / SDK) — после T10a/b

## T11. Evidence/golden
`codian_table.json:1227/1381/1591` → SUPERSEDED.md, фиксация 49 (0.4),
независимый fixture для неизменённых 43/46 по тексту владельца, golden для
`grids` (прецедент `codian_geometry_golden.rs`) — обязательный первый шаг перед
сокращением геометрии (r6-elegance).

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
Из T2 (P1): session-route тест «auto→Конструктор не дёргает POST
/api/v3/ui/video_layouts/mode» (в конференц-редакторе switchLayoutMode нет,
0 POST; на session-route watchEffect может успеть — проверить и подавить);
захват payload сохранения в evidence; чипы «Количество видеоокон» сбрасывают
preset на вендорский — зафиксировать поведение в тесте.

## Отметки выполнения

- [x] T1 — commit `c36f973`: бейджи 59/59, live 0 ошибок. Ревью: r6-t1-review-elegance
  9/10 → после P1 (aria-label с числом, единый источник count/data-windows) 9.5/10.
  P1 применены, MANIFEST согласован.
- [ ] T2 — ПЕРЕОТКРЫТА: независимый live на HEAD `aadaf7e` даёт 2 P0 (ревью
  r6-t2-review-bug, 6/10): (P0-1) клик по чипу «Количество видеоокон» при
  активном «Конструктор» ставит вендорский preset (`preset_equal_9`) — наш
  тайл остаётся активным, карточки без active, сохраняется не vks_*; (P0-2)
  fresh auto→«Конструктор»→Сохранить → PATCH `display_name:null`,
  `type:default` — имя раскладки стирается (в manual-ветке с vks_pin_r_25
  save/reopen проходит). План T2.2: (1) запоминать pin-сторону и
  re-adopt `vks_*_<N>` при смене счётчика/вендорского preset, один активный
  тайл; (2) при auto→manual устанавливать/сохранять `type=individual` и
  `displayName` (assert в payload); (3) raw fresh-auto + count-row evidence.
  Прежние коммиты: `472e444` + `a13fcf4`. Ревью r6-t2-review-elegance: 6/10 →
  9.5/10 после P0: slots-массив текущего preset (не map), отложенный adopt
  после mode='manual' без fallback count=1, selectConstructor = только preset
  (вендор синтезирует слоты из хвоста id), единственный активный тайл.
  Live: auto→«Конструктор» → activePreset=vks_grid_25, activeTiles=[constructor],
  переоткрытие сохраняет, 0 ошибок. Второе ревью r6-t2-review-short: 9/10 на
  committed-состоянии, P0 нет, save/reopen payload read-only подтверждён
  (manual + vks_grid_25 + 25 AUTO); P1-risk session-route POST mode → T12
  (в scope conference-layout воспроизведено 0 POST).
- [ ] T2 — ПОСЛЕ ревью r6-t2-review-elegance (6/10) переработана: убраны
  `constructorTarget` и мёртвый `onChipPointer`; первичный выбор — однократная
  отложенная установка после вендорского `preset_equal_25` (`initialAdopt`
  глушит reAdopt в этом окне), Codian-карточки больше не перехватываются
  (`reAdopt` только для `preset_*`, `select()` снимает engaged), сторона пина
  восстанавливается из сохранённого preset. Raw evidence:
  `t2-final-A-auto13-chip9.json` (auto/13→vks_grid_13→чип 9→PATCH vks_grid_9,
  reopen 9), `t2-final-B-chip25.json` (чип 25→vks_grid_25→PATCH/reopen 25),
  `t2-final-C-codian.json` (codian_07 после конструктора→PATCH codian_07,
  имя сохранено), `t2-final-D-pin-restore.json` (reopen vks_pin_r_9→сторона
  pin_r), скриншот `t2-constructor-13.png` (13 окон, бейджи 13). Осталось:
  повторное ревью ≥9.5 и commit.
- [ ] T2 … T12

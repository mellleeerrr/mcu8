# T1 — Регрессионный анализ фикса «Общей» раскладки (session-scope default)

Дата: 2026-10-28. Ветка WIP от `543bc7b`. Фикс затрагивает три слоя:
backend (`crates/vks-admin/src/api_v3/resources.rs`, `shell.rs`), project-слой UI
(`client/pnm-frontend/deployed/pnm/codian-layouts.js`) и вендорский бандл
(`scripts/patch-pnm-layout-crash.py` + `MANIFEST.sha256`).

## Инвариант

Для любого непустого `*:video_layouts` scope в session-ответе UI есть **ровно одна**
раскладка с `payload.type == "default"`; `payload.type` существующей раскладки
иммутабелен; при COW «Общая» клонируется с `type=default`. Тогда
`sessionLayoutListByType.default` всегда определён и `hasIndividualLayout()` /
`SessionBroadcastPopup.created()` не падают.

## Поведение до/после — матрица

| Сценарий | До | После | Почему не регресс |
|---|---|---|---|
| PATCH «Общей» из редактора (UI шлёт `type=individual` из-за project-слоя) | COW-клон `type=individual`; session-scope без default → крэш | project-слой больше не форсит тип; backend сохраняет `type=default`, клон остаётся «Общей» | Редактор и раньше ожидал default (вендор шлёт `type=default`); теперь оба источника согласованы |
| `POST /api/v3/ui/video_layouts/mode` c `payload.type=individual` | писал `type` из body | `type` игнорируется, `mode/preset/size/slots` применяются | mode-эндпоинт меняет только режим/схему; `type` не входит в его контракт (см. `shell.rs` комментарий) |
| Создание layout без `type` (`POST /api/v3/ui/video_layouts`) | canonical payload → «Общая» с чужим preset | `payload.type=individual`, кроме точного canonical seed (`{}`) | UI всегда шлёт type; seed-путь (создание «Общей» перед attach) сохранён и по-прежнему один на документ |
| `payload.type` вне `{default,individual}` | принимался как есть | 422 `payload.type must be "default" or "individual"` | невалидных клиентов не было (кроме тестов); контракт ужесточён осознанно |
| Сессия с `[canonical default, individual-клон]` (старый inherit + COW) | после рестарта 2 «Общих» или 0 | при загрузке alias-доказательство перетипирует клон в default; лишние default-ссылки отвязываются, активная остаётся | ремонт идемпотентен и детерминирован: не зависит от порядка рестартов (session-first/server-first — один результат) |
| Повторный старт сессии после правки «Общей» | inherit добавлял pristine canonical рядом с клоном | clone-aware inherit: id, представленный COW-клоном, не наследуется повторно | конференция сохраняет pristine canonical для других сессий; в сессии одна «Общая» |
| «Общая», отредактированная пользователем (имя «Codian 07 — 13 окон») | после ремонта осталась бы individual-only | alias-доказательство возвращает `type=default`; имя в БД сохраняется, UI скрывает его computed'ом для default-строк | выбран вариант A (retype по provenance, см. 01_crash_forensics.md §разногласия); engine применяет ту же геометрию, визуально ничего не меняется |
| Пустой scope (0 раскладок) | `repair_scope_defaults` его не трогает | так же; крэш закрыт JS-guard'ом | пустой scope = комната без раскладок (stale-ссылки вычищаются `links.retain`), создавать записи при загрузке нельзя — иначе мусор на каждую мёртвую комнату |
| `list_linked` (скрытие canonical конференции из session-ответа) | как было | как было | не трогали: это вендорская семантика «сессия показывает свои раскладки»; после фикса скрывать больше нечего |
| UI-сохранение конструктора | `editValueField('type','individual')` в `restoreIdentity`/`onVendorModeClick` | удалено; store `type` — отдельный ref, инициализируется из payload | вендорский store сам пишет `type` из загруженного payload; override ломал его |

## Проверки (фактические, 18:00–18:12 MSK)

- `cargo fmt --all -- --check` — ok.
- `cargo clippy -p vks-admin --locked --all-targets -- -D warnings` — ok.
- `cargo test -p vks-admin --lib` — **304 passed, 0 failed** (в т.ч. 7 `t00_*`):
  `t00_layout_type_is_immutable_and_session_keeps_default_after_edit`,
  `t00_normalize_loaded_restores_default_from_alias_evidence`,
  `t00_normalize_loaded_dedupes_two_defaults_keeping_active`,
  `t00_normalize_loaded_prefers_alias_clone_over_pristine_default`,
  `t00_session_restart_does_not_add_second_default_after_edit`,
  `t00_layout_type_is_validated_on_create_and_repaired_on_load`,
  `t00_create_without_type_is_individual_except_canonical_seed`.
- `cargo build --release --locked -p vks-server` — ok; сервис перезапущен
  (PID 1814143, старт 18:00:39), в логе ровно один ремонт:
  `WARN resources: restored layout type=default from COW alias evidence
  scope=sessions:49b599d7…` — ожидаемо (второй битый scope `cac8ed13…` уже не существует).
- Живой API после рестарта:
  - `GET /api/v3/ui/sessions/49b599d7…/video_layouts` → `[2caea6d1:default]`
    (было `[2caea6d1:individual]`, count=1);
  - `GET /api/v3/ui/sessions/1e0337c6…/video_layouts` → `[29c4da67:default]`
    (было два default: `29c4da67`, `8a75f3e4`);
  - `PATCH …/video_layouts/<default>` c `{payload:{type:individual,mode:manual,preset:vks_pin_l_13}}`
    → 200, ответ `type=default`, scope остаётся `1 × default` (COW-клон default);
  - `POST /api/v3/ui/video_layouts/mode` c `payload.type=individual` → 200, `type=default`;
  - `POST /api/v3/ui/video_layouts` без type → 201 `individual`; с `payload:{}` → 201 `default`;
  - `payload.type=foo` → 422.
- Playwright (см. 04_e2e_repro.md): `--expect-clean` — 0 крэшей, immutable=true;
  `--expect-clean --guard-only` — 0 крэшей при «битом» списке; `--guard-only --legacy-bundle`
  — исходный `TypeError … reading 'some'` воспроизведён (тест чувствителен).
- `sha256sum -c MANIFEST.sha256` после патча бандла — 214/214 ok (патч-скрипт регенерирует
  манифест; повторный прогон `--check` — «БЕЗ ИЗМЕНЕНИЙ»).

## Остаточные риски / follow-up (не блокируют T1)

1. **Fork-on-type-change (P1).** Смена типа существующей раскладки через старый кэш
   (PATCH c `type=individual` по default) больше не портит данные, но и не создаёт
   отдельную individual-раскладку: пользователь, осознанно менявший тип, ожидал форк.
   Решение: `PATCH` default + `type=individual` → создать новый individual-клон и оставить
   «Общую» (≈8 строк) — отдельная задача после T1.
2. **Имя отредактированной «Общей».** При retype имя «Codian 07 — 13 окон» остаётся в
   `display_name`, но UI для default-строк его не показывает (вендорский computed). Это
   следствие варианта A; при желании — отдельное поле/бейдж, не критично.
3. **Пустой scope.** Ремонт не создаёт раскладку; крэш закрыт JS-guard'ом. Если появится
   write-путь, создающий пустой session-scope, нужен серверный инвариант «непустой при
   старте сессии» (сейчас `inherit_and_ensure_default_layout` это делает).
4. **`previews` 501.** `POST /sessions/{id}/previews/{pid}` — реальная вендорская фича,
   отвечает 501 (P2, не причина крэша).
5. **Firefox.** Отдельного прогона нет; guard и код одинаковы, риск низкий (см. 04).

# T10 — независимое ревью B: directory/addresses в SQL

- **Дата:** 2026-10-29, 14:35–15:40 MSK.
- **Ревьюер:** независимый adversarial-reviewer (агент); живой репозиторий только читался, изменён только этот файл. Production-файлы T10 в полигоне побайтно совпадают с `3e2983c` (проверено `sha256sum` для `directory_migrate.rs`, `core_adapter.rs`, `ports/storage.rs`, `0007_directory_normalized.sql`).
- **Объект:** HEAD `3e2983c` «fix(T04b): derive production RoomCaps from the codec SSOT». Реализация T10b — коммит `451fd9f` «T10b: directory/addresses в SQL с schema_state и идемпотентной миграцией»; после него T10-файлы не менялись, а `crates/vks-server/src/main.rs` менялся только в T04b-части (блок `directory_migrate::run` и `sync_from_storage` не затронут — `git diff 451fd9f 3e2983c -- crates/vks-server/src/main.rs`).
- **Полигон:** `git archive 3e2983c` → `/dev/shm/t10-reviewB` (без `.git`); `CARGO_TARGET_DIR=/home/pro/.cache/t10_reviewB_target`; Rust **1.94.0** (`rust-toolchain.toml`), `--offline --locked`. Временные probe-тесты жили только в копии и удалены; в копии нет ничего, кроме распакованного архива.
- **Pitfalls:** [`docs/audit/2026-10-29/pitfalls/T10.md`](../pitfalls/T10.md). Acceptance — [`MASTER_TASK_LIST_2026-10-29.md:49`](../../../progress/MASTER_TASK_LIST_2026-10-29.md#L49).

## Вердикт

**Score: 9.0/10. Done: yes.** Собственных P0/P1 у T10 не найдено. Все заявленные acceptance-сценарии, которые можно проверить без живого PostgreSQL, проверены независимо: API → SQL → рестарт → hot reload → edit/delete, сохранение личной копии при удалении глобального адреса, атомарный откат миграции при сбое записи snapshot, CAS-ретраи, adopt при потерянном маркере, `reset`, граничные owner/id. PG-путь локально не исполнялся (в окружении нет сервера PostgreSQL), но DDL и код паритетны, а CI-джоб `test-postgres` реально запускает parity-тест с `VKS_STORAGE_PG_URL`.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **4** |
| **P3** | **11** |

Главное:

- **P2-1 (forward-compat, data loss):** неизвестные поля адреса сохраняются миграцией в `directory_addresses.address_json`, но **теряются после первой гидратации RAM** и затираются в SQL следующей мутацией: `apply_snapshot` парсит `address_json` в `AddressRecord` без `flatten` (`directory_migrate.rs:403-409`, `resources.rs:122-129`), а `snapshot_from_resources` сериализует только известные поля (`directory_migrate.rs:347-357`, `:421-423`). Probe: `future_address` есть в SQL сразу после миграции и исчезает после `apply_snapshot` + `snapshot_from_resources`. Профили/группы/роли свои unknown-поля сохраняют (`flatten`), адреса — нет.
- **P2-2 (acceptance evidence):** parity-тест PG **не проверяет ветку импорта**: он предварительно заполняет оба бэкенда через `replace_directory_snapshot(&fixture())`, поэтому `migrate` идёт по ветке adopt (`tests/directory_migration.rs:245-256`, комментарий `:252` «rows already exist -> adopt»). Ветка `existing == 0` (`core_adapter.rs:769-770` SQLite; `:1397-1398` PG) на PG не исполняется. CI покроет adopt-parity, но не import-parity.
- **P2-3 (data loss, config-gated):** при `storage_allow_fallback = true` ошибка `sync_from_storage` только логируется (`main.rs:190-196`); если документ повреждён, sync падает до загрузки snapshot (`state.rs:715`), RAM остаётся пустой, и **следующая обычная мутация** через `commit` → `persist_with_directory` (`resources.rs:313-322`, `:269-291`) пишет пустой snapshot, стирая `directory_profiles`/`directory_addresses`. Probe воспроизвёл: после `POST /groups` профили и глобальные адреса исчезли. Дефолт/prod — `false`, но dev-escape-hatch документирован (`deploy/README.md:141`).
- **P2-4 (multi-writer, PG-кластер):** каждый `commit` заменяет весь snapshot (`resources.rs:289` → `core_adapter.rs:73-88`/`:231-245`); второй инстанс со stale-кэшем удаляет строки первого. Single-node SQLite под `DbLock` не затронут; для PG-кластера/второго writer — data loss (P2).

---

## §1. Pristine-сборка и гейты

| Гейт | Команда | Результат |
|---|---|---|
| fmt | `cargo fmt --all -- --check` | **0** (exit 0) |
| clippy | `cargo clippy -p vks-core -p vks-storage -p vks-admin -p vks-server --all-features --all-targets --offline --locked -- -D warnings` | **0** (exit 0, 1m14s) |
| vks-storage | `cargo test -p vks-storage --offline --locked --no-fail-fast` | **32 passed** (24 lib + 1 dedup_lock + 7 directory_migration) |
| vks-admin | `cargo test -p vks-admin --lib --offline --locked` | **368 passed, 0 failed** (37.19s) |
| vks-core | `cargo test -p vks-core --offline --locked` | **88 passed** (lib) + 0 doc |
| vks-server | `cargo test -p vks-server --lib --offline --locked` | **150 passed, 3 ignored, 0 failed** (20.09s) |

CI-матрица (`cargo test --workspace --all-features --locked --no-fail-fast`, `test-postgres`, `test-serial`) на этом хосте целиком не воспроизводилась: другие агенты уже занимали cargo-lock, а полный workspace-прогон не является предметом T10. Затронутые крейты и их тесты прогнаны; clippy — all-features/all-targets для этих крейтов, как в CI (`.github/workflows/ci.yml:71`).

**PostgreSQL локально недоступен:** нет `pg_isready`, нет слушателя на `:5432`, нет docker/podman; поднимать чужой сервер и трогать занятые порты (`:8080`, `:127.0.0.1:18082`) запрещено условиями. Поэтому PG-путь проверен статически (общий SQL-хелпер, переносимый DDL, один и тот же `DirectorySnapshot`, отсутствие JSONB/FK) и по CI-конфигурации; parity-тест без `VKS_STORAGE_PG_URL` печатает `SKIP sqlite_and_pg_load_same_directory_snapshot` и **не является доказательством** PG-паритета. Это учтено в P2-2 и в оценке.

---

## §2. Что атаковано и что получилось

| Атака | Итог | Доказательство |
|---|---|---|
| API create user/group/role/address → SQL | ✅ | probe `probe_api_sql_restart_hot_reload_edit_delete`: строки в `directory_*`/`directory_addresses`, API читает то же |
| Рестарт с SQL (новый `AppState`) | ✅ | тот же probe: профиль, группа, membership, адрес на месте |
| Hot reload | ✅ | тот же probe + `hot_reload_preserves_directory_and_addresses` |
| Edit глобального/личного адреса | ✅ | probe: `PATCH /call_strings/{id}` и `PATCH /ui/users/{id}/call_strings/{id}` → SQL `display_name`/`payload_json`/`address_json` обновлены |
| Delete глобального адреса при наличии личной копии | ✅ | probe + `personal_address_copy_survives_global_delete`: PK `(owner_id,id)` не конфликтует, копия остаётся |
| Повторная миграция (маркер есть) | ✅ | `stale_document_does_not_override_sql_rows`, probe: `already_migrated`, строки не возвращаются |
| Потеря маркера при непустых таблицах | ✅ adopt | probe `probe_lost_marker_no_resurrection_and_partial_windows`: удалённый контакт не воскресает, документ чистится в той же транзакции |
| Потеря маркера + **все таблицы пусты** | ⚠️ D5 | probe `probe_lost_marker_with_all_tables_empty_resurrects`: 1 профиль/3 адреса вернулись; это задокументированный предел, но в репозитории он не описан (P3-6) |
| Сбой записи snapshot в середине | ✅ rollback | probe `probe_failed_snapshot_write_rolls_back_document_rows_and_ram`: trigger `RAISE(ABORT)` → откат marker+rows+document, RAM не тронут. |
| CAS при конкурентном писателе | ✅/⚠️ | probe: 3 попытки, затем ошибка; SQL-бэкенды откатывают; Memory оставляет строки (P3-1); при `new_document=None` CAS вообще не выполняется (P3-9) |
| Битый JSON документа | ✅ fail-fast | `invalid_document_aborts_without_marker_or_rows`; main аварийно завершается до bind |
| Битые отдельные записи | ✅ tolerant | `broken_legacy_entries_do_not_abort_migration`; плохие записи пропускаются, хорошие импортируются |
| Дубликаты canonical (global) | ✅ | `duplicate_canonical_global_addresses_merge_and_rewrite_refs`, probe: дубликат слит, `primary_address` переписан |
| Одинаковый canonical global vs personal | ✅ | probe `probe_edge_owner_ids_and_duplicate_canonical_global_vs_personal`: личная копия не конфликтует с глобальной |
| owner `""` / `" "` / несуществующий user, id `""` | ✅ сохраняются | probe: строки не теряются, API их не маскирует |
| Unknown-поля профиля/группы/роли | ✅ | `migration_strips_legacy_keys_preserves_unknown_fields_and_is_idempotent` |
| Unknown-поля адреса | ❌ P2-1 | probe `probe_memory_document_changed_leaves_rows_and_address_extras_are_dropped` |
| Вложенные unknown-поля `resources.directory` | ❌ P3-4 | probe `probe_nested_directory_extras_are_unmapped`: `future_directory` исчезает вместе с поддеревом |
| Все admin persist-пути | ✅ | `commit` → `persist_with_directory`; единственный не-directory путь `persist_with_video_settings` пишет settings/video, не directory (`resources.rs:263-291`); `put_configuration_document` без snapshot вызывает только он |
| `Storage::reset` | ✅ | Memory/SQLite/PG чистят 6 таблиц + `schema_state` (`core_adapter.rs:1297-1323`, `:1924-1950`); `reset_allows_migration_again` |
| Кросс-крейтовые дефолты trait | ⚠️ P3-2 | `load_directory_snapshot` по умолчанию отдаёт пустой snapshot (`storage.rs:421-423`), `MockStorage` его не переопределяет (`rooms.rs:398`) |
| SQLite/PG DDL | ✅ статически | TEXT/BIGINT, PK `(owner_id,id)`, partial unique `ux_dir_addresses_global_canonical`, без FK/JSONB; одинаковые запросы в общем коде |
| PG parity/CI | ⚠️ P2-2 | parity-тест есть, CI-джоб с `VKS_STORAGE_PG_URL` есть; import-ветка не покрыта; локально PG нет |
| Два инстанса на одной БД | ⚠️ P2-4 | `resources.rs:289` snapshot из RAM + `DELETE`/`INSERT` всего snapshot (`core_adapter.rs:73-88`, PG `:231-245`); последний writer затирает строки другого, CAS/версии нет |
| Personal dedup + `primary_address` | ❌ P3-11 | `dedup_personal_book` удаляет duplicate, но не переписывает `users[].primary_address` (`directory_migrate.rs:253-290`) |
| Документация/эксплуатация | ❌ P3-7 | в T10b-коммите нет docs; `deploy/README.md` не упоминает `schema_state`/T10/порядок старый-новый бинарь |

---

## §3. P2 — найдено

### P2-1. Unknown-поля адреса теряются после RAM-гидратации и затираются в SQL

**Где:** `crates/vks-admin/src/api_v3/directory_migrate.rs:403-409` (парсинг `address_json` в `AddressRecord`), `crates/vks-admin/src/api_v3/resources.rs:122-129` (`AddressRecord` без `#[serde(flatten)] extra`), `crates/vks-admin/src/api_v3/directory_migrate.rs:347-357` (`stored_address` сериализует `AddressRecord`), `:421-423` (`snapshot_from_resources`), `crates/vks-admin/src/state.rs:741` (`apply_snapshot` при старте/hot reload), `crates/vks-admin/src/api_v3/resources.rs:269-291` (`persist_with_directory` на каждой мутации).

**Механика:** миграция кладёт в `address_json` исходный raw-объект (overlay, `directory_migrate.rs:137-160`), поэтому `future_address` виден в SQL сразу после миграции. Но `apply_snapshot` парсит его в `AddressRecord`, у которого нет catch-all, и следующий `snapshot_from_resources` сериализует только `id/uri/display_name/created_at/payload`. Любая мутация после рестарта (даже не связанная с адресом) вызывает `commit` → `persist_with_directory` → перезапись `address_json` без `future_address`.

**Probe:** `probe_memory_document_changed_leaves_rows_and_address_extras_are_dropped`: после миграции `future_address` есть; после `apply_snapshot` + `snapshot_from_resources` — нет. Это соответствует обещанию T10b «unknown-поля адресов сохраняются» только на шаге импорта, но не после первого рестарта.

**Влияние:** forward-compat данных адресной книги; известные поля не теряются. Не P1, потому что acceptance T10 этого не требует, а P2-5 pitfalls — требует.

### P2-2. PG parity-тест проверяет adopt, а не импорт; локального PG нет

**Где:** `crates/vks-storage/tests/directory_migration.rs:245-256` — оба бэкенда сначала получают `replace_directory_snapshot(&fixture())`, затем документ; `migrate` видит непустые строки и делает adopt (комментарий `:252`). Ветка импорта `existing == 0` — `crates/vks-storage/src/core_adapter.rs:769-770` (SQLite) и `:1397-1398` (PG) — на PG не исполняется (документ остаётся legacy, но adopt его не парсит).

**CI:** `.github/workflows/ci.yml:131-163` поднимает `postgres:16`, экспортирует `VKS_STORAGE_PG_URL: postgres://postgres:postgres@localhost:5432/vks` и запускает `cargo test -p vks-storage --locked --no-fail-fast`; при наличии env `tests/directory_migration.rs:230` не пропускает тест. Значит, adopt-parity в CI будет, import-parity — нет.

**Почему P2, а не P1:** DDL и запросы переносимы, общий код; критерий «sqlite==pg» формально подтверждён на уровне кода и CI-конфигурации, но не на живом PG. Живой PG в этом окружении недоступен; условие задачи прямо разрешает зафиксировать skip с обоснованием.

### P2-3. `storage_allow_fallback=true` + сбой sync → следующая мутация стирает directory

**Где:** `crates/vks-server/src/main.rs:190-196` (ошибка `sync_from_storage` не фатальна при fallback), `crates/vks-admin/src/state.rs:715` (парсинг документа падает до `load_directory_snapshot`), `crates/vks-admin/src/api_v3/resources.rs:313-322` (`commit` → `persist_with_directory` при любой мутации), `:269-291` (snapshot из RAM), `crates/vks-admin/src/api_v3/directory_migrate.rs:421-423` (пустой RAM → пустой snapshot).

**Probe:** `probe_tolerated_sync_failure_then_mutation_wipes_sql_directory`: SQLite с профилем и глобальным адресом; документ заменён на `{not json`; новый `AppState::sync_from_storage()` → `Err`; затем `POST /api/v3/groups` (обычная мутация) → `directory_profiles` пуст, глобальный `directory_addresses` пуст, группа записана. То есть сервер, переживший сбой sync по fallback-политике, при первой же записи уничтожает directory в SQL.

**Влияние:** data loss. Дефолт `storage_allow_fallback=false` (`configs/default.toml:37`, `configs/prod.example.toml:50`), поэтому prod не затронут при штатной конфигурации; dev-режим документирован (`deploy/README.md:141`). P2, а не P1.

### P2-4. Два процесса на одной БД: whole-snapshot replace затирает чужие строки

**Где:** `crates/vks-admin/src/api_v3/resources.rs:285-294` — snapshot строится из **локального RAM-кэша**; `crates/vks-storage/src/core_adapter.rs:73-88` (SQLite) и `:231-245` (PG) — `DELETE` всех 6 таблиц + `INSERT`; `put_configuration_document_with_directory` — `:724-737` (SQLite) / `:1355-1368` (PG). Ни per-row upsert, ни версии снапшота, ни advisory-lock нет.

**Механика:** каждый `commit` заменяет весь directory-snapshot. Два `AppState` на одной БД (второй PG-узел, второй инстанс, старый+новый бинарь) с разными кэшами: второй writer удаляет строки, созданные первым, и записывает только своё состояние. Локальный кэш второго узла не видит URI первого, поэтому API может вернуть 201 на уже существующий адрес, а в SQL останется только строка второго.

**Влияние:** data loss в multi-writer PG-кластере. Для одиночного SQLite под `DbLock` недостижимо; acceptance T10 (single-node) не ломается. P2, не P1: штатная конфигурация проекта — один writer.

**Рекомендация:** либо явно задокументировать «один writer на БД, PG-кластер с T10 не поддерживается», либо перейти на per-row upsert/delete по id + версию снапшота/`pg_advisory_lock`.

---

## §4. P3 — найдено

1. **MemoryStorage: `DocumentChanged` — не no-op.** `crates/vks-storage/src/core_adapter.rs:465-476`: snapshot присваивается до CAS; при несовпадении документа возвращается `DocumentChanged`, но строки уже записаны, marker — нет. Контракт `DirectoryMigrationOutcome::DocumentChanged` прямо говорит «Nothing was written» (`crates/vks-core/src/ports/storage.rs:260-262`). SQLite/PG откатывают транзакцию. Reachability в prod низкая (MemoryStorage — in-process fallback, миграция до обслуживания), но контракт нарушен; probe подтверждает.
2. **Trait default `load_directory_snapshot` — тихий пустой snapshot.** `crates/vks-core/src/ports/storage.rs:421-423`; write-дефолты при этом возвращают `Err` (`:427-461`). Любой новый бэкенд/тест-дабл, забывший override, молча теряет directory при старте; `MockStorage` в `crates/vks-admin/src/rooms.rs:398` его не переопределяет. Probe `probe_cas_retry_is_bounded_to_three_and_defaults_are_asymmetric`.
3. **`MigrationReport.imported` всегда `true`, если в документе были legacy-ключи.** Doc-комментарий обещает `false` при adopt (`directory_migrate.rs:43-45`), но `:129` безусловно ставит `true`, а `:455`/`:460` возвращают его и для adopt, и для `already_migrated`. Логи вводят в заблуждение при потерянном маркере.
4. **Вложенные unknown-поля `resources.directory` исчезают.** `directory_migrate.rs:127-128` удаляет всё поддерево; SQL-дома для них нет. Probe `probe_nested_directory_extras_are_unmapped`: `future_directory` пропал. Это ограничение дизайна, а не регрессия known-полей.
5. **Маркер есть + старые legacy-ключи в JSON → ключи остаются навсегда.** `core_adapter.rs:761-766` (SQLite) возвращает `AlreadyMigrated` до любого strip; probe `probe_lost_marker_no_resurrection_and_partial_windows`: после восстановления старого документа третий запуск — `already_migrated`, `resources.addresses` в JSON остаётся. Это сознательный выбор P0-2 (не воскрешать), но acceptance «поле убрано из JSON» выполняется только для документа, который был чист на первом успешном запуске.
6. **Предел D5 (все таблицы пусты + потерян маркер) не задокументирован в репозитории.** Probe `probe_lost_marker_with_all_tables_empty_resurrects`: после удаления всех 6 таблиц и маркера + восстановления старого документа повторная миграция вернула 1 профиль и 3 адреса. Pitfalls T10 §D5 требует записать это в отчёт/документацию; в `docs/`/`deploy/` упоминания нет. Как ограничение — P3, не баг.
7. **Нет эксплуатационной документации T10.** В `451fd9f` нет docs; `deploy/README.md` не описывает `schema_state`, порядок «сначала новый бинарь, потом старый», откат и поведение старого бинаря на новой БД. Откат на старый бинарь после strip документа даст пустой справочник (данные лежат в SQL, старый код их не читает) — операционный риск, P3.
8. **`apply_snapshot` не очищает `resources.addresses`.** `directory_migrate.rs:402-416`: обновляет `directory`, но глобальные адреса только вставляет; текущие вызовы (`state.rs:731-741`) передают пустой кэш после `clear()`/`Default`, поэтому сегодня безопасно; будущий вызов на непустом кэше оставит stale-адреса.
9. **CAS пропускается, когда `new_document=None`.** `directory_migrate.rs:449-452` передаёт `None`, если в прочитанном документе не было legacy-ключей; SQL (`core_adapter.rs:773`) и Memory (`:470`) в этом случае ставят маркер, **не сверяя** документ. Если между чтением и транзакцией старый/конкурентный писатель добавит `resources.directory`, данные останутся в JSON, marker будет выставлен, и новый код их проигнорирует навсегда. Узкая гонка со старым бинарём/мультиинстансом; P3.
10. **Дедуп не переписывает ссылки комнат и теряет extras слитых личных копий.** `directory_migrate.rs:229-252` переписывает только личные книги и `primary_address`; `resources.contacts` (id контактов комнат) могут остаться указывать на удалённый duplicate-id. Плюс `overlay_raw_address_json` (`:146`) ищет raw по `(owner_id,id)` после переименования duplicate→keep (`:242-245`), поэтому unknown-поля слитой личной копии не переносятся. Первое — pre-existing поведение, второе — следствие dedup.

11. **Персональный canonical-дедуп не переписывает `primary_address`.** `directory_migrate.rs:253-257` вызывает `dedup_personal_book` без возврата aliases; `:261-290` удаляет duplicate personal-копию, но `users[].primary_address` может продолжать ссылаться на удалённый id. API переживёт (ссылка станет null), но «звёздочка»/primary теряется. Глобальные aliases при этом переписываются (`:247-251`), а персональные — нет.

---

## §5. Остаточные ограничения (не баги)

- **D5:** «свежая БД» и «потерянный маркер при пустых таблицах» неразличимы; старый JSON может воскресить удалённое. Требуется backup/осознанное решение, в коде это не лечится.
- **Marker already present:** старые ключи в документе не вычищаются (P3-5); данные в SQL авторитетны, JSON игнорируется.
- **Старый бинарь на новой БД:** читает только JSON; после T10-миграции справочник для него пуст. Mixed-version deploy не поддерживается; документации нет (P3-7).
- **`updated_at` в `directory_profiles/groups/roles`** фактически равен `created_at` (`snapshot_from_parts`, `directory_migrate.rs:300-330`); семантика колонки «last write» не соблюдается. На T10 не влияет, но при будущих инкрементальных синхронизациях может ввести в заблуждение.
- **Unknown-поля верхнего уровня документа** (`future_top` вне `resources`) сохраняются миграцией, но `configuration_document` (`resources.rs:298-310`) заново собирает JSON только из `version/resources/conference_settings/video_settings`; первая же мутация их теряет. Это pre-existing поведение, не регрессия T10, но forward-compat-обещание ограничено моментом миграции.
- **Битые `profile_json`/`group_json`/`role_json`** при `apply_snapshot` пропускаются с warning, а следующая мутация записывает snapshot без них (тихое удаление повреждённой строки). Для адресов есть fallback на колонки; для профилей/групп/ролей — нет. Редкий сценарий ручной порчи/несовместимой схемы.

---

## §6. Чистота

- Живая БД `/home/pro/Рабочий стол/mcu/vks_data.db`: sha256 `687b758f6e3c6dd31021109c05fa626f19dfb3f2e1fd6c69d3f3f1c90caa81ce`, mtime `2026-10-29 12:38:31`, размер 3248128 — **не менялась** (сверено до и после). `vks_data.db.lock` (0 байт, 2026-10-23) не трогался.
- Занятые порты `:8080` и `:127.0.0.1:18082` не открывались; никакие живые процессы не запускались/не останавливались.
- `backups/`, живой `target/`, чужие `docs/audit/.../reviews/*` не изменялись. В репозитории появился только этот файл.
- Все probe-БД, SQLite-файлы, Python-хелперы и probe-тесты — в `/dev/shm`/в копии; после проверки probe-файл удалён, `directory_migrate.rs` восстановлен побайтно (`sha256` совпал с `3e2983c`). Копия и target удалены после прогона.
- В рабочем дереве репозитория уже были чужие изменения (`.github/workflows/ci.yml`, `deploy/README.md`, `docs/...`, `tests/e2e/webrtc.js`, untracked `scripts/check-pnm-webrtc-drift.py` и др.) — они не мои и не трогались.

---

## §7. Почему 9.0, а не выше

- **0 P0/P1**, acceptance по существу выполнен: SQL — источник истины, миграция идемпотентна и атомарна, adopt не воскрешает при непустых таблицах, API/рестарт/hot reload/edit/delete и личные копии проверены.
- **9.5+** требует «real evidence» по всем пунктам, включая живой PG. Здесь PG локально нет; parity-тест к тому же покрывает только adopt (P2-2). Это не позволяет поставить 9.5.
- **9.0** (а не 8.5): data-loss P2 — forward-compat unknown-полей (P2-1), config-gated fallback (P2-3) и multi-writer PG-кластер (P2-4); ядро T10 и его acceptance они не ломают при штатной single-writer конфигурации. P3 — контрактные/документационные/краевые, не блокирующие.
- Если Lead готов принять PG-паритет только по CI и считает unknown-поля адресов вне acceptance, оценка может быть повышена до 9.2–9.3; evidence для 9.5+ всё равно нет.

---

## §8. Проверенные файлы/тесты (основное)

- `crates/vks-storage/migrations/0007_directory_normalized.sql`
- `crates/vks-core/src/ports/storage.rs` (`DirectorySnapshot`, `DirectoryMigrationOutcome`, дефолты)
- `crates/vks-storage/src/core_adapter.rs` (Memory/SQLite/PG, `reset`)
- `crates/vks-admin/src/api_v3/directory_migrate.rs` (parse/dedup/snapshot/apply/run)
- `crates/vks-admin/src/api_v3/directory_migrate_tests.rs` (9 тестов)
- `crates/vks-admin/src/api_v3/resources.rs` (`#[serde(skip)]`, `persist*`, `commit`)
- `crates/vks-admin/src/state.rs` (`sync_from_storage_inner`, hot reload)
- `crates/vks-server/src/main.rs` (порядок storage init → migration → sync → bind)
- `crates/vks-storage/tests/directory_migration.rs` (7 тестов, PG parity)
- `crates/vks-storage/src/dedup.rs` (personal books, `--apply`)
- `.github/workflows/ci.yml` (`test-postgres`, clippy all-features)

Временные probe-тесты (удалены после прогона; 9/9 passed):
`probe_api_sql_restart_hot_reload_edit_delete`, `probe_lost_marker_no_resurrection_and_partial_windows`, `probe_failed_snapshot_write_rolls_back_document_rows_and_ram`, `probe_cas_retry_is_bounded_to_three_and_defaults_are_asymmetric`, `probe_memory_document_changed_leaves_rows_and_address_extras_are_dropped`, `probe_edge_owner_ids_and_duplicate_canonical_global_vs_personal`, `probe_nested_directory_extras_are_unmapped`, `probe_tolerated_sync_failure_then_mutation_wipes_sql_directory`, `probe_lost_marker_with_all_tables_empty_resurrects`.

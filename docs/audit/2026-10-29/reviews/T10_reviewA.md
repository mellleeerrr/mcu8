# Ревью T10 (T10b): directory/addresses в SQL + `schema_state` + идемпотентная миграция

- **Предмет:** дерево commit `3e2983c` (T10b-содержимое — `451fd9f`, «T10b: directory/addresses в SQL с schema_state и идемпотентной миграцией»; между `451fd9f` и `3e2983c` T10-файлы не менялись, но `main.rs` получил T04b-правки, порядок T10-вызовов сохранён).
- **Метод:** чистый `git archive 3e2983c` в `/dev/shm/t10rA.G0Czv4rT` (не `/tmp`), Rust 1.94.0 из `rust-toolchain.toml`, `CARGO_TARGET_DIR=/dev/shm/t10rA-target`, все прогоны `--locked --offline`. Живая БД `vks_data.db`, live-порты, `backups/` не открывались и не менялись. PG-прогон **пропущен**: `VKS_STORAGE_PG_URL` не задан, локального PostgreSQL/`psql`/`docker` нет — PG проверен только чтением кода (SQL-хелперы идентичны SQLite) и env-gated тестом, который в этом окружении печатает `SKIP`.
- **Итог:** **done: yes**, **9.0/10**. **P0 = 0, P1 = 0, P2 = 3, P3 = 4.** Формальные критерии приёмки на выполненном (SQLite/Memory) пути закрыты; P2-2 и P2-3 нужно зафиксировать/исправить до объявления T10 «долгосрочно завершённой».

---

## 1. Приёмка по пунктам

| # | Критерий | Вердикт | Чем подтверждено |
|---|---|---|---|
| 1 | Миграция: legacy-документ → SQL, идемпотентность, маркер, no-resurrection, unknown-поля, очистка JSON, PK/owner_id, global unique | **Выполнено** (с P2-2 после первой мутации) | Прогоны `probe_run_end_to_end_acceptance_sqlite`, `sqlite_non_empty_rows_adopted_on_first_migration`, `sqlite_marker_present_never_rewrites_document_or_rows`, `sqlite_lost_marker_with_empty_tables_resurrects_documented_limit`; реальные старты сервера на `/dev/shm/t10-ra-server-good.db` |
| 2 | Атомарность: строки + JSON strip + маркер в одной транзакции; CAS → rollback + retry | **Выполнено для SQLite/PG-кода; Memory — P2-1** | `sqlite_mid_transaction_constraint_failure_rolls_back_marker_rows_and_document`, `sqlite_cas_conflict_rolls_back_and_retry_applies`, `sqlite_put_document_with_directory_is_atomic_on_mid_tx_failure`; `memory_cas_conflict_leaves_snapshot_partially_written` (баг) |
| 3 | Durability: real admin API → restart/hot reload → то же состояние; GET; personal copy после delete global | **Выполнено** | `directory_crud_survives_sqlite_restart`, `hot_reload_preserves_directory_and_addresses`, `personal_address_copy_survives_global_delete`, `address_book_crud_filters_and_persists`, `probe_delete_user_keeps_global_and_other_personal_rows`; плюс полный admin-lib 368/368 |
| 4 | Порядок startup: миграция после `DbLock`/init, до `sync_from_storage`, fail-fast, fallback не маскирует | **Выполнено** | Код `crates/vks-server/src/main.rs:177-194`; реальный бинарь на испорченном JSON: exit **78** и `configuration rejected` **при `VKS_STORAGE_ALLOW_FALLBACK=true`**; bad-DB остался без маркера и строк |
| 5 | Reset/Mock/Memory/PG parity | **Выполнено** (PG — только inspection) | `memory_snapshot_roundtrip_marker_and_reset`, `sqlite_reset_clears_directory_rows_and_marker`, `reset_allows_migration_again`; PG SQL построчно сверен, parity-тест skipped |
| 6 | Гейты: fmt, clippy, тесты, отсутствие T10-флейков | **Выполнено** | fmt — 0 diff на pristine; clippy 4 крейтов — 0 warnings; storage 32/32; admin 368/368; server 150 passed + 3 ignored; workspace 1557 passed, 1 unrelated media-flake (изолированно passed) |
| 7 | Гигиена | **Выполнено** | Только `git archive`/`git show` (read-only) и scratch в `/dev/shm`; в репозитории создан только этот файл |

---

## 2. Ключевые исполненные проверки

### 2.1. End-to-end миграция через настоящий `run(&AppState)` на SQLite

Скрэтч-тест `probe_run_end_to_end_acceptance_sqlite` (в scratch-дереве, в коммит не входит):

- legacy-документ с `future_top`, `resources.future_res`, двумя глобальными адресами с одним canonical URI, personal-копией, built-in ролью Administrator, `future_profile`/`future_group`/`future_role`/`future_address`/`future_personal`;
- `report = applied`, `profiles=1 groups=1 roles=1 global_addresses=1 personal_addresses=2`;
- в JSON не осталось `resources.directory` и `resources.addresses`, `future_top`/`future_res` сохранены;
- в `directory_addresses`: один глобальный `a-old` (earliest `created_at`), personal-копия `a-old` жива отдельно, `primary_address` переписан на `a-old`, payload’ы слиты, `future_address`/`future_personal` сохранены;
- `schema_state('directory')=1`; повторный `run` — `already_migrated`, snapshot не изменился;
- новый `AppState::sync_from_storage()` на той же БД поднял RAM-кэш из SQL: пользователь, глобальный адрес, 2 personal-копии на месте.

Проверено и DDL: PK `(owner_id, id)`, partial unique index `ux_dir_addresses_global_canonical ... WHERE owner_id = ''`, `PRAGMA foreign_key_list(directory_addresses)` пуст. Дубликат canonical через API: POST идемпотентен (201, тот же id), PATCH на занятый canonical — **409**, не 500 (`probe_duplicate_canonical_post_idempotent_patch_409`).

### 2.2. Атомарность и retry (SQLite)

- `sqlite_mid_transaction_constraint_failure_rolls_back_marker_rows_and_document`: внутри транзакции после вставки маркера, документа и части строк подсунут duplicate PK профиля → ошибка; после неё `directory_schema_version=None`, snapshot пуст, документ побайтово старый. **Retry** с валидным snapshot → `Applied`, все строки и документ на месте.
- `sqlite_cas_conflict_rolls_back_and_retry_applies`: `DocumentChanged` → маркера нет, строк нет, документ не изменён; retry с корректным `expected` → `Applied`.
- `sqlite_put_document_with_directory_is_atomic_on_mid_tx_failure`: падение после записи документа и части directory-строк откатывает и документ, и snapshot.
- `sqlite_non_empty_rows_adopted_on_first_migration`: на свежей БД уже есть непустые SQL-строки без маркера → строки побеждают, legacy `g-1` не импортируется, документ очищен, маркер поставлен.

### 2.3. Startup и реальный бинарь

- Испорченный JSON в `configuration_documents` (БД предварительно создана через `SqliteStorage::init`):
  - `exit=78`, лог `configuration rejected; aborting startup error=T10 directory migration: legacy document is not valid JSON...`;
  - повторено с `VKS_STORAGE_ALLOW_FALLBACK=true` — **тоже exit 78** (fallback не маскирует ошибку миграции);
  - после падения в БД: `schema_state` пуст, все 6 directory-таблиц пусты, документ `{not json` не изменён.
- Валидный legacy-документ: первый старт — `outcome="applied"`, затем `vks-server ready and listening`; документ очищен, строки/маркер записаны. Второй старт — `outcome="already_migrated"`, данные целы.
- Порядок в коде: `_db_lock` (`main.rs:110-118`) → init storage (`:133+`) → `directory_migrate::run` (`:183`) → `sync_from_storage` (`:187`). Соответствует требованию.

### 2.4. Reset / Memory / PG

- `MemoryStorage::reset` обнуляет `directory_state` (snapshot + marker) — `core_adapter.rs:678-692`; SQLite/PG `reset` удаляет 6 таблиц и `schema_state` — `:1297-1307`, `:1924-1934`.
- PG-хелперы (`postgres_replace_snapshot_tx`, `postgres_load_snapshot`, `postgres_directory_row_count`, `postgres_schema_version`) повторяют SQLite SQL: тот же порядок `DELETE`, те же колонки, те же `ORDER BY`, те же `$n`; `migrate_directory_document` в PG использует тот же `INSERT ... ON CONFLICT ... WHERE version < excluded.version` + CAS. Семантика идентична; живой PG не запускался.

### 2.5. Гейты (точные числа)

| Гейт | Результат |
|---|---|
| `cargo fmt --all -- --check` (pristine `3e2983c`) | exit 0, 0 строк diff |
| `cargo clippy -p vks-storage -p vks-admin -p vks-server -p vks-core --all-targets -- -D warnings` | exit 0, warning’ов нет |
| `cargo test -p vks-storage --locked --offline` | **24** lib + **7** `tests/directory_migration.rs` + **1** `tests/dedup_lock.rs` = **32 passed**, 0 failed |
| `cargo test -p vks-admin --lib --locked --offline` | **368 passed**, 0 failed, 0 ignored (50.43 s) |
| `cargo test -p vks-server --lib --locked --offline` | **150 passed**, 0 failed, **3 ignored** |
| `cargo test --workspace --locked --offline --no-fail-fast` (pristine) | **1557 passed, 1 failed, 8 ignored**; единственный failed — `vks-media-mcu farm::tests::concurrent_acquires_share_one_encoder_process` (timeout под параллельной нагрузкой), **изолированно passed** за 0.35 s; к T10 отношения не имеет |

T10-специфичных флейков/падений в параллельном workspace-прогоне не было.

---

## 3. Находки

### P2-1. `MemoryStorage::migrate_directory_document` оставляет частичное состояние при CAS-конфликте

`crates/vks-storage/src/core_adapter.rs:465-475`: snapshot присваивается **до** проверки CAS; при несовпадении документа возвращается `DocumentChanged`, но строки уже записаны в память, а маркер — нет. Это противоречит контракту `DirectoryMigrationOutcome::DocumentChanged` («Nothing was written», `crates/vks-core/src/ports/storage.rs:260-263`).

Доказательство: `memory_cas_conflict_leaves_snapshot_partially_written` — после `DocumentChanged`: `version=None`, но `load_directory_snapshot()` равен импортированному fixture; документ остался старым. Retry с корректным `expected` сходится в `Applied`, поэтому практический ущерб ограничен (на startup конкурентного писателя нет, Memory — dev/test-backend), но паритет с SQL и обещание «no partial state» нарушены.

**Рекомендация:** проверять `expected_document`/CAS до присваивания `directory.snapshot`, либо клонировать состояние и коммитить его только после успешной проверки.

### P2-2. Unknown/future-поля внутри адресов теряются при первой же мутации после миграции

Миграция кладёт исходный адрес в `address_json` (`crates/vks-admin/src/api_v3/directory_migrate.rs:347-357`, overlay `:134-155`), но `AddressRecord` не имеет catch-all поля (`crates/vks-admin/src/api_v3/resources.rs:121-129`), а `apply_snapshot` при загрузке парсит `address_json` в типизированный `AddressRecord` (`directory_migrate.rs:402-409`), после чего `stored_address` сериализует его заново без неизвестных ключей.

Доказательство: `probe_future_address_field_survives_first_mutation` — сразу после миграции `address_json["future_address"] == true`; после одного `POST /api/v3/groups` (любая directory-мутация через `persist_with_directory`) поле исчезает (`Null`). Профили/группы/роли не страдают: у них есть `#[serde(flatten)] extra` (`crates/vks-admin/src/api_v3/directory.rs:178-208`).

Это не потеря текущих данных (поля будущих версий), и в старом JSON-пути такие поля тоже терялись при мутации, но T10 добавляет `address_json` именно как catch-all (P2-5) и объявляет сохранение unknown-полей — обещание выполняется только до первой мутации.

**Рекомендация:** добавить в `AddressRecord` `#[serde(flatten)] extra: BTreeMap<String, Value>` (или протаскивать `address_json` в `snapshot_from_resources` без ре-сериализации) и покрыть тестом «миграция → мутация → unknown-поле на месте».

### P2-3. Две процессы на одной БД: whole-snapshot replace затирает строки другого процесса (last-writer-wins)

`persist_with_directory` строит snapshot из **локального RAM-кэша** (`crates/vks-admin/src/api_v3/resources.rs:285-294`), а storage заменяет snapshot целиком: `DELETE` всех 6 таблиц + `INSERT` (`crates/vks-storage/src/core_adapter.rs:73-88`, PG `:232-245`; `put_configuration_document_with_directory` — `:724-737`/`:1355-1368`).

Доказательство: `probe_two_states_same_db_last_writer_wins` — два `AppState` на одном SQLite-файле (стенд «второй PG-узел»); узел A создаёт `sip:node@example.com`, узел B (со stale-кэшем) создаёт тот же URI: B получает **201**, после чего в БД остаётся только строка B с новым id — строка A молча исчезла. Ни 409, ни 500, ни предупреждения.

Для одиночного сервера (SQLite под `DbLock`, один процесс) это недостижимо и покрытие T10 single-node в pitfalls (`T10.md` P1-10) достаточное. Но для PG-кластера/второго инстанса это потеря данных, а не только 409-конфликт.

**Рекомендация:** либо задокументировать «PG-кластер с T10 не поддерживается, один writer», либо перейти на per-row upsert/удаление по id + версию/`pg_advisory_lock`, либо на CAS по версии снапшота.

### P3-1. Персональный canonical-дедуп не переписывает `primary_address`

Глобальные алиасы переписывают `primary_address` (`directory_migrate.rs:247-251`), а `dedup_personal_book` (`:261-290`) удаляет дубликат personal-копии, но не трогает `users[].primary_address`: в `dedup_address_book` (`:253-257`) он вызывается без доступа к `users`.

Доказательство: `probe_personal_dedup_leaves_dangling_primary_address` — `primary_address="p-b"`, после дедупа в книге остался `p-a`, а профиль по-прежнему ссылается на удалённый `p-b`. API переживёт (битая ссылка → `null`), но «звёздочка»/primary теряется.

**Рекомендация:** возвращать из `dedup_personal_book` map aliases и переписывать `primary_address` по ней.

### P3-2. Нет обновления deploy/README под T10

`deploy/README.md:237-274` описывает только `vks-db-dedup`/backup и не упоминает: T10-миграцию, `schema_state('directory')`, запрет отката на старый бинарь после миграции (старый бинарь не увидит SQL-справочник), поведение `vks-db-dedup` после T10 (на новой БД `resources.directory` уже нет) и необходимость бэкапа перед первым стартом. Пункт 11 из `T10.md` §3.2 не выполнен; коммит `451fd9f` docs/deploy не трогал.

**Рекомендация:** добавить короткий раздел «T10 directory migration: backup → first start → no downgrade → dedup is no-op on new DB».

### P3-3. Unknown-поля верхнего уровня документа сохраняются только до первого `persist`

`configuration_document` (`crates/vks-admin/src/api_v3/resources.rs:298-310`) заново собирает JSON из `version/resources/conference_settings/video_settings`; любые будущие ключи вне `resources` (например, `future_top`) после миграции сохраняются, но исчезают при первой же мутации layout/settings/history через `persist`. Это pre-existing поведение, не регрессия T10, но forward-compat-обещание T10 частичное; в отчёте стоит явно ограничить его «сохраняется на момент миграции».

### P3-4. `already_migrated` печатает счётчики legacy-документа как будто он импортирован

`directory_migrate.rs:459-461` формирует `MigrationReport` из `parsed.report` и на `AlreadyMigrated`; в логе видны `profiles=… global_addresses=…`, хотя `imported=false` и ничего не записано. Это может путать при аудите миграции.

**Рекомендация:** обнулять счётчики (или писать `parsed` отдельным полем) при `already_migrated`.

### Известное ограничение (не дефект, задокументировано)

Если маркер потерян **и все 6 таблиц пусты**, старая JSON-книга импортируется снова — `existing == 0` (`core_adapter.rs:767-771`), отличие от «свежей БД» невозможно. Это прямо зафиксировано в `T10.md` P0-2 (документированный предел); тест `sqlite_lost_marker_with_empty_tables_resurrects_documented_limit` подтверждает поведение. Требование «ops-бэкап + знание предела» остаётся в силе; в README (P3-2) это стоит продублировать.

---

## 4. Что проверено и претензий нет

- DDL `0007`: PK `(owner_id,id)`, `owner_id=''` = global, partial unique по canonical для global, **нет FK** на built-in роли/пользователей (`crates/vks-storage/migrations/0007_directory_normalized.sql`).
- Транзакция миграции: маркер → adopt/import rows → CAS документа → commit; откат при ошибке на любом шаге (SQLite-тесты выше). CAS-конфликт не перезаписывает документ.
- Порядок «существующие строки побеждают»: удалённый контакт не воскресает, пока в SQL есть хоть одна строка (включая только профиль/роль) — `sqlite_mid_transaction...`, `sqlite_non_empty_rows_adopted_on_first_migration`.
- `marker present` → `AlreadyMigrated`, ничего не пишется и не чистится (P0-2: не удалять данные, добавленные чужим writer’ом).
- `#[serde(skip)]` на `addresses`/`directory`: старый документ не воскрешает удалённое; JSON больше не содержит оба ключа; профили/группы/роли сохраняют unknown-поля через `flatten`.
- Все мутации справочника/адресов идут через `commit_directory`/`commit` → `put_configuration_document_with_directory` (одна транзакция); прямых мутаций `resources.directory`/`addresses` в обход commit в прод-коде не найдено (кроме гидратации `apply_snapshot`).
- Startup и hot reload грузят SQL-snapshot **вне** `if let Some(document)` (`state.rs:727-741`), т.е. справочник жив и без JSON-документа.
- `delete_user` чистит профиль/членства/personal в SQL, глобальная книга и personal других пользователей целы (`probe_delete_user_keeps_global_and_other_personal_rows` + `delete_user_cascades_credentials_memberships_and_profile`).
- Personal-копия адреса с тем же id, что глобальный, живёт отдельной строкой и переживает удаление глобального (`personal_address_copy_survives_global_delete`).
- `reset()` Memory/SQLite/PG чистит строки **и** маркер, повторная миграция после reset снова применяется.
- `/api/v1/records` и recording-код T10 не менял; admin/server workspace-тесты по records/rooms проходят.
- `vks-db-dedup` не сломан: `dedup_document` идемпотентен, personal-дедуп добавлен, тест `second_run_is_byte_identical`/`dedup_lock` проходит.
- Raw `serde_json::Value` при миграции: документ не проходит через `ResourceState`; unknown-поля профилей/групп/ролей/адресов на момент миграции сохраняются (см. P2-2 про дальнейшую жизнь адресов).

---

## 5. Ограничения ревью

- **PG не исполнялся:** нет `VKS_STORAGE_PG_URL`, `psql`, `pg_isready`, `docker`/`podman`. `sqlite_and_pg_load_same_directory_snapshot` в прогоне `vks-storage` формально `ok`, но печатает `SKIP`; PG-путь проверен только построчным сравнением SQL-хелперов и DDL. Для оценки 9.5+ нужен реальный PG-прогон (CI job `test-postgres` его делает).
- Проверки «second node» и «unknown address fields after mutation» — независимые скрэтч-пробы, а не тесты коммита; они намеренно падают, фиксируя P2-2/P2-3, и в репозиторий не добавлялись.
- Реальная БД, live-порты и `backups/` не трогались; все артефакты — в `/dev/shm`, после отчёта удалены.

---

## 6. Вердикт

**done: yes** по формальным критериям: 0 P0, 0 P1; миграция, маркер, no-resurrection (при непустых строках), очистка JSON, PK/owner_id, global unique, атомарность SQLite/PG-кода, fail-fast startup, reset и durability подтверждены исполнением. **Score: 9.0/10.**

Перед тем как считать T10b окончательно закрытой, рекомендую исправить **P2-1** (тривиально) и **P2-2** (catch-all в `AddressRecord`), а для **P2-3** — либо явно задокументировать single-writer/PG-cluster ограничение, либо перейти на per-row upsert. P3-1/P3-2 стоит закрыть вместе со следующим изменением справочника/деплой-документации.

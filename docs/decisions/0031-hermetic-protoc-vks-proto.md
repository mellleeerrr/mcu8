# ADR-0031: Герметичный protoc для vks-proto (без системной зависимости)

- **Статус:** accepted
- **Дата:** 2026-10-23
- **Решение принято:** владелец проекта (S15 трекера)
- **Связанные ADR:** 0005 (протокол — protobuf/prost)
- **Связанные требования:** S15 трекера; `docs/PROJECT_RULES.md` §19.3 (никаких `unwrap`/`expect` в проде без объяснения)

## Контекст (Context)

`vks-proto` генерирует Rust-код из `proto/*.proto` через `prost-build` в `build.rs`.
Исходно сборка опиралась на `protobuf-src` под фичей `vendored-protoc`: крейт компилировал
protobuf 21.5 из исходников через autotools — «чтобы НЕ требовать системного `protoc`
(кросс-платформенно: Linux/macOS/CI)».

Два дефекта, оба воспроизведены локально:

1. **Фича не собирается в пути с пробелом.** `protobuf-src` собирает protobuf в `$OUT_DIR`
   воркспейса; libtool protobuf'а не переносит пробел в `$PWD` и режет путь:
   ```
   configure: WARNING: Libtool does not cope well with whitespace in `pwd`
   libtool: error: cannot find the library 'стол/mcu/.../src/libprotobuf.la'
   make[1]: *** [Makefile:4001: protoc] Ошибка 1
   ```
   Сборка protobuf идёт около трёх минут и падает на финальной линковке. Собственный каталог
   проекта содержит пробел («/home/pro/Рабочий стол/mcu») ⇒ фича нерабочая **в рабочем
   каталоге проекта**. Починить на стороне `build.rs` нельзя: пробел режет libtool внутри
   protobuf, а не cargo.

2. **Системного `protoc` в CI нет.** Ветка без фичи берёт `protoc` из `PATH`, а образ
   `ubuntu-latest` (22.04 и 24.04) его не содержит. Шаги `clippy`, `test`, `build` упали бы
   с «Could not find `protoc`». Дополнительно: `cargo check --workspace --all-features
   --all-targets` падал (exit 101) ⇒ ни одна фича с реальным кодом (`oxideav`, `rvoip`,
   `str0m`, `openapi`) не компилировалась в CI ни разу.

Побочно выяснилось, что `build.rs` содержал мёртвый код: подстановка `PROTOC_INCLUDE`
(`~/.cargo/include`) не нужна — ни один `.proto` не импортирует `google/protobuf/*`,
а флаг `--experimental_allow_proto3_optional` избыточен (proto3-optional стабилен с 3.15,
prost-build 0.13 сам его не добавляет).

## Варианты (Options)

### Вариант A: снять фичу, требовать системный `protoc` в CI
- Описание: удалить `vendored-protoc` и `protobuf-src`; в CI поставить `protoc`
  пакетным менеджёром.
- Плюсы: ноль новых зависимостей; минимальный диф.
- Минусы: системная зависимость переносится на каждого разработчика; версия `protoc`
  задаётся образом CI и дрейфует; ломается офлайн-сборка; отменяется документированное
  намерение `build.rs` («не требовать системного `protoc`»); требует отдельной инструкции
  «поставь protoc 3.21.5», иначе сгенерированный код разъезжается с локальным.
- Риски: тихий дрейф версии `protoc` между машинами.

### Вариант B: `protoc-bin-vendored` (готовые бинари в крейтах)
- Описание: `protoc-bin-vendored` = 9 крейтов (агрегатор + 8 платформенных), в каждом
  лежит готовый бинарь `protoc` и include с well-known типами. Ни компиляции C++, ни сети
  на этапе сборки.
- Плюсы: бинарь лежит в реестре cargo, а не в `$OUT_DIR` воркспейса ⇒ в пути проекта
  пробелов не остаётся вовсе (чинит класс отказа, а не экземпляр); сборка hermetic и
  работает офлайн; версия `protoc` зафиксирована в `Cargo.lock` ⇒ воспроизводимость;
  покрыты все целевые платформы из `rust-toolchain.toml`.
- Минусы: девять новых крейтов в графе и около 30 МБ в кэше реестра; поверхность
  supply-chain (крейт с готовыми бинарями).
- Риски: смена версии `protoc` может изменить сгенерированный код — снято измерением
  (см. «Решение»).

### Вариант C: `protobuf-src` 2.x (сборка protobuf через CMake)
- Описание: новая ветка `protobuf-src` (MaterializeInc) перешла с autotools на CMake,
  а CMake переносит пробелы в путях.
- Плюсы: официальный вендорский фикс; та же сборочная зависимость.
- Минусы: требует `cmake` на машине сборки (новая системная зависимость — хуже, чем
  `protoc`); компилирует protobuf из исходников (минуты на чистой сборке); версия
  `protoc` меняется 21.5 → 27.1.
- Риски: долгие и тяжёлые сборки; расхождение версий с локальной машиной.

## Решение (Decision)

Выбран **Вариант B**: `protoc-bin-vendored` без условных фич, системный `protoc`
переопределяется переменной `PROTOC` (соглашение prost-build).

1. **Фича `vendored-protoc` удалена** из `crates/vks-proto/Cargo.toml` вместе с
   зависимостью `protobuf-src`. Это **BREAKING-изменение**: фича была публичной
   (`cargo build -p vks-proto --features vendored-protoc` перестаёт собираться).
   Внешних потребителей нет — ни у одного крейта воркспейса нет `publish = true`,
   крейты публикуются только как часть монорепозитория; ломается лишь код,
   включавший фичу вручную внутри этого воркспейса.
2. **`build.rs` упрощён** (52 → 44 строки, из них 13 — комментарии): `protoc` берётся из
   переменной `PROTOC`, иначе — `protoc_bin_vendored::protoc_bin_path()`; `PROTOC_INCLUDE` =
   `include_path()` из крейта (нужен, только если `.proto` будут импортировать
   `google/protobuf/*`); убран мёртвый костыль `~/.cargo/include` и устаревший флаг
   `--experimental_allow_proto3_optional`.
3. **Сгенерированный код не меняется.** Проверено измерением: дескрипторы от protoc 3.21.5
   (локальный системный) и от protoc 36.2 (в `protoc-bin-vendored` 3.3.0) **байт-в-байт
   равны** (25049 байт), и все четыре сгенерированных файла `vks.*.rs` совпадают
   побайтово. Diff сгенерированного кода — нулевой (файлы живут в `OUT_DIR`, вне git).

## Последствия (Consequences)

### Положительные
- Сборка `vks-proto` работает в любом пути, включая путь с пробелом, и не требует
  системного `protoc` — ни у разработчика, ни в CI.
- `cargo check/clippy/test --workspace --all-features` перестали падать: впервые
  компилируются `oxideav`, `rvoip`, `str0m`, `openapi`.
- Версия `protoc` зафиксирована в `Cargo.lock` ⇒ сгенерированный код воспроизводим.
- `build.rs` потерял мёртвый код (костыль `~/.cargo/include`, устаревший флаг) и
  cfg-ветку — на одну фичу меньше.

### Отрицательные / компромиссы
- **BREAKING:** фича `vendored-protoc` удалена (публичный интерфейс крейта сузился).
  Внешних потребителей нет: `publish = true` не задан ни у одного крейта воркспейса,
  поэтому ломается только код, включавший фичу вручную.
- Девять новых крейтов в графе зависимостей и около 30 МБ в кэше реестра.
- Supply-chain поверхность: крейт с готовыми бинарями (все — MIT, разрешены `deny.toml`).
- `PROTOC_INCLUDE` ставится через `std::env::set_var`: в edition 2021 это безопасно,
  в edition 2024 потребуется `unsafe`-блок (отмечено комментарием в коде).
- `cargo deny` в CI теперь проверяет граф с `--all-features` — состав графа вырос.

### Что нужно сделать для реализации
- [x] `crates/vks-proto/Cargo.toml`: снять `[features]`, заменить `protobuf-src`
      на `protoc-bin-vendored`.
- [x] `crates/vks-proto/build.rs`: новая версия (без cfg-веток и мёртвого костыля).
- [x] `Cargo.lock`: обновить (удалён `protobuf-src`/`autotools`, добавлены 9 крейтов).
- [x] `.github/workflows/ci.yml`: `--locked` и `--all-features` во всех шагах, кэш cargo,
      пининг образа `ubuntu-24.04`, новые джобы `check`, `test-serial`,
      `path-with-spaces`, `doc`.
- [x] Проверка «сгенерированный код не изменился» (дескрипторы и `vks.*.rs` — байт-в-байт).

## Обоснование выбора библиотек (если применимо)

| Библиотека | Версия | Лицензия | Почему | Альтернативы отвергнуты |
|---|---|---|---|---|
| `protoc-bin-vendored` | 3.3 | MIT | Готовые бинари в крейтах: без C++, без сети, без пробелов в пути; версия в `Cargo.lock` | `protobuf-src` 1.x (libtool), `protobuf-src` 2.x (нужен cmake) |
| `prost-build` | 0.13.5 | Apache-2.0 | Уже в воркспейсе; `PROTOC`/`PROTOC_INCLUDE` — штатные точки расширения | — |

## Документация, изученная перед решением

- https://docs.rs/protoc-bin-vendored/3.3.0/protoc_bin_vendored/ — API `protoc_bin_path()`,
  `include_path()`; набор платформенных крейтов.
- https://crates.io/api/v1/crates/protoc-bin-vendored — версия 3.3.0 от 2026-10-05,
  лицензия MIT, активные публикации.
- https://crates.io/api/v1/crates/protobuf-src — текущая ветка 2.1.1+27.1 (CMake),
  прошлая 1.1.0+21.5 (autotools, используется в проекте).
- https://docs.rs/prost-build/0.13.5/prost_build/struct.Config.html — `protoc_executable`,
  `compile_well_known_types` (ставит `prost_types = false`), отсутствие сеттера
  `protoc_include` (include берётся только из `PROTOC_INCLUDE`).
- https://raw.githubusercontent.com/actions/runner-images/main/images/ubuntu/Ubuntu2404-Readme.md
  — состав образа: `protoc`/`protobuf-compiler` отсутствуют.

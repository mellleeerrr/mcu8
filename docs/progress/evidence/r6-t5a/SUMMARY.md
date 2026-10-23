# T5a — удаление мёртвого кода (R6)

База: `4c36a6c` (T2.8+T13). Коммит T5a — см. `git log --grep "T5a"`.

## Что удалено (0 потребителей, проверено grep по крейтам и клиенту)
| Файл/сущность | LOC |
|---|---|
| `vks-signaling/src/reconnect.rs` целиком (+ `pub mod`/`pub use Backoff, ResumeNonce`) | 166 |
| `vks-signaling` no-op заглушки `tungstenite_impl`/`wtransport_impl` + фичи `ws`/`wtransport` | ≈187 |
| `vks-signaling` dev-dep `proptest` | — |
| `vks-layout`: `Layout::total_area`/`any_overlap` + тест | ≈40 |
| `vks-storage`: legacy standalone `layouts` (`rows::Layout`/`Tile`, `LayoutRow`, `insert/get/list_layout` в memory/sqlite/postgres, 2 теста) | ≈130 |
| `vks-admin`: `tower` → dev-dependencies | — |
| `vks-storage`: sqlx feature `_unstable-all-types` (не использовалась) | — |

Итог `git diff --shortstat`: 12 файлов, +17 −552; `Cargo.lock` −118 строк
(ушли транзитивные зависимости sqlx all-types). Мёртвая половина `core_layouts`
(≈150), `frame_for_viewer`/`set_self_view`, `core_adapter.rs`, `SlotRole`/Codian,
`webrtc_glue`, `viewer.rs`/`plan_viewer`, трейты/моки ws/wt — НЕ удалялись
(см. решения T5 в мастер-доке: T5b после T10b, T5c по отдельному решению).

## Гейты
- `cargo fmt --all -- --check` — 0.
- `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` — 0 ошибок.
- `cargo test --workspace --locked --no-fail-fast`: **1301 passed, 0 failed**
  (база `4c36a6c`: 1308 passed; удалено 7 реально запускавшихся тестов —
  4 reconnect, 1 layout, 2 storage; ещё 2 теста заглушек ws/wt были
  feature-gated и по умолчанию не запускались).
- `cargo test -p vks-signaling -p vks-layout -p vks-storage`:
  48→44, 77→76, 25→23.
- `cargo check -p vks-client-sdk --target wasm32-unknown-unknown --locked` — 0.

## Проверка отсутствия ссылок
`grep -rn "reconnect" crates client` — остались только описательные комментарии
о поведении reconnect в `vks-core/session.rs`, `vks-media-sfu` и
`signaling_client.rs` (не ссылки на удалённый модуль); `grep -rn "feature = \"ws\"|feature = \"wtransport\""` — 0; `Backoff|ResumeNonce|TungsteniteSession|WtSession` — 0.

Исторические упоминания в `docs/audit/FULL_AUDIT_2026-10-16.md` (аудит прошлой
фазы) не правились — это снимок состояния, актуализация документации в T6.

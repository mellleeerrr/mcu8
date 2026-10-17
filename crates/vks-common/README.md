# vks-common

Общие утилиты для backend-крейтов VKS (фаза 0).

## Модули
- `error` — единый `Error` (thiserror) + `ErrorCode`/`HttpStatus` (BACKEND_RULES §6.4).
- `tracing` — инициализация `tracing-subscriber` (human/JSON), правила логирования (PROJECT_RULES §19.4).
- `metrics` — Prometheus-реестр + дефолтные метрики (CLUSTERING §12).
- `config` — загрузка конфига: defaults → TOML → env `VKS_*` (BACKEND_RULES §6.3, R6).
- `ids` — UUIDv7 (room/participant) и ULID (msg_id) (SIGNALING_PROTOCOL.md).

## Позиция в дереве зависимостей
`vks-common` — standalone в фазе 0. В фазе 1 получит зависимость от `vks-core`
(см. ARCHITECTURE.md §3, дерево зависимостей).

## Авторитативные документы
- `docs/rules/BACKEND_RULES.md` §6
- `docs/PROJECT_RULES.md` §19–22
- `docs/DEVELOPMENT_PLAN.md` — Фаза 0

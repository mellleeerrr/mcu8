# VKS — Видеоконференцсвязь на Rust

Собственный сервер видеоконференцсвязи (ВКС) на Rust с поддержкой режимов **MCU** и **SFU** (переключение оператором, прозрачно для клиента), протоколов **WebRTC, SIP, H.323**, всех популярных аудио/видео кодеков, управлением из фронтенда и кастомными раскладками.

## Начало работы

1. **Прочитать правила:** [`docs/PROJECT_RULES.md`](docs/PROJECT_RULES.md) — обязательный для любого, кто работает над проектом (включая ИИ-агентов).
2. **Архитектура:** [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) — требования, инвариант MCU/SFU, структура workspace, стек.
3. **План разработки:** [`docs/DEVELOPMENT_PLAN.md`](docs/DEVELOPMENT_PLAN.md) — фазы 0–8 с критериями готовности.
4. **Решения (ADR):** [`docs/decisions/DECISIONS.md`](docs/decisions/DECISIONS.md) — реестр архитектурных решений.
5. **Статус:** [`docs/progress/PROGRESS.md`](docs/progress/PROGRESS.md) — что сделано, что в работе, известные ошибки.

## Структура документации

```
docs/
├── PROJECT_RULES.md          # ← правила для всех (человек + ИИ-агент)
├── DEVELOPMENT_PLAN.md       # фазы 0–8, критерии готовности
├── rules/                    # детальные правила по областям
│   ├── BACKEND_RULES.md      # auth, security, reliability, data
│   ├── FRONTEND_RULES.md     # SPA, UX, SDK API, PWA, i18n
│   └── CODE_STYLE.md         # rustfmt, clippy, tracing, tests
├── architecture/
│   ├── ARCHITECTURE.md       # ← целевая архитектура
│   ├── SIGNALING_PROTOCOL.md # protobuf, FSM, reconnect, congestion
│   ├── MEDIA_PIPELINE.md     # SFU/MCU pipeline, mode switch, codecs
│   ├── LAYOUT_ENGINE.md      # формат, БД, MCU/client apply, editor
│   └── CLUSTERING.md         # NATS, failover, backup, TLS, SLO, runbooks
├── decisions/
│   ├── DECISIONS.md          # реестр ADR
│   ├── TEMPLATE_DECISION.md  # шаблон
│   └── NNNN-*.md             # отдельные ADR
└── progress/
    ├── PROGRESS.md           # живой статус (обновляется в каждом коммите)
    ├── TEMPLATE_ERROR.md     # шаблон записи ошибок
    └── archive/              # закрытые итерации
```

## Статус проекта

**Фаза 0 — в работе.** Документация и правила готовы. Далее: workspace, CI, `vks-common`, `vks-proto`.

См. [`docs/progress/PROGRESS.md`](docs/progress/PROGRESS.md) для деталей.

## Лицензия

(определить в ADR-0023)

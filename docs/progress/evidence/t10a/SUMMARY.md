# T10a — единый справочник и UI-адреса

Дата: 2026-10-22. HEAD: `e78fa47` (приёмка `5f59dea`, ревью **9.5/10 APPROVED**, `directory-db`).

## Что проверено на живом стенде

- Одна физическая БД `vks_data.db`; один `configuration_documents` документ
  `pnm-definitions-v1`; resources поднимаются из него после рестарта.
- `/api/v3/users` = 2 active (`Administrator`, `cc`); deleted-профили скрыты.
- `/api/v3/call_strings` = 2 глобальных адреса: `h323:192.168.1.132`,
  `sip:192.168.1.132`; canonical-дублей нет.
- Личный адрес `cc` (`sip:192.168.1.132`) виден в карточке справочника:
  «Адрес → Зал 132 (sip) → sip:192.168.1.132».
- Vendor-алиасы: 11 trailing-slash коллекций + `/api/v3/users[/]`,
  `/api/v3/ui/users[/]`, `/api/v3/ui/directory/users[/]`,
  `/api/v3/ui/users/{id}`, `/api/v3/ui/directory/users/{id}` = 200;
  коллекции байт-в-байт совпадают с canonical.
- Startup-dedup: `normalize_loaded` схлопывает одинаковые canonical URI,
  оставляет самый ранний id, переписывает `directory.user_addresses`,
  объединяет payload; пустое имя заменяется непустым.
- Рестарт: после пересборки и `systemctl --user restart` API отдаёт те же ids,
  `/readyz` = 200, в логе нет ERROR/panic.

## Гейты

```text
cargo test -p vks-admin      274 passed / 0 failed
cargo clippy -p vks-admin --all-targets -- -D warnings   clean
cargo fmt --all -- --check                               clean
```

## Осталось (T10b, отдельная задача)

- SQL `call_strings` + durable unique index на canonical URI.
- Перенос global/personal addresses и directory из configuration-document.
- Миграция 69/70 старых `contact_id` в history (snapshots сохранены).
- Drop пустых legacy-таблиц (`users`, `layouts`, `api_keys`, `audit_log`,
  `room_mode_override`, `core_api_keys`, `core_layouts`).

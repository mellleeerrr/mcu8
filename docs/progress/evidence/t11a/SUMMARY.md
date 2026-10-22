# T11a — честный system status

**Статус:** реализовано; код/тесты в рабочем дереве, live-проверка выполнена.

## Что изменено
- `AppState.server_addr` уже содержал нормализованный ICE-candidate; статус
  теперь берёт IP оттуда, а не `settings.ip` (`127.0.0.1`).
- `AppState.database_file` заполняется в `main.rs` из `database_url` через
  `vks_storage::db_lock::sqlite_path_from_url`; `database.size_bytes` — реальный
  `stat` файла (0 для in-memory/PostgreSQL).
- `AppState.gateway_health` + `record_gateway_health`/`gateway_health(ttl)`:
  `GatewayManager` пишет факт SIP/H.323-дозвона (успех/ошибка), статус читает
  последний результат с TTL 300 с.
- `/api/v3/ui/system/status`: `sip`/`h323` получают `status/connected/enable/
  registration/account=FALSE` и `last_error` при реальном отказе; `network.ip`
  и `local_address` — фактический IP.

## Live-проверка (2026-10-22, HEAD рабочего дерева)
- `network.ip=192.168.1.93`, `sip.local_address=192.168.1.93`.
- `database.size_bytes=2011136`, совпадает с `ls -l vks_data.db`.
- Реальный H.323-дозвон: `remote_release` → `h323.status=FAILED`,
  `connected=false`, `last_error="H.323 outbound dial failed: ... remote_release"`.
- После успешного SIP-дозвона — `READY`, `last_error=null`; неудачный вызов на
  занятый терминал не красит шлюз навсегда (TTL 300 с).

## Гейты
- Rust: **1038 passed / 0 failed / 6 ignored**; clippy/fmt чисто.
- Новый тест: `system_status_uses_real_ip_db_size_and_last_gateway_dial`.
- Живой SIP E2E остаётся 114/114 (high/low), H.323 live по-прежнему заблокирован
  терминалом (`remote_release`) — T07.

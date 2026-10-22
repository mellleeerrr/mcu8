# T11a — честный system status

**Статус:** реализовано и проверено live; ожидает финального ревью.

## Что изменено
- `network.ip` и `sip/h323.local_address` берутся из `AppState.server_addr`
  (нормализованный ICE candidate), а не из `settings.ip`.
- `database.size_bytes` — реальный `stat` SQLite-файла; путь заполняется в
  `main.rs` через `sqlite_path_from_url`. Для in-memory/PG остаётся 0 (T10).
- `AppState.gateway_health` хранит **последний фактический** результат
  SIP/H.323-дозвона; `GatewayManager` пишет его ровно один раз на попытку:
  успех — после полного media-setup, ошибка — из единой обёртки
  `dial_with_contact` (включая ошибки до/после сети).
- Классификация: `busy/486/603/remote_release/decline/adaptiveBusy` — отказ
  терминала, он **не** переводит шлюз в FAILED; транспортные/ресурсные
  ошибки — FAILED до следующего фактического успеха.
- `enable` — наличие dialer, `registration` — реальный
  `SipGateway::is_registered()` / `H323Gateway::is_registered()`,
  `account/connected/status` — health. TTL-самолечения нет: FAILED не
  исчезает сам, READY без регистрации/успеха не выставляется.
- Убрано случайное изменение sidecar (`VKS_H323_WIRE_LOG`) из T11a-scope.

## Live-проверка (2026-10-22, HEAD после redeploy)
- `network.ip=192.168.1.93`, `sip/h323.local_address=192.168.1.93`.
- `database.size_bytes=2019328` == `ls -l vks_data.db`.
- До вызова: `sip/h323.status=READY`, `registration=true`, `last_error=null`.
- Реальный H.323 `remote_release`: `h323.status=READY`, `connected=true`,
  `registration=true`, `last_error="H.323 outbound dial failed: ... remote_release"`
  — отказ терминала не гасит живой шлюз и не «лечится» таймаутом.
- Транспортный FAILED и отсутствие регистрации проверены unit-тестами
  (`system_status_uses_real_ip_db_size_and_last_gateway_dial`,
  `terminal_and_transport_gateway_errors_are_classified`).

## Гейты
- Rust: **1039 passed / 0 failed / 6 ignored**; clippy/fmt чисто.
- Python sidecar: **24/24**.
- Живой SIP E2E: high/low 114/114 (на предыдущем финальном бинарнике; этот
  коммит меняет только статус и не трогает медиа-путь).
- H.323 live по-прежнему `remote_release` — T07, не T11a.

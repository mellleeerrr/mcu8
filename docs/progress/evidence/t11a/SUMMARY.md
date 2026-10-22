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
- Классификация **структурная, без подстрок текста**: SIP —
  `SipDialError::Rejected/InvalidSdpAnswer/NoCommonCodec{peer_status}`,
  H.323 — точный токен sidecar `remote_release[:cause]`; на границе шлюза
  ставится sentinel `terminal-reject: `, который снимается перед
  `last_error`/ответом. `sidecar not found`, таймаут на `sip:4861234@…`,
  IO `:4040`, `480/408/600` больше не дают ложных классификаций
  (тесты `terminal_marker_is_exact...`, `terminal_and_transport...`,
  `dial_reason_classification_uses_exact_tokens`).
- `enable` — per-protocol конфигурация; SIP `registration` — реальный
  `SipGateway::is_registered()`, H.323 `registration` — факт успешного
  H.225/H.245 setup либо реальный Q.931-ответ терминала (direct mode без
  регистрации); `account/connected/status` — health. TTL-самолечения нет:
  FAILED не исчезает сам, READY без факта не выставляется.
- `database.size_bytes` = main + WAL, путь канонизирован (не зависит от cwd);
  для in-memory/PG — `null`, а не ложный 0 при READY.
- Убрано случайное изменение sidecar (`VKS_H323_WIRE_LOG`) из T11a-scope.

## Live-проверка (2026-10-22, HEAD после redeploy)
- `network.ip=192.168.1.93`, `sip/h323.local_address=192.168.1.93`.
- `database.size_bytes=2019328` == `ls -l vks_data.db`.
- До вызова: `sip.status=READY/registration=true`; H.323 direct mode без
  факта setup — `DISABLED/registration=false` (не ложный READY),
  `enable=true`.
- Реальный H.323 `remote_release`: `h323.status=READY`, `connected=true`,
  `registration=true`, `last_error="remote_release"` (sentinel снят) —
  отказ терминала не гасит живой шлюз и не «лечится» таймаутом.
- Транспортный FAILED и отсутствие регистрации проверены unit-тестами
  (`system_status_uses_real_ip_db_size_and_last_gateway_dial`,
  `terminal_and_transport_gateway_errors_are_classified`).

## Гейты
- Rust: **1041 passed / 0 failed / 6 ignored**; clippy/fmt чисто.
- Python sidecar: **24/24**.
- Живой SIP E2E: high/low 114/114 (на предыдущем финальном бинарнике; этот
  коммит меняет только статус и не трогает медиа-путь).
- H.323 live по-прежнему `remote_release` — T07, не T11a.

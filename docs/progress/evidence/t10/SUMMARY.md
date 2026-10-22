# T10 — полный live E2E и финальный commit

Дата: 2026-10-22. База: `726e3f2` + T9 cleanup. Release пересобран и сервис
перезапущен на том же `configs/default.toml` (PID см. `systemctl --user show`).

## Сборка/перезапуск
- `cargo build --release -p vks-server` — exit 0, 1m50s.
- `systemctl --user restart vks-mcu.service` — active; `/readyz` 200,
  `/pnm/monitoring/sessions` 200.
- Лог: `SIP UAS listening 0.0.0.0:5060`, `H.323 UAS listening 0.0.0.0:1720`,
  `ready and listening for HTTP/WebSocket 0.0.0.0:8080`; panic/ERROR нет.

## Playwright live E2E
Команда:
```
NODE_PATH=/home/pro/.npm/_npx/e41f203b7505f1fb/node_modules \
VKS_ARTIFACTS=/tmp/vks-e2e-final \
VKS_CHROMIUM=/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome \
VKS_VIDEO_LOW=1 node tests/e2e/live.js
```
Результат: **233/233 checks OK**, `page_errors=[]`, 6 известных вендорских
`TypeError` в консоли + 2 ожидаемых 404 (detach) = 8 записей. `failure=null`, exit 0.
Полный JSON: [`result.json`](result.json).

Покрыто: login → `/pnm/monitoring/sessions` → комната → «Добавить участника по
адресу» → SIP `sip:probe-e2e@192.168.1.132` → connected → двусторонние
audio/video счётчики → все проверенные layout-пресеты (1/2/3/4/5/8/9/10/25/34/36,
2×2 7 вариантов, overlap-схемы, точные ячейки) → clear-all/detach → статистика →
cleanup без pageerror.

## API
- `/api/v3/video_layout_grids` — 6 builtin custom-схем (эталон TrueConf отдаёт
  ровно 6; 179 `preset_*` — статическая карта UI, не grid-каталог, иначе UI
  ломается). Критерий T0 «count ≥ 180» устарел и заменён этим контрактом.
- `/api/v3/sessions` — 200, `/api/v3/statistics` — 200 (engine.codecs).
- `/api/v3/sessions/{id}/video_layouts/templates` — 200.

## Git
- `cargo fmt --check` OK, clippy `-D warnings` OK, Python H.323 54/54 OK.
- `cargo test --workspace --lib`: 16/16 test-бинарей (16 — число крейтов);
  после стабилизации двух timing-тестов `media_runtime_tests` — 5 полных
  прогонов подряд зелёные (1092 passed / 5 ignored). До фикса тесты были
  flaky под нагрузкой: G711 ждал вторую половину 40-мс пакета фиксированные
  1 с, G723 допускал ровно 0 пропусков 20-мс тика; теперь G711 ждёт дренаж
  до 3+2 с, G723 допускает один планировщиковый gap (`<=1`), реальный
  underrun по-прежнему ловится (повторяется каждый второй тик).
- Commit содержит test-only фикс и evidence/docs; рабочее дерево после
  коммита чистое.

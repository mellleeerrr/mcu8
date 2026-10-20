# T1.6 — приёмка SIP+H.323 из UI, живые контролы, раскладка, `smoke_v3.sh`

Дата: 2026-10-20. Стенд: release `vks-server` pid 547164, sha256 `971323a3…`
(точные git head/pid/sha — в `summary.json → build`; Rust-код на момент прогонов
не менялся с `b78d0d9`, последующие коммиты — тесты/доки/скрипты).
UI `/pnm/`, endpoint `192.168.1.132` (SIP 5060, H.323 1720; одновременно
активен только один протокол).

## Что доказано (оба протокола, из веб-UI)

Шаги обоих сценариев: создание конференции и старт сессии из UI →
«Добавить участника по адресу» с автодополнением сохранённого адреса →
CONNECTED и рост медиа-счётчиков → mute камеры/микрофона/колонки (video.rx/audio.rx/audio.tx) из панели управления →
ручная раскладка на 4 окна («Заполнить видеоокна участниками» → «Применить»),
участник в позиции 1 → reload страницы и проверка слота в редакторе раскладки →
kick из UI (call_end + подтверждение «Отключить»).

| Проверка | H.323 | SIP |
|---|---|---|
| Модалка показывает выбранный протокол (`protocol_label`) | H323 | SIP |
| Живой вызов CONNECTED | ✅ | ✅ |
| Рост аудио-счётчиков rx/tx (точные числа — `summary.json`) | ✅ | ✅ |
| Видео H.264 640x360 rx | ✅ | ✅ |
| Камера выключается (`video.rx.enabled=false`) из UI | ✅ | ✅ |
| Микрофон выключается (`audio.tx.enabled=false`) из UI | ✅ | ✅ |
| Ручная раскладка: mode=manual, size=4, участник в position 1 | ✅ | ✅ |
| После reload участник остаётся в position 1 и виден в редакторе | ✅ | ✅ |
| Kick из UI: диалог подтверждения → `connected` пуст, участник в «Неподключившиеся» | ✅ | ✅ |
| Console errors на шагах вызова/контролов/раскладки/reload/kick | 0 | 0 |
| HTTP ≥400 за сценарий | 0 | 0 |

Первичные доказательства: 16 PNG (`01`–`08` H.323, `07`–`14` SIP),
`summary.json` (пошаговые ошибки, медиа, раскладки, build-провенанс),
`http-trace.json` (все `/api/v3` запросы UI), `server-correlate.txt`
(выдержка серверного лога по session id), 3 smoke-лога.

## Честная атрибуция ошибок

На шаге **открытия формы создания конференции** vendor-оболочка 2.2.1.66 даёт
6 transient vendor `TypeError` на каждую форму (поля `route`, `displayName`,
`length`, `mode`, `connectionsLimit`; точный список — в `summary.json → notes`). Это не запросы к нашему API: HTTP ≥400 за сценарий — 0, и на всех
остальных шагах (вызов, контролы, раскладка, reload, kick) ошибок нет.
Ошибки не скрыты: `summary.json → totals` даёт
`console_errors=12 = vendor_create_form_typeerrors=12`, `other_console_errors=0`.

## Скриншоты

- H.323: `01-h323-autocomplete`, `02-h323-address-selected` (Протокол H.323),
  `03-h323-connected`, `04-h323-controls`, `05-h323-layout`,
  `06-h323-layout-reload` (слот 1 после reload), `07-h323-kick-confirm`,
  `08-h323-kicked`.
- SIP: `07-sip-autocomplete`, `08-sip-address-selected` (Протокол SIP),
  `09-sip-connected`, `10-sip-controls`, `11-sip-layout`,
  `12-sip-layout-reload` (слот 1 после reload), `13-sip-kick-confirm`,
  `14-sip-kicked`.

## Воспроизведение

```bash
# 1) API/живой smoke (логи в этом каталоге обновляются вручную)
./scripts/smoke_v3.sh                                        # 24 проверки (API), 31 с живым вызовом
SMOKE_URI=h323:192.168.1.132 ./scripts/smoke_v3.sh           # 27 проверок
SMOKE_URI=sip:192.168.1.132  ./scripts/smoke_v3.sh           # 27 проверок

# 2) UI-evidence (нужны node+playwright+Chromium и свободный endpoint)
PLAYWRIGHT_MODULE=/path/to/node_modules/playwright \
CHROME_PATH=/path/to/chrome \
SERVER_LOG=vks_server.log \
node scripts/evidence/t16-evidence.js
```

`summary.json`, `http-trace.json` и PNG генерируются скриптом; smoke-логи
обновляются запуском смоука. Скрипт печатает итоговые totals и пишет
`git_dirty` по tracked-файлам, поэтому provenance проверяем.

## Ограничения

- endpoint держит только один протокол: между H.323 и SIP нужна пауза на
  освобождение (в прогонах — 15 с), иначе 486 Busy Here;
- live-часть смоука включается только через `SMOKE_URI`, в CI — без неё;
- UI-отключение участника требует подтверждения в диалоге (учитывается
  скриптом); smoke проверяет kick через API с read-back.

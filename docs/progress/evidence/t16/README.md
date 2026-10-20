# T1.6 — приёмка SIP+H.323 из UI, живые контролы, раскладка, smoke_v3

Дата: 2026-10-20. Стенд: release `vks-server` pid 547164, sha256 `971323a3…`
(исходники = HEAD `b78d0d9`, см. `summary.json → build`), UI `/pnm/`,
endpoint `192.168.1.132` (SIP 5060, H.323 1720; одновременно активен один протокол).

## Что доказано (оба протокола, из веб-UI)

| Шаг | H.323 | SIP |
|---|---|---|
| Создание конференции из UI, старт сессии | ✅ | ✅ |
| «Добавить участника по адресу» → автодополнение сохранённого адреса (`Зал 132 (h323/sip)`) | ✅ | ✅ |
| Вызов CONNECTED, живые медиа-счётчики | audio rx 0→74, tx 67→266 пакетов | audio rx 0→73, tx 58→254 |
| Видео H.264 640x360 в `details.video` | ✅ | ✅ |
| Контролы из UI: камера (rx off/on), микрофон (tx off/on) | ✅ | ✅ |
| Ручная раскладка: 4 видеоокна → «Заполнить видеоокна участниками» → «Применить» | ✅ слот 1 | ✅ слот 1 |
| Раскладка после reload страницы (слот/позиция сохранились) | ✅ position 1 | ✅ position 1 |
| Отключение участника из UI (call_end → подтверждение «Отключить») | ✅ | ✅ |
| Console errors на шагах вызова/контролов/раскладки | 0 | 0 |
| HTTP ≥400 за сценарий (кроме намеренных негативных тестов) | 0 | 0 |

Сессии прогона: `03d60982-cd54-4bdd-918e-54715b4f88d2` (H.323),
`16cdb288-feda-47d8-9543-1dac5aefbef3` (SIP); раскладки
`5e3d2916-6efc-4557-b034-7f06fa950f57` и `0d9ffae3-de0c-420d-a9d0-d734c71f76fe`.

Скриншоты (по 10): `01-h.323-*`…`04-h.323-layout-reload.png`,
`05-sip-*`…`08-sip-layout-reload.png`; полные данные — `summary.json`.

## Честная атрибуция ошибок

На шаге **создания конференции** vendor-оболочка 2.2.1.66 даёт 6 transient
`TypeError` на каждое открытие формы (`Proxy.get: route/displayName`, `Qv: length`).
Это не запросы к нашему API: HTTP ≥400 на шагах 0. Ошибки не скрыты — они
посчитаны в `summary.json → totals.vendor_create_form_typeerrors = 12`
(6 H.323 + 6 SIP), `other_console_errors = 0`.

## `scripts/smoke_v3.sh`

Bash+curl+jq смоук полной вертикали `/api/v3`: healthz → login → конференция →
сессия → адресная книга (create/search/patch/delete) → аудиоканал
(create/attach/list/detach) → раскладка (create/attach/abonents/detach) →
опциональный живой вызов `SMOKE_URI` (connected, рост RTP rx/tx, mute
камеры/микрофона, volume, kick) → stop/delete, всегда чистит за собой.

```bash
./scripts/smoke_v3.sh                                        # API-смоук, 18 проверок
SMOKE_URI=h323:192.168.1.132 ./scripts/smoke_v3.sh           # + живой H.323, 24 проверки
SMOKE_URI=sip:192.168.1.132  ./scripts/smoke_v3.sh           # + живой SIP, 24 проверки
```

Логи прогонов в этом каталоге: `smoke-api.log` (PASS 18),
`smoke-live-h323.log` (PASS 24, RTP rx 0→25 / tx 7→215),
`smoke-live-sip.log` (PASS 24, RTP rx 0→9 / tx 36→188).

## Ограничения / бэклог

- endpoint держит только один протокол: между H.323 и SIP нужна пауза на
  освобождение (в прогоне — 15 с; smoke не ретраит 486 Busy Here);
- live-ветка смоука рассчитана на доступный endpoint в локальной сети,
  в CI запускается без `SMOKE_URI`;
- UI-отключение участника требует подтверждения в диалоге — это учтено в
  evidence-скрипте, в smoke используется API.

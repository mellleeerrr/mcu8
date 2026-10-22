# T13 — подпись участника в плитке ×2 и счётчики комнаты

Дата: 2026-10-22. Артефакты: `docs/progress/evidence/e2e-label-zoom/`, харнесс `tests/e2e/label-zoom.js`.

## Что было

1. **Подпись в плитке раскладки была мелкой.** Вендор рисует имя под аватаром как
   `1em` от font-size плитки, а этот размер вендорный JS считает по площади слота
   (`LayoutGridConfigurator`: `fontSize = T < 90 ? (T < 60 ? "8px" : "10px") : "12px"`).
   Итог: 12px в раскладках 1..8 и **8px** в 9+ — в 25/34+ подпись нечитаема.
2. **Счётчики списка мониторинга показывали 0 при непустой комнате.**
   В `session_json` (`crates/vks-admin/src/api_v3/sessions.rs`) поле `contacts`
   собиралось из **живых** участников room-hub (`sip_count`/`h323_count` по
   `@sip-gateway`/`@h323-gateway`), а `users/rtsp/vnc/media/file` были константами 0.
   Приглашённые, но не подключённые контакты комнаты в счётчики не попадали.
   Тултипы вендора при этом прямо означают «приглашённые, но не участвующие»:
   «Участники (SIP/H.323)» = `contacts.users + contacts.h323 + contacts.sip`,
   «Устройства (VNC/RTSP) и медиафайлы» = `contacts.rtsp + contacts.vnc + contacts.media + contacts.file`.

## Что сделано

1. **CSS (фронт).** `client/pnm-frontend/deployed/pnm/index.html`, блок
   `<style id="prostor-pnm-overrides">`:
   `.layout-grid-slot .layout-grid-slot-dn__text { font-size: 2em; }`
   — подпись 24px (раскладки 1..8) и 16px (9+), ровно вдвое больше.
   Специфичность 0,2,0 — намеренно: Vite подключает CSS чанка 7 (layout-grid)
   **после** инлайн-блока, поэтому правило равной специфичности (0,1,0)
   проигрывало по порядку в документе (проверено: `ratio` оставался 1.00).
   Сдвиг центровки `-2.335em` (T12b) остался верным: подпись — нижний элемент
   flex-блока, её центр не зависит от высоты шрифта.
   Пересобрана строка `index.html` в `client/pnm-frontend/MANIFEST.sha256`
   (`sha256sum -c` — OK, 212/212).
2. **API (бэкенд).** `crates/vks-admin/src/api_v3/resources.rs` — общий хелпер
   классификации контактов: `ContactProtocol`, `contact_protocol()` (приоритет
   `contact_type`, затем схема URI), `uri_protocol()` (через `canonical_uri`),
   `ContactCounts` + `contact_counts()`.
   `crates/vks-admin/src/api_v3/sessions.rs` — `session_json` получает контакты
   комнаты и отдаёт `contacts` из них; одинаковые значения у списка и карточки
   конференций/сессий. `abonents` (живые участники) и `participants` не тронуты:
   для карточки сессии вендор читает именно `abonents`.
   Схема URI определяет протокол, а не `payload.call_string` (там перечислены все
   протоколы с `primary:true` — иначе SIP-контакт попал бы ещё и в «Устройства»).
3. **Харнесс.** `tests/e2e/label-zoom.js` — повторяемая проверка Playwright:
   10 размеров раскладки × (font-ratio 2.00, центровка, подпись внутри плитки,
   отсутствие переполнения) + сверка счётчиков UI ↔ API ↔ шапка карточки ↔
   `/contacts`. Проверен откат: с инжектом `font-size:1em !important` прогон
   падает (exit 1) на всех 10 размерах.

## Доказательства

`docs/progress/evidence/e2e-label-zoom/result.json` — `ok=true`, **102 проверки, 0 провалов**,
`failedFatalChecks: []`, `knownIssues: []`.

| размер | плитка | slot font | label font | подпись | dx | dy | внутри плитки | overflow |
|---|---|---|---|---|---|---|---|---|
| 1 | 887×499 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.01 | да | нет |
| 2 | 443×250 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.00 | да | нет |
| 3 | 443×249 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.01 | да | нет |
| 4 | 443×249 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.01 | да | нет |
| 5+ | 295×249 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.01 | да | нет |
| 8 | 221×249 | 12px | **24px** | 189.2×26.4 | 0.00 | −0.01 | да | нет |
| 9 | 295×166 | 8px | **16px** | 126.1×17.6 | −0.01 | −0.01 | да | нет |
| 10+ | 221×124 | 8px | **16px** | 126.1×17.6 | −0.01 | −0.00 | да | нет |
| 25 | 177×99 | 8px | **16px** | 126.1×17.6 | −0.01 | −0.00 | да | нет |
| 34+ | 147×83 | 8px | **16px** | 126.1×17.6 | −0.01 | −0.00 | да | нет |

`fontRatios`: `2.00` на всех десяти размерах.

Счётчики (живой стенд, комната `49b599d7-b5cd-45a7-8999-47a28c453a92`,
контакт «T12b Label Probe», SIP):

```
uiCounts      = {participants: 1, devices: 0}   (список мониторинга)
apiCounts     = {participants: 1, devices: 0}   (GET /api/v3/ui/conferences)
headerCounts  = {participants: 1, devices: 0}   (шапка карточки комнаты)
contacts      = {count: 1, byType: {call_strings: 1}, byProtocol: {sip: 1}}
match = true
```

Backend: `cargo test -p vks-admin --lib` — 286 passed / 0 failed;
`cargo clippy -p vks-admin --all-targets -- -D warnings` — чисто; `cargo fmt --check` — чисто.
Стенд перезапущен (`systemctl --user restart vks-mcu.service`), `/healthz` и `/readyz` — 200.

## Ограничения (осознанные)

- Ширина подписи в 34+ (147px плитка, 126px подпись) — запас ~10px; длинные имена
  уходят в вендорский `text-overflow: ellipsis`. Это ограничение вендорского
  `nowrap + ellipsis`, а не регресс.
- `contacts.users` останется 0: вендор зовёт `POST /api/v3/sessions/{id}/contacts/user`
  и `POST /api/v3/conferences/{id}/contacts`, которых в API нет (в логе — 501).
  Сам счётчик и его тест готовы; нужен отдельный flow добавления пользователя в комнату.
- `PATCH /api/v3/conferences/{id}/contacts/{contact_id}` отсутствует (501) —
  переименование контакта из UI не применяется. Это отдельный пробел API.
- Харнесс меряет подпись на клиентском черновике раскладки: «Создать» не
  нажимается, в БД ничего не пишется.

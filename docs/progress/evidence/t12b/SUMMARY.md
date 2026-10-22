# T12b — настройки кодеков без N+1, preview-мусор, центровка подписей

Дата: 2026-10-22. Артефакты: `artifacts/e2e-t12b6/`, `tests/e2e/perf-cache.js`.

## Что было
- Вендорский UI на каждую строку/резолв делал GET `/api/v3/settings/audio_codecs` и
  `/video_codecs`: 1550+1550 XHR за 79 с в E2E, 10 XHR на одну страницу.
- `GET /api/v3/abonents/previews` опрашивался постоянно, хотя endpoint всегда `{"previews":[]}`
  (кодировщика кадров нет); в меню участника был мёртвый чекбокс preview.
- Подпись `.layout-grid-slot-dn__text` была смещена вниз на `header/2 + avatar/2 + gap/2`:
  `+28 px` для 1/4/8, `+18.67 px` для 9/25/34+.
- Codec PATCH был get→mutate→set: параллельные audio/video PATCH теряли один список.

## Что сделано
1. `client/pnm-frontend/deployed/pnm/perf-overrides.js` (pre-bundle, same-origin):
   - XHR coalesce + 30 s cache для двух codec-GET; stats `window.__vksCodecCacheStats`;
   - PATCH: двухфазная инвалидация — abort старых GET, deferred waiters; cache clear,
     generation/epoch и replay только после успешного `loadend` PATCH;
   - stale/aborted owner не переиздаётся; waiters в состоянии OPENED переиздаются;
   - cross-tab инвалидация через `localStorage` epoch + `storage` event;
   - synthetic 200 `{"previews":[]}` для мёртвого previews-GET; same-origin guard.
2. `index.html`: скрипт подключён до бандла; CSS
   `.layout-grid-slot-content:has(.layout-grid-slot-dn){transform:translateY(-2.335em)}`;
   preview-checkbox скрыт.
3. Backend:
   - `SettingsService::try_update_async` / `try_set_async`: persist fallible, откат
     settings+mode, 503 вместо молчаливого 200; `settings_persist` сериализует persist;
   - `calls_patch` применяет legacy-поля к актуальным настройкам под lock'ом;
   - ETag (strong FNV-1a) + `Cache-Control: private, no-cache` + 304 для weak/list/`*`;
   - hydration настроек под `settings_persist`;
   - codec PATCH отвечает 503 при ошибке storage.
4. `MANIFEST.sha256` перегенерирован; `perf-overrides.js`/`api-errors.js` — 0644;
   `deploy/README.md` — `chmod -R a+rX` после `cp -a`.

## Доказательства
- `artifacts/e2e-t12b6/result.json`: `ok=true`, **232/232** проверок, 147 с.
  - label centered для всех диапазонов 1/2/3/4/5+/8/9/10+/25/34+, `|dx|,|dy| < 0.01 px`;
  - codec network = 2 на документ, всего 16 GET за journey, previews polling = 0;
  - preview toggle hidden; реальный SIP-вызов, two-way audio/video.
- `tests/e2e/perf-cache.js`: PATCH во время in-flight codec GET →
  coalesced GET завершился свежим ответом (`G722`, `fresh:true`), PATCH 200.
- Rust: `cargo test -p vks-admin --lib` — **281 passed**;
  `cargo clippy -p vks-admin --all-targets -- -D warnings` — clean; `cargo fmt --check` — clean;
  `sha256sum -c MANIFEST.sha256` — OK.
- Новые тесты: codec PATCH 503+rollback при отказе persist, persist round-trip,
  mode не перетирается codec PATCH, calls PATCH + codec PATCH без потерь,
  ETag 304 для strong/weak/list/`*`.

## Ограничения (осознанные)
- Реальных preview-кадров нет: endpoint всегда пуст; synthetic-путь и скрытие
  чекбокса — честное удаление мёртвой фичи, не имитация фичи.
- `:has` и `2.335em` рассчитаны на Chromium 105+ (целевой Playwright 1.64.1/Chromium 1243);
  для legacy-бандла подпись не центрируется — зафиксировано как P2.
- `calls_patch` пишет resources до settings: при отказе settings возможен частичный
  commit ресурсов (P2, требует транзакции).

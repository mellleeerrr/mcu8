# T04a evidence — узкий P0-smoke раскладок (live Playwright)

Дата: 2026-10-22. Задача: T04a, после T03 (`d8aecc3`).
Итоговый прогон: `VKS_ARTIFACTS=docs/progress/evidence/t04a/result.json node tests/e2e/live.js`
(артефакты `result.json`, `network.json`, `trace.zip`, PNG; в git не коммитятся,
`/artifacts/` в `.gitignore`). Промежуточные прогоны t04a-1..5 сохранены.

## Живой деплой

- Старый сервис PID 1242813 остановлен; старый бинарь сохранён в
  `/tmp/vks-server-pre-T03` (rollback), БД — `vks_data.db.before-t03-*`.
- Dedup на остановленной БД: 6 → 5 layouts (1 duplicate default), 1 alias
  перемаркирован; backup `vks_data.db.before-dedup-*`, `quick_check ok`.
- Новый release-бинарь (T03) запущен user-unit `vks-mcu.service` (PID 1500133,
  затем штатные рестарты для maintenance); `/readyz` 200, `/healthz` 200,
  `/pnm/` 200, media UDP 40000 слушается; второй экземпляр не запускался.

## Итоговый E2E

**exit 0, 98 checks / 0 failed, 109.9 s** (лимит T04a ≤3 мин), `page_errors: 0`,
5xx **0**, 501 **0**, unexpected 4xx **0**, unexpected console **0**.

- login → конференция → старт сессии → «Добавить участника по адресу» →
  SIP `sip:probe-e2e@192.168.1.132` → participant виден в сессии;
- ручная 2×2: 4 ячейки, participant в ячейке 1 (bbox top-left + текст);
  все **7 подвариантов 2×2** дают по 4 ячейки, набор preset'ов ровно 7
  (`preset_equal_4`, `selected_b/t/r/l_4`, `selected_b/t_4_(2)`), без дублей;
- **UI-посадка участника в ячейки 1, N/2, N для N=4,8,9,25**: для каждой из
  12 комбинаций проверены bbox целевой ячейки (>20×20 px) и текст
  participant'а; число ячеек ровно N (без off-by-one);
- calculate `preset_equal_4/8/9/25` — ровно 4/8/9/25 slots, позиции `1..N`
  (1-based);
- «Очистить все видеоокна» → PATCH: 4 slots, все `role=AUTO`;
- detach ручной: DELETE 204, id исчез из списка сессии, canonical default
  остался; после reload UI показывает «Общую», имени ручной раскладки нет
  (скриншот `08-detach-default.png` отличается от `07-clear-all-applied.png`);
- final-ассерты уже после закрытия страницы/cleanup: pageerror 0,
  unexpected 4xx 0, console 404 только ожидаемые stale-запросы detach;
- cleanup: сессия 204, созданный layout 204, комната 204; браузер закрывается
  до cleanup, поэтому вендорские TypeError не попадают в page_errors.
  Повторный прогон не увеличивает rooms/layouts (pre = post).

## Чистка накопленного мусора

- Удалены E2E-комнаты `779ffd6e…` (t04a-1) и `ba508078…` (baseline), их link'и
  и осиротевшие layout'ы (`a25a1cbd…`, `96785105…`, `d99209b7…`).
- Остановлен сервис, backup `vks_data.db.before-dangling-*`, удалены 3
  dangling-link key на несуществующие комнаты; сервис поднят, `/readyz` 200.
- Итог БД: **3 комнаты, 5 layouts, 0 dangling links**; canonical default
  `ec5a0a14…` сохранён, dedup-набор 5 layouts не тронут.
- Ограничения: UI-посадки — превью без PATCH (persist/движок покрыты T03 и
  calculate; полная матрица — T04b); при смене размера ячейка 1 может
  наследовать участника, поэтому «руками» проверены N/2 и N; на скриншоте
  detach видны два stale-тоста UI после API-detach — это ожидаемое следствие,
  pageerror/4xx = 0.

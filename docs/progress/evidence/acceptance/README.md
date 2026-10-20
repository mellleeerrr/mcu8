# Приёмка UI T1.3–T1.4 (живой H.323)

Дата: 2026-10-20. Стенд: release-сборка `vks-server` (pid 461301, собрана 19:05 из
текущего дерева; sha256 бинаря и git-база — в `summary.json → build`), `./scripts/run-local.sh restart`,
`http://127.0.0.1:8080/pnm/`, MCU 192.168.1.93, терминал 192.168.1.132 (H.323 1720).

Сессия приёмки: `a37ecc9c-f15f-4a40-a2e4-7d42ce33254b` (`Conf_505679476`),
участник `h323:192.168.1.132`, «Общая» раскладка `c438981c-07d4-49b2-ad62-9080bb15a3ef`
(auto/13 окон), индивидуальная `dc8d0f49-a5ad-4325-b9f5-da2cf0561955`
(manual/`preset_equal_25`/25 окон, 1 получатель).

## Сценарий (браузер, без API-обходов)

1. `admin` / `admin_password` → «Конференции» → «+». UI сам создаёт конференцию,
   «Общую» auto-раскладку, аудиоканал и привязывает их
   (`summary.json → ui_api_mutations`, шаг `create-conference`).
2. «Начать конференцию» → UI переходит в `/pnm/monitoring/sessions/{id}`
   (PATCH конференции + `POST /api/v3/sessions`).
3. «Добавить участника по адресу» → протокол `H.323` → `192.168.1.132` → «Добавить»
   (`02-add-address-modal.png`); участник connected (`h323:192.168.1.132`,
   `01-session-started.png`).
4. Меню участника («⋮») → «Настроить раскладку» → «Ручная настройка» →
   «Заполнить видеоокна участниками» → «Создать» (`03-layout-editor.png`,
   `04-layout-filled.png`, `05-layout-created.png`). URL становится
   `…/sessions/{id}/layouts/{lid}`.
   Запросы шага (`summary.json → ui_api_mutations`):
   `POST /api/v3/ui/video_layouts/calculate` (`preset_equal_25`),
   `POST /api/v3/ui/video_layouts` (manual/individual, 25 слотов, слот 1 — реальный
   `h323-…@h323-gateway`), `POST …/sessions/{id}/video_layouts/{lid}`,
   `POST …/video_layouts/{lid}/abonents/{h323-…}`.
   «Общая» раскладка остаётся auto — отдельных PATCH в прогоне нет.
5. Вкладка сессии «Раскладки» показывает обе раскладки и получателя
   (`06-session-layouts-tab.png`); после перезагрузки страницы участник и его
   индивидуальная раскладка сохраняются (`07-after-reload.png`).
6. `GET …/abonents`: `video_layout_id=dc8d0f49-…` (индивидуальная), медиа живое
   (`summary.json → media`): audio RX/TX PCMU ~62/62 кбит/с, video RX/TX H264 640x360
   (RX ~32 кбит/с, TX ~256 кбит/с). В логе:
   `H.323 CONNECT … audio_port=3230 video_port=Some(3232)` и
   `Outbound gateway call connected … has_video=true protocol="H.323"`.

## Честная атрибуция ошибок консоли

`summary.json → console_errors_attribution`:

- шаг создания конференции в vendor-форме: 6 transient `TypeError` в Proxy-геттерах
  (`reading 'route'/'displayName'/'mode'/'connectionsLimit'` у временно `null` сущности).
  Ни одного неуспешного HTTP-запроса (`fails=[]`), конференция создаётся корректно;
- старт сессии, добавление адреса, редактор раскладки, вкладка «Раскладки»,
  перезагрузка — **0 ошибок консоли и 0 неуспешных запросов** (`steps`).

## Файлы

- `summary.json` — id, build-провенанс (git/sha256 бинаря), медиастатистика,
  API-мутации, шаги, атрибуция ошибок.
- `01`…`07` — скриншоты шагов в хронологическом порядке (один прогон).

## Повторная проверка

```bash
cargo build --release -p vks-server && ./scripts/run-local.sh restart
# /pnm/monitoring/sessions/{id}, admin/admin_password
# терминал 192.168.1.132: SIP 5060 и H.323 1720 доступны, одновременно — только один протокол
```

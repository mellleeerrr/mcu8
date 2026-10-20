# T1.5 — «Добавить по адресу»: автодополнение, адресная книга, аудиоканалы

Дата: 2026-10-20. Стенд: release-сборка `vks-server` (pid 547164, sha256 `971323a3…`;
head/сборка — в `summary.json → build`), UI `/pnm/`, endpoint `192.168.1.132` (H.323 1720).

## Что доказано

1. **Сохранённый адрес в адресной книге.** `POST /api/v3/call_strings` с
   `{"uri":"h323:192.168.1.132","display_name":"Зал 132 (h323)","payload":{"h323":{"bitrate":4096}}}`
   → 201 `{call_string:{id,uri,display_name,busy,payload,contact}}`; запись переживает
   рестарт (configuration document; толерантная десериализация чинит/чистит битые записи).
2. **Автодополнение.** В модалке «Добавить участника по адресу» ввод `192.168` шлёт
   `GET /api/v3/ui/call_strings?page=1&page_size=5&sort_field=created_at&sort_order=1&uri=192.168&is_binded=true`
   и показывает «Зал 132 (h323) / h323:192.168.1.132» (`10-address-autocomplete.png`).
   Клик по подсказке заполняет протокол H.323, адрес и отображаемое имя
   (`11-address-selected.png`).
3. **Реальный вызов по выбранному адресу.** После выбора кодеков (payload адреса создан
   без кодеков; форма требует хотя бы один) «Добавить» → `POST /ui/sessions/{id}/call_strings`
   → абонент CONNECTED: PCMU rx/tx ~203/385 пакетов, H.264 640x360 rx/tx ~64/404
   (`12-address-call-connected.png`, `summary.json → connected`).
4. **Аудиоканалы сессии.** Вкладка «Аудиоканалы»: список `{count,audiochannels}`, «Добавить»
   → `POST /api/v3/audiochannels` (201) → `POST /api/v3/sessions/{id}/audiochannels/{id}` (204),
   после перезагрузки канал виден в таблице (`08-audiochannel-created.png`,
   `09-audiochannel-tab.png`). Счётчики `speakers/listeners` считаются из payload
   (в прогоне канал пустой — 0/0; ненулевые значения покрыты юнит-тестом).
5. **CRUD адресов** (`PATCH`/`DELETE`, запись — Operator без room-scope, чтение —
   Moderator+, 201/400/403/404) и **WS** `ui:call_strings:search` используют тот же
   источник/фильтры/дефолты (`page_size=100`) и `AuthContext` (`room_scope`), что
   HTTP-список;
   юнит-тесты: `address_book_crud_filters_and_persists`,
   `pnm_audiochannels_list_filters_and_counts_speakers_listeners`,
   `pnm_normalize_keeps_audiochannel_links_and_drops_dangling`.

## Честные ограничения

- 6 transient vendor console TypeErrors на шаге открытия формы создания конференции
  (Proxy-getter по временно null entity: `route`/`displayName`/`mode`/`connectionsLimit`);
  HTTP>=400 нет, шаги сессии/раскладок/адреса/аудио — 0 ошибок.
- Адресная книга пользователя (`contact_id`, `/api/v3/ui/users/{id}/call_strings`,
  вкладка Addresses карточки пользователя) — T2.1; сейчас книга глобальная, `is_binded`
  игнорируется.
- `busy` в выдаче всегда false, дубли адресов не отклоняются (P3-бэклог).
- `page`/`page_size` — saturating-арифметика; битые persisted payload протоколов
  ремонтируются на записи (PATCH не паникует).
- Vendor не имеет `/pnm/addresses` и `/api/v3/ui/addresses`; `/ui/sessions/{id}/audiochannels`
  у нас — алиас (vendor зовёт без `/ui`), добавлен для полноты контракта.

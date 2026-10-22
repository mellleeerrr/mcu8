# T12a — подписи без emoji/стикеров

Дата: 2026-10-22.

## Что сделано

- `vks-common::text::sanitize_label` — единый denylist-санитайзер:
  удаляет emoji/пиктограммы, региональные флаги, ZWJ/VS16/skin-tone/tag-
  последовательности, keycap-базы (`1️⃣` → пусто), управляющие и bidi/format-
  символы; **сохраняет** буквы любого алфавита, combining marks (NFD-кириллица),
  цифры и типографскую пунктуацию (`— «» № % & ! ?`); схлопывает пробелы,
  обрезает до 128 символов без хвостового пробела и без разрыва combining-кластера.
- `sanitize_label_or(raw, fallback)` — пустое/всё-emoji имя заменяется fallback.
- `resources::sanitize_layout_label` делегирует в общий helper (раньше был
  локальный allowlist, резавший `—`, `«»`, `%`).
- `call_strings`: санитизируется только **явно переданный** `display_name`
  на create/update/PATCH (`address_fields`, `create_call_string`,
  `create_conference_call_string`, `create_session_call_string*`).
  `uri`, `raw_uri` и fallback-адрес не изменяются; slot/participant identity
  и history/audit не перезаписываются.
- Тесты: emoji/ZWJ/флаг/skin-tone/keycap/tag, кириллица+combining, типографика,
  идемпотентность, лимит 128, fallback, create+update API, URI не изменён.

## Живая проверка

```text
POST /api/v3/call_strings {"uri":"sip:10.77.77.77","display_name":"  Тест 🎤  "}
→ display_name="Тест", uri="sip:10.77.77.77", id стабилен; DELETE 204
```

## Гейты

```text
cargo test --workspace --all-targets --no-fail-fast
1058 passed / 0 failed / 6 ignored
cargo clippy --workspace --all-targets -- -D warnings   clean
cargo fmt --all -- --check                              clean
```

## Осталось (T12b)

- UI-центрирование подписи в плитке — CSS вендорского бандла, отдельный
  визуальный тест.
- Previews (`/api/v3/abonents/previews` = 200 пустой) и лишние codec-запросы
  при навигации.

# T2.1 — справочник PNM: пользователи, группы, роли, учётные записи, личные адреса

Дата: 2026-10-21. Evidence UI-прогона на vendor-оболочке 2.2.1.66 против
нашего `/api/v3`. Точные `git_head`, `git_dirty`, `binary_sha256` и
`server_pid` — в `summary.json → build` (единственный источник истины; прогон
сделан на чистом дереве, evidence-коммит — docs-only после него).
Артефакты: `summary.json`, 9 PNG (`01`–`09`), самодостаточный скрипт
`scripts/evidence/t21-evidence.js`.

## Что доказано (через веб-UI и те же vendor-эндпоинты, что вызывает UI)

| Шаг | Проверка | Результат |
|---|---|---|
| `users-list` | WS `ui:users:search`: у каждого элемента есть `id` (без него карточка не открывалась) | ✅ |
| `user-create` | «Справочник → Пользователи → +»: пользователь появляется в списке/API | ✅ |
| `user-card` | карточка с вкладками «Информация / Профиль / Адрес / Учётная запись»; кнопка «Удалить» активна для обычного пользователя | ✅ |
| `credentials` | вкладка «Учётная запись»: логин/пароль → `GET …/credentials`, реальный `POST /api/v3/login` = 200 | ✅ |
| `user-address` | «Адрес → Добавить»: адрес в личной книге (`GET /users/{id}/call_strings`) | ✅ |
| `user-address-edit` | vendor-PATCH `/ui/users/{u}/call_strings/{a}`: смена `uri`/`display_name` сохраняется | ✅ |
| `group-create` | режим «Группы → +»: группа создана и находится | ✅ |
| `group-search` | поиск тем же параметром, что шлёт UI — `?display_name=` (не только `search`) | ✅ |
| `group-member` | «Добавить» участника: пользователь в `user.groups` | ✅ |
| `role-create-assign` | `POST /roles`, `POST /users/{u}/roles/{r}`, роль видна в карточке, `DELETE /roles/{r}` | ✅ |
| `self-password` | свой пароль vendor-телом `{login:null,...}`: старый пароль → 401, новый → 200 | ✅ |
| `reload` | после перезагрузки SPA пользователь, группа и адрес на месте | ✅ |
| `user-delete-ui` | кнопка «Удалить» в UI: пользователь исчезает, `GET /users/{id}` → 404 | ✅ |
| `cleanup` | группа удалена и отсутствует в поиске; created-сущности не остаются | ✅ |

`summary.json → ok=true` — строгий гейт: все 16 acceptance-флагов,
0 HTTP ≥ 400 (кроме двух ожидаемых credentials-404, см. ниже),
0 неатрибутированных console errors; при провале exit 1.

## Честная атрибуция

- `expected_credentials_404=2` — ровно два РЕАЛЬНЫХ запроса vendor-UI
  `GET /api/v3/users/{id}/credentials`, которым положен 404 (пустое
  состояние «учётной записи нет»). Считаются один раз, по response.
- `vendor_console_errors=1` — точный allowlist известного дефекта бандла:
  `TypeError: this.validationHandler(...).then(...).then.catch is not a
  function` в диалоге создания группы. Группа создаётся (201 + уведомление),
  к нашему API отношения не имеет; полный sample — в
  `summary.json → vendor_error_samples`. Любая другая console-ошибка
  (включая CSP) попадает в `other_console_errors` и валит гейт.
- **CSP `'unsafe-eval'`** — вынужденная уступка неизменяемым бандлам 2.2.1.66:
  без неё `index2.bundle` при открытии карточки/удалении исполняет строку как
  JavaScript, и Chromium пишет
  `Evaluating a string as JavaScript violates the following Content Security
  Policy directive because 'unsafe-eval' is not an allowed source of script`.
  Это зафиксировано прогоном (source: `/mcu/chunks/index2.bundle-2.2.1.66.js`);
  статический `grep` по `eval(`/`new Function(` этот runtime-путь не видит.
  Vendor-сервер `.92` CSP не отдаёт вовсе. `'unsafe-inline'` в `script-src`
  не добавлялся.

## Что покрыто кодом/тестами вне UI-прогона

`crates/vks-admin/src/api_v3/directory_tests.rs` (18 тестов) дополнительно
проверяет: каскадное удаление (credentials/профиль/членства/адреса, login
удалённого → 401), флаги `defaults.deletable`, `room_scope` не читает
глобальный справочник, WS у Moderator, CRUD ролей, сохранение неявной роли
Administrator при добавлении Operator, переименование логина, Unicode-дубли и
лимиты длин, malformed-JSON-конверт, гидратацию справочника из
configuration-document при рестарте.

## Воспроизведение

```bash
PLAYWRIGHT_MODULE=/path/to/node_modules/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t21-evidence.js
```

Скрипт создаёт уникальные сущности по timestamp, проверяет их через тот же
API, которым пользуется UI, и удаляет созданное (пользователь — кнопкой UI,
группа — API, роль — в своём шаге).

# T2.1 — справочник PNM: пользователи, группы, роли, учётные записи, личные адреса

Дата: 2026-10-20. Evidence UI-прогона на vendor-оболочке 2.2.1.66 против
нашего `/api/v3`. Точные `git_head`, `git_dirty`, `binary_sha256` и
`server_pid` — в `summary.json → build` (единственный источник истины).
Артефакты: `summary.json`, 8 PNG (`01`–`08`), самодостаточный скрипт
`scripts/evidence/t21-evidence.js`.

## Что доказано (через веб-UI)

| Шаг | Проверка | Результат |
|---|---|---|
| `users-list` | WS `ui:users:search`: список с `Administrator`, у каждого элемента есть `id` | ✅ |
| `user-create` | «Справочник → Пользователи → +»: создание пользователя, появление в списке и в API | ✅ |
| `user-card` | карточка с вкладками «Информация / Профиль / Адрес / Учётная запись» | ✅ |
| `credentials` | вкладка «Учётная запись»: логин/пароль/подтверждение → `GET …/credentials` = логин, реальный `POST /api/v3/login` новым логином = 200 | ✅ |
| `user-address` | «Адрес → Добавить»: `sip:` адрес в личной книге (`GET /users/{id}/call_strings` count=1) | ✅ |
| `group-create` | режим «Группы → +»: создание группы, `GET /groups?search=` находит | ✅ |
| `group-member` | «Добавить» участника в группе: пользователь в `user.groups` | ✅ |
| `reload` | после перезагрузки страницы созданный пользователь на месте (configuration-document) | ✅ |
| `cleanup` | удаление пользователя/группы; read-back `404`/`count=0` | ✅ |

`summary.json → ok=true` — строгий гейт: все 11 acceptance-флагов,
0 HTTP ≥ 400 и 0 неатрибутированных console errors; при провале exit 1.

## Честная атрибуция

- `expected_credentials_404=4`: vendor-UI проверяет наличие учётной записи
  через `GET /api/v3/users/{id}/credentials`; `404` — пустое состояние.
- `vendor_console_errors=1`: известный дефект vendor-бандла
  `this.validationHandler(...).then(...).then.catch is not a function` в
  диалоге создания группы. Группа при этом создаётся (201 + уведомление),
  к нашему API отношения не имеет; ошибка не скрыта и посчитана отдельно.
- CSP: для неизменяемых vendor-бандлов в `script-src` добавлен
  `'unsafe-eval'` (runtime-компиляция шаблона карточки пользователя);
  vendor-сервер `.92` CSP не отдаёт вовсе, `'unsafe-inline'` не добавлялся.

## Воспроизведение

```bash
PLAYWRIGHT_MODULE=/path/to/node_modules/playwright \
BASE_URL=http://127.0.0.1:8080 \
node scripts/evidence/t21-evidence.js
```

Скрипт создаёт уникальные сущности по timestamp, проверяет их через тот же
API, которым пользуется UI, и удаляет в `cleanup`.

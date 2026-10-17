# vks-auth

Аутентификация VKS: JWT, API-ключи, LDAP, встроенная БД юзеров и адаптер
`AuthProvider`. АDR-0009 (JWT/API/LDAP), ADR-0022 (RBAC).

> Фаза 5 (`docs/DEVELOPMENT_PLAN.md`). Адаптер [`provider`] — самодостаточный
> двойник порта `vks-core::AuthProvider`, чтобы крейт собирался без импорта
> `vks-core`.

## Модули

| Модуль | Назначение |
|---|---|
| [`jwt`] | Выпуск + верификация JWT (HS256, секрет из конфига). Claims: `sub`, `jti`, `room_id?`, `exp`, `iat`. Ошибки показывают только `jti`. |
| [`api_key`] | API-ключи `vks_` + 32-hex; в БД только SHA-256; наружу — `prefix` (первые 8). |
| [`ldap`] | LDAP bind через mockable-порт `LdapClient`; строит bind DN из шаблона. |
| [`local_db`] | Встроенная (default) БД юзеров: id + PBKDF2-SHA256-хеш + salt. |
| [`provider`] | Стратегия `AuthProvider` (Local/Ldap/Oidc) + `AuthenticatedUser`. |

## Стратегии провайдеров

- **Local** (default): в-memory хранилище юзеров, PBKDF2-SHA256. `verify` принимает
  `username:password`.
- **Ldap**: bind по DN из шаблона `{user}`; транспорт — за `LdapClient` (в продакшене
  ldap3 + GSSAPI/Kerberos, timeout 5s, circuit breaker по BACKEND_RULES §4.5).
- **Oidc**: stub, возвращает `Err(NotImplemented)` (реальный discovery + PKCE — позже).

## RBAC (ADR-0022)

Роль пользователя определяет права (см. полную матрицу в BACKEND_RULES §1.7):

| Роль | Ключевые права |
|---|---|
| **Operator** | Все: комнаты, глобальное MCU/SFU, пользователи, API-ключи, аудит. |
| **Moderator** | Комнаты, режим комнаты, mute/remove, layout, аудит, запись (confirm). |
| **Participant** | Publishing, subscribing, screen sharing, chat, запись (confirm). |
| **Viewer** | Только subscribing (наблюдение), запись (confirm). |

## Секреты (BACKEND_RULES §6.3)

- Переменные окружения `VKS_*`, никогда в config 644 и в git.
- JWT-секрет: `VKS_JWT_SECRET` (env). В логах — только `jti`, никогда весь токен.
- API-ключ: в БД только SHA-256; в логах — только `prefix` (первые 8 символов).

## Зависимости

`jsonwebtoken`, `sha2`, `pbkdf2`, `subtle`, `rand`, `async-trait` + workspace
(`uuid`, `ulid`, `thiserror`, `serde`).

## Тесты

```
cargo test -p vks-auth
```

Покрытие: JWT roundtrip / expired / wrong-secret / jti-in-error; API-key формат,
prefix, verify; local_db register+authenticate; provider Local end-to-end + OIDC stub.

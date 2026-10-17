# ADR-0022: RBAC-матрица ролей (Operator/Moderator/Participant/Viewer)

- **Статус:** accepted
- **Дата:** 2026-10-13 (ретроспективная запись файла — 2026-10-17; до этого решение
  жило строкой в `DECISIONS.md` + канонической таблицей §1.7)
- **Связанные ADR:** 0009 (fail-closed JWT), 0026 (MCU/SFU — только Operator), 0027 (legacy UI)
- **Каноническая матрица:** `docs/rules/BACKEND_RULES.md` §1.7 — источник истины,
  включая примечание T1.1 про запись в audit-log

## Контекст

VKS — многоролевая ВКС-система. Требуется единая матрица «действие × роль» для REST-API,
сигнализации и UI, чтобы проверки прав были непротиворечивы, доказуемы и fail-closed.

## Решение

1. Роли упорядочены: `Operator > Moderator > Participant > Viewer`
   (`vks_core::Role::level`); проверка — `role.level() >= required.level()`.
2. Каноническая матрица действий — BACKEND_RULES §1.7. Ключевые строки:
   - комнаты: создание/удаление/режим комнаты — Operator+Moderator;
   - глобальный MCU/SFU, управление пользователями, API-ключи — Operator only;
   - mute/kick **участников сессии** — Operator+Moderator;
   - audit-log: запись — Operator; чтение/экспорт — Operator+Moderator;
   - publishing/screen sharing/чат — Operator..Participant; subscribing/запись — все роли.
3. Реализация v1-поверхности — `crates/vks-admin/src/auth_middleware.rs::v1_required_role`
   с табличным тестом матрицы; **неизвестный роут → Operator** (fail-closed дефолт).
4. Роль резолвится из admin-хранилища на каждый запрос (`Users::resolve_role`), а не из
   токена: soft-delete и смена роли действуют немедленно; неизвестный/удалённый субъект — 403.

## Последствия

- Все новые v1-роуты обязаны иметь строку в §1.7 и тест в таблице `v1_required_role_matches_matrix`.
- Строки про live-модерацию не распространяются на bulk-операции над учётными записями:
  `POST /api/v1/users/bulk` (mute/kick = состояние `UserRecord`, kick = soft-delete) — Operator only.
- v3-поверхность (`/api/v3/*`) — контракт legacy UI (ADR-0027); её RBAC — зона владельца
  токена сессии, детали в `api_v3`-тестах; полный per-role гейт — долг T3.

// Участники и RBAC.
//
// `Participant` — участник комнаты. `Role` — роль (ADR-0022).
// `Permissions` — проверка прав «роль → действие» (RBAC matrix).
//
// RBAC matrix (ADR-0022): Operator > Moderator > Participant > Viewer.
// Ядро предоставляет только проверки прав; конкретное применение — в
// vks-signaling / vks-admin (фазы 2, 5).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Идентикатор участника (UUIDv4/v7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParticipantId(Uuid);

impl PartialOrd for ParticipantId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ParticipantId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl ParticipantId {
    /// Новый глобально уникальный идентификатор участника.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Создаёт идентификатор из существующего [`Uuid`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Возвращает внутренний [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for ParticipantId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for ParticipantId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Идентикатор пользователя (identity, до роли в комнате).
///
/// Один пользователь может быть участником нескольких комнат с
/// различными ролями.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(Uuid);

impl PartialOrd for UserId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for UserId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl UserId {
    /// Новый идентификатор пользователя.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Создаёт идентификатор из существующего [`Uuid`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Возвращает внутренний [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for UserId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Роль участника (ADR-0022, RBAC matrix).
///
/// Порядок убывания полномочий: `Operator` > `Moderator` >
/// `Participant` > `Viewer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Полное управление сервером и всеми комнатами.
    Operator,
    /// Управление конкретной комнатой (moderation).
    Moderator,
    /// Обычный участник (публикует и подписывается).
    Participant,
    /// Только наблюдение (подписка, без публикации).
    Viewer,
}

impl Role {
    /// Все роли в порядке убывания полномочий.
    pub const ALL: [Role; 4] = [Role::Operator, Role::Moderator, Role::Participant, Role::Viewer];

    /// Имя роли (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Operator => "operator",
            Role::Moderator => "moderator",
            Role::Participant => "participant",
            Role::Viewer => "viewer",
        }
    }

    /// Иерархический уровень (для сравнения «не ниже роли»).
    ///
    /// Больше значение — выше полномочия.
    pub fn level(self) -> u8 {
        match self {
            Role::Operator => 3,
            Role::Moderator => 2,
            Role::Participant => 1,
            Role::Viewer => 0,
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Действия, над которыми проверяются права (RBAC matrix, ADR-0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// Присоединиться к комнате.
    JoinRoom,
    /// Публиковать медиатреки.
    Publish,
    /// Подписываться на медиатреки.
    Subscribe,
    /// Изменять раскладку комнаты.
    ChangeLayout,
    /// Мьютить участников.
    MuteParticipants,
    /// Удалять участников (kick).
    RemoveParticipants,
    /// Повышать/понижать роли.
    ManageRoles,
    /// Начать/остановить запись.
    ManageRecording,
    /// Завершить комнату для всех (end for all).
    EndForAll,
    /// Переключать режим MCU/SFU (per-room override).
    SwitchMode,
    /// Глобальное управление сервером (admin).
    ManageServer,
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Permission::JoinRoom => "join_room",
            Permission::Publish => "publish",
            Permission::Subscribe => "subscribe",
            Permission::ChangeLayout => "change_layout",
            Permission::MuteParticipants => "mute_participants",
            Permission::RemoveParticipants => "remove_participants",
            Permission::ManageRoles => "manage_roles",
            Permission::ManageRecording => "manage_recording",
            Permission::EndForAll => "end_for_all",
            Permission::SwitchMode => "switch_mode",
            Permission::ManageServer => "manage_server",
        };
        f.write_str(s)
    }
}

/// Проверка прав «роль → действие» (RBAC matrix, ADR-0022).
///
/// Матрица зафиксирована:
///
/// | Permission            | Viewer | Participant | Moderator | Operator |
/// |-----------------------|:------:|:-----------:|:---------:|:--------:|
/// | JoinRoom              |    ✓   |      ✓      |     ✓     |    ✓     |
/// | Subscribe             |    ✓   |      ✓      |     ✓     |    ✓     |
/// | Publish               |    —   |      ✓      |     ✓     |    ✓     |
/// | ChangeLayout          |    —   |      —      |     ✓     |    ✓     |
/// | MuteParticipants      |    —   |      —      |     ✓     |    ✓     |
/// | RemoveParticipants    |    —   |      —      |     ✓     |    ✓     |
/// | ManageRoles           |    —   |      —      |     ✓     |    ✓     |
/// | ManageRecording       |    —   |      —      |     ✓     |    ✓     |
/// | EndForAll             |    —   |      —      |     ✓     |    ✓     |
/// | SwitchMode            |    —   |      —      |     —     |    ✓     |
/// | ManageServer          |    —   |      —      |     —     |    ✓     |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions;

impl Permissions {
    /// Проверяет, есть ли у роли указанное право.
    pub fn has(role: Role, perm: Permission) -> bool {
        match (role, perm) {
            (Role::Operator, _) => true,
            (Role::Moderator, p) => matches!(
                p,
                Permission::JoinRoom
                    | Permission::Subscribe
                    | Permission::Publish
                    | Permission::ChangeLayout
                    | Permission::MuteParticipants
                    | Permission::RemoveParticipants
                    | Permission::ManageRoles
                    | Permission::ManageRecording
                    | Permission::EndForAll
            ),
            (Role::Participant, p) => matches!(p, Permission::JoinRoom | Permission::Subscribe | Permission::Publish),
            (Role::Viewer, p) => matches!(p, Permission::JoinRoom | Permission::Subscribe),
        }
    }

    /// Проверяет, что роль не ниже `min` (по уровню полномочий).
    pub fn at_least(role: Role, min: Role) -> bool {
        role.level() >= min.level()
    }
}

/// Участник комнаты.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    /// Идентикатор участника (unique in room).
    pub id: ParticipantId,
    /// Идентикатор пользователя (identity).
    pub user_id: UserId,
    /// Отображаемое имя.
    pub display_name: String,
    /// Роль в комнате (ADR-0022).
    pub role: Role,
}

impl Participant {
    /// Создаёт участника с указанным именем и ролью.
    pub fn new(user_id: UserId, display_name: impl Into<String>, role: Role) -> Self {
        Self { id: ParticipantId::new(), user_id, display_name: display_name.into(), role }
    }

    /// Проверяет, есть ли у участника указанное право.
    pub fn has_permission(&self, perm: Permission) -> bool {
        Permissions::has(self.role, perm)
    }

    /// Проверяет, что роль участника не ниже `min`.
    pub fn role_at_least(&self, min: Role) -> bool {
        Permissions::at_least(self.role, min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn participant(role: Role) -> Participant {
        Participant::new(UserId::new(), "test", role)
    }

    #[test]
    fn operator_has_all_permissions() {
        for perm in [
            Permission::JoinRoom,
            Permission::Publish,
            Permission::Subscribe,
            Permission::ChangeLayout,
            Permission::MuteParticipants,
            Permission::RemoveParticipants,
            Permission::ManageRoles,
            Permission::ManageRecording,
            Permission::EndForAll,
            Permission::SwitchMode,
            Permission::ManageServer,
        ] {
            assert!(Permissions::has(Role::Operator, perm), "Operator should have {perm}");
        }
    }

    #[test]
    fn moderator_permissions() {
        // moderator: everything except SwitchMode, ManageServer
        assert!(Permissions::has(Role::Moderator, Permission::JoinRoom));
        assert!(Permissions::has(Role::Moderator, Permission::Publish));
        assert!(Permissions::has(Role::Moderator, Permission::Subscribe));
        assert!(Permissions::has(Role::Moderator, Permission::ChangeLayout));
        assert!(Permissions::has(Role::Moderator, Permission::MuteParticipants));
        assert!(Permissions::has(Role::Moderator, Permission::RemoveParticipants));
        assert!(Permissions::has(Role::Moderator, Permission::ManageRoles));
        assert!(Permissions::has(Role::Moderator, Permission::ManageRecording));
        assert!(Permissions::has(Role::Moderator, Permission::EndForAll));
        assert!(!Permissions::has(Role::Moderator, Permission::SwitchMode));
        assert!(!Permissions::has(Role::Moderator, Permission::ManageServer));
    }

    #[test]
    fn participant_permissions() {
        assert!(Permissions::has(Role::Participant, Permission::JoinRoom));
        assert!(Permissions::has(Role::Participant, Permission::Publish));
        assert!(Permissions::has(Role::Participant, Permission::Subscribe));
        assert!(!Permissions::has(Role::Participant, Permission::ChangeLayout));
        assert!(!Permissions::has(Role::Participant, Permission::MuteParticipants));
        assert!(!Permissions::has(Role::Participant, Permission::RemoveParticipants));
        assert!(!Permissions::has(Role::Participant, Permission::ManageRoles));
        assert!(!Permissions::has(Role::Participant, Permission::ManageRecording));
        assert!(!Permissions::has(Role::Participant, Permission::EndForAll));
        assert!(!Permissions::has(Role::Participant, Permission::SwitchMode));
        assert!(!Permissions::has(Role::Participant, Permission::ManageServer));
    }

    #[test]
    fn viewer_permissions() {
        assert!(Permissions::has(Role::Viewer, Permission::JoinRoom));
        assert!(Permissions::has(Role::Viewer, Permission::Subscribe));
        assert!(!Permissions::has(Role::Viewer, Permission::Publish));
        assert!(!Permissions::has(Role::Viewer, Permission::ChangeLayout));
        assert!(!Permissions::has(Role::Viewer, Permission::MuteParticipants));
        assert!(!Permissions::has(Role::Viewer, Permission::RemoveParticipants));
        assert!(!Permissions::has(Role::Viewer, Permission::ManageRoles));
        assert!(!Permissions::has(Role::Viewer, Permission::ManageRecording));
        assert!(!Permissions::has(Role::Viewer, Permission::EndForAll));
        assert!(!Permissions::has(Role::Viewer, Permission::SwitchMode));
        assert!(!Permissions::has(Role::Viewer, Permission::ManageServer));
    }

    #[test]
    fn at_least_hierarchy() {
        assert!(Permissions::at_least(Role::Operator, Role::Moderator));
        assert!(Permissions::at_least(Role::Moderator, Role::Participant));
        assert!(Permissions::at_least(Role::Participant, Role::Viewer));
        assert!(Permissions::at_least(Role::Viewer, Role::Viewer));
        assert!(!Permissions::at_least(Role::Viewer, Role::Participant));
        assert!(!Permissions::at_least(Role::Participant, Role::Moderator));
        assert!(!Permissions::at_least(Role::Moderator, Role::Operator));
        assert!(!Permissions::at_least(Role::Viewer, Role::Operator));
    }

    #[test]
    fn participant_method_delegates() {
        let p = participant(Role::Participant);
        assert!(p.has_permission(Permission::Publish));
        assert!(!p.has_permission(Permission::ChangeLayout));
        assert!(p.role_at_least(Role::Viewer));
        assert!(!p.role_at_least(Role::Moderator));
    }

    #[test]
    fn role_display() {
        assert_eq!(Role::Operator.to_string(), "operator");
        assert_eq!(Role::Moderator.to_string(), "moderator");
        assert_eq!(Role::Participant.to_string(), "participant");
        assert_eq!(Role::Viewer.to_string(), "viewer");
    }

    #[test]
    fn permission_display() {
        assert_eq!(Permission::Publish.to_string(), "publish");
        assert_eq!(Permission::SwitchMode.to_string(), "switch_mode");
        assert_eq!(Permission::ManageServer.to_string(), "manage_server");
    }

    #[test]
    fn participant_id_roundtrip() {
        let id = ParticipantId::new();
        let s = id.to_string();
        let parsed: ParticipantId = s.parse().expect("parse");
        assert_eq!(id, parsed);
    }
}

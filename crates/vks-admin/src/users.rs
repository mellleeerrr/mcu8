// Users — CRUD пользователей + bulk actions + LDAP sync (mock).
//
// RBAC (ADR-0022): управление пользователями — Operator only.
// Real LDAP (ldap3 + GSSAPI) живёт в vks-auth (фаза 5); здесь —
// mock-провайдер, возвращающий `Vec<vks_core::User>`.

use vks_core::{Role, User, UserId};

use crate::state::{AppState, UserRecord};

/// Операции над пользователями.
pub struct Users;

impl Users {
    /// Создаёт пользователя (role из ADR-0022).
    pub fn create(state: &AppState, username: impl Into<String>, role: Role) -> UserRecord {
        let id = UserId::new();
        let record = UserRecord { id, username: username.into(), role, muted: false, deleted: false };
        let mut users = state.users.lock().expect("users store poisoned");
        users.insert(id, record.clone());
        record
    }

    /// Возвращает запись пользователя.
    pub fn get(state: &AppState, id: &UserId) -> Option<UserRecord> {
        state.users.lock().expect("users store poisoned").get(id).cloned()
    }

    /// Список активных (не soft-deleted) пользователей.
    pub fn list(state: &AppState) -> Vec<UserRecord> {
        state.users.lock().expect("users store poisoned").values().filter(|u| !u.deleted).cloned().collect()
    }

    /// Обновляет username/role.
    pub fn update(state: &AppState, id: &UserId, username: Option<String>, role: Option<Role>) -> Option<UserRecord> {
        let mut users = state.users.lock().expect("users store poisoned");
        let record = users.get_mut(id)?;
        if let Some(name) = username {
            record.username = name;
        }
        if let Some(r) = role {
            record.role = r;
        }
        Some(record.clone())
    }

    /// Soft-delete (GDPR 152-7: 1000d через 30 — в vks-storage).
    ///
    /// Возвращает `true`, если запись существовала и НЕ была уже удалена.
    pub fn delete(state: &AppState, id: &UserId) -> bool {
        let mut users = state.users.lock().expect("users store poisoned");
        match users.get_mut(id) {
            Some(record) if !record.deleted => {
                record.deleted = true;
                true
            }
            _ => false,
        }
    }

    /// Bulk: mute/unmute по списку ids (состояние `UserRecord`).
    ///
    /// RBAC: Operator — зона «Управление пользователями» (ADR-0022; энфорсится
    /// `v1_required_role`). Строка §1.7 «Мутить/удалить участника» — про
    /// live-модерацию сессий, не про записи пользователей.
    /// Возвращает число затронутых записей.
    pub fn bulk_mute(state: &AppState, ids: &[UserId], muted: bool) -> usize {
        let mut users = state.users.lock().expect("users store poisoned");
        let mut touched = 0;
        for id in ids {
            if let Some(record) = users.get_mut(id) {
                record.muted = muted;
                touched += 1;
            }
        }
        touched
    }

    /// Bulk: kick (soft-delete) по списку ids.
    ///
    /// RBAC: Operator. Возвращает число затронутых записей.
    pub fn bulk_kick(state: &AppState, ids: &[UserId]) -> usize {
        let mut users = state.users.lock().expect("users store poisoned");
        let mut touched = 0;
        for id in ids {
            if let Some(record) = users.get_mut(id) {
                record.deleted = true;
                touched += 1;
            }
        }
        touched
    }

    /// LDAP sync (mock; real ldap3 in vks-auth).
    ///
    /// Принимает `Vec<vks_core::User>` от LDAP-провайдера и upsert
    /// их в хранилище (по username). Возвращает число записей после синка.
    pub fn sync_ldap(state: &AppState, ldap_users: Vec<User>) -> usize {
        let mut users = state.users.lock().expect("users store poisoned");
        for u in ldap_users {
            let exists = users.values().any(|r| r.username == u.username);
            if exists {
                if let Some(record) = users.values_mut().find(|r| r.username == u.username) {
                    record.role = u.role;
                    record.muted = false;
                    record.deleted = false;
                }
            } else {
                let record = UserRecord { id: u.id, username: u.username, role: u.role, muted: false, deleted: false };
                users.insert(record.id, record);
            }
        }
        users.len()
    }

    /// Роль субъекта токена для RBAC (v1-middleware и профиль `/api/v3`).
    ///
    /// `subject` — `sub` из JWT: id или username записи admin-хранилища;
    /// id локального провайдера (`u-admin`, `/api/v3/login`) резолвится в
    /// username через [`vks_auth::local_db::LocalDb`]. `None` — записи нет
    /// или она удалена (fail-closed: доступ запрещён).
    pub fn resolve_role(state: &AppState, subject: &str) -> Option<Role> {
        let username = state
            .local_db
            .as_ref()
            .and_then(|db| db.user(subject).ok())
            .map(|u| u.username)
            .unwrap_or_else(|| subject.to_string());
        let users = state.users.lock().expect("users store poisoned");
        users.values().find(|u| !u.deleted && (u.username == username || u.id.to_string() == subject)).map(|u| u.role)
    }
}

/// LDAP-модель для mock-синка (в production — ldap3 в vks-auth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdapUser {
    /// Username (существенный ключ).
    pub username: String,
    /// Глобальная роль.
    pub role: Role,
}

/// Mock LDAP-провайдер: возвращает фиксированный список пользователей.
///
/// В production заменяется на ldap3 + GSSAPI (vks-auth, 5s timeout,
/// circuit breaker 50%/10s → OPEN 30s, BACKEND_RULES §1.5).
pub struct MockLdapProvider;

impl MockLdapProvider {
    /// Возвращает mock-данные (1 user, role Participant).
    pub fn fetch() -> Vec<User> {
        vec![User {
            id: UserId::new(),
            username: "ldap-user".to_string(),
            role: Role::Participant,
            groups: vec!["vks-users".to_string()],
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_get_roundtrip() {
        let state = AppState::new();
        let created = Users::create(&state, "admin", Role::Operator);
        let fetched = Users::get(&state, &created.id).unwrap();
        assert_eq!(fetched.username, "admin");
        assert_eq!(fetched.role, Role::Operator);
        assert!(!fetched.muted);
    }

    #[test]
    fn soft_delete_hides_from_list() {
        let state = AppState::new();
        let created = Users::create(&state, "u1", Role::Participant);
        assert_eq!(Users::list(&state).len(), 1);
        assert!(Users::delete(&state, &created.id));
        assert_eq!(Users::list(&state).len(), 0);
        // Запись остаётся (soft-delete).
        assert!(Users::get(&state, &created.id).unwrap().deleted);
        assert!(!Users::delete(&state, &created.id));
    }

    #[test]
    fn update_changes_only_given_fields() {
        let state = AppState::new();
        let created = Users::create(&state, "u1", Role::Participant);
        let updated = Users::update(&state, &created.id, None, Some(Role::Moderator)).unwrap();
        assert_eq!(updated.username, "u1");
        assert_eq!(updated.role, Role::Moderator);
    }

    #[test]
    fn bulk_mute_only_touched() {
        let state = AppState::new();
        let a = Users::create(&state, "a", Role::Participant);
        let b = Users::create(&state, "b", Role::Participant);
        let touched = Users::bulk_mute(&state, &[a.id], true);
        assert_eq!(touched, 1);
        assert!(Users::get(&state, &a.id).unwrap().muted);
        assert!(!Users::get(&state, &b.id).unwrap().muted);
        // Non-existent id не считается.
        assert_eq!(Users::bulk_mute(&state, &[UserId::new()], true), 0);
    }

    #[test]
    fn bulk_kick_soft_deletes() {
        let state = AppState::new();
        let a = Users::create(&state, "a", Role::Participant);
        let b = Users::create(&state, "b", Role::Participant);
        assert_eq!(Users::bulk_kick(&state, &[a.id, b.id]), 2);
        assert_eq!(Users::list(&state).len(), 0);
    }

    #[test]
    fn sync_ldap_upserts_by_username() {
        let state = AppState::new();
        // Существующий user с тем же username.
        let existing = Users::create(&state, "ldap-user", Role::Viewer);
        let ldap = MockLdapProvider::fetch();
        assert_eq!(Users::sync_ldap(&state, ldap), 1);
        // Обновил роль + снял deleted, не создал дубликат.
        let rec = Users::get(&state, &existing.id).unwrap();
        assert_eq!(rec.role, Role::Participant);
        assert!(!rec.deleted);
        assert_eq!(state.users.lock().unwrap().len(), 1);
    }

    #[test]
    fn sync_ldap_creates_new_user() {
        let state = AppState::new();
        let ldap = MockLdapProvider::fetch();
        assert_eq!(Users::sync_ldap(&state, ldap), 1);
        let guard = state.users.lock().unwrap();
        let (_, record) = guard.iter().next().unwrap();
        assert_eq!(record.username, "ldap-user");
    }

    #[test]
    fn ldap_user_model() {
        let _ = LdapUser { username: "x".into(), role: Role::Participant };
    }

    #[test]
    fn resolve_role_by_username_id_and_local_db() {
        let mut state = AppState::new();
        let op = Users::create(&state, "op", Role::Operator);
        assert_eq!(Users::resolve_role(&state, "op"), Some(Role::Operator));
        assert_eq!(Users::resolve_role(&state, &op.id.to_string()), Some(Role::Operator));
        assert_eq!(Users::resolve_role(&state, "ghost"), None);

        // LocalDb-субъект (`u-admin`) резолвится в username admin-хранилища.
        let db = std::sync::Arc::new(vks_auth::local_db::LocalDb::new());
        db.register("u-admin", "admin", "x").expect("register");
        state.local_db = Some(db);
        Users::create(&state, "admin", Role::Operator);
        assert_eq!(Users::resolve_role(&state, "u-admin"), Some(Role::Operator));

        // Удалённая запись — доступа нет.
        assert!(Users::delete(&state, &op.id));
        assert_eq!(Users::resolve_role(&state, "op"), None);
    }
}

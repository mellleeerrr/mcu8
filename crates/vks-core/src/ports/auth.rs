// Порт AuthProvider — identity (local / LDAP / OIDC).
//
// Ядро определяет интерфейс для проверки credentials и получения
// пользователя. Реализации: `vks-auth` (local_db, ldap, oidc).
//
// RBAC matrix (ADR-0022) применяется в vks-auth; ядро только
// проверяет credentials.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::participant::{Role, UserId};

/// Ошибка AuthProvider.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// Credentials неверны.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Пользователь не найден.
    #[error("user not found: {0}")]
    UserNotFound(String),
    /// Пользователь заблокирован.
    #[error("user locked out: {0}")]
    LockedOut(String),
    /// Ошибка реализации.
    #[error("auth error: {0}")]
    Other(String),
}

/// Учётные данные для проверки.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// Имя пользователя (или email / DN).
    pub username: String,
    /// Пароль (или secret).
    pub password: String,
}

/// Аутентифицированный пользователь.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Идентикатор пользователя.
    pub id: UserId,
    /// Имя (для display).
    pub username: String,
    /// Глобальная роль (по умолчанию Participant).
    pub role: Role,
    /// Группы (для LDAP).
    pub groups: Vec<String>,
}

/// Порт AuthProvider — стратегия identity (local/LDAP/OIDC).
///
/// АDR-0021: JWT RS256 + rotation. АDR-0009: JWT + API-ключи + LDAP.
#[async_trait]
pub trait AuthProvider: Send + Sync + 'static {
    /// Проверяет credentials; возвращает User при успехе.
    async fn verify(&self, creds: &Credentials) -> Result<User, AuthError>;

    /// Возвращает пользователя по id (после аутентификации).
    async fn get_user(&self, id: UserId) -> Result<Option<User>, AuthError>;

    /// Возвращает пользователя по username.
    async fn get_user_by_name(&self, username: &str) -> Result<Option<User>, AuthError>;
}

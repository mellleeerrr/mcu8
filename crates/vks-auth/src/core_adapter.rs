//! Адаптер реализаций аутентификации под порт `vks_core::ports::AuthProvider`.
//!
//! Связывает конкретные механизмы `vks-auth` (`LocalAuth`, `AuthProviderEnum`)
//! с доменным трейтом ядра `vks_core::ports::AuthProvider`.

use async_trait::async_trait;
use uuid::Uuid;
use vks_core::participant::{Role, UserId};
use vks_core::ports::auth::{
    AuthError as CoreAuthError, AuthProvider as CoreAuthProvider, Credentials, User as CoreUser,
};

use crate::local_db::LocalDbError;
use crate::provider::{AuthProviderEnum, LocalAuth};

fn string_to_user_id(id_str: &str) -> UserId {
    match Uuid::parse_str(id_str) {
        Ok(u) => UserId::from_uuid(u),
        Err(_) => UserId::new(),
    }
}

#[async_trait]
impl CoreAuthProvider for LocalAuth {
    async fn verify(&self, creds: &Credentials) -> Result<CoreUser, CoreAuthError> {
        match self.db().authenticate_async(&creds.username, &creds.password).await {
            Ok(u) => Ok(CoreUser {
                id: string_to_user_id(&u.id),
                username: u.username,
                role: Role::Participant,
                groups: vec![],
            }),
            Err(LocalDbError::AuthFailed) => Err(CoreAuthError::InvalidCredentials),
            Err(LocalDbError::NotFound(name)) => Err(CoreAuthError::UserNotFound(name)),
            Err(LocalDbError::AlreadyExists(e)) => Err(CoreAuthError::Other(e)),
            Err(LocalDbError::PasswordTooLong) => Err(CoreAuthError::InvalidCredentials),
            Err(LocalDbError::Busy) => Err(CoreAuthError::Busy),
        }
    }

    async fn get_user(&self, id: UserId) -> Result<Option<CoreUser>, CoreAuthError> {
        let id_str = id.to_string();
        match self.db().user(&id_str) {
            Ok(u) => Ok(Some(CoreUser { id, username: u.username, role: Role::Participant, groups: vec![] })),
            Err(LocalDbError::NotFound(_)) => Ok(None),
            Err(e) => Err(CoreAuthError::Other(e.to_string())),
        }
    }

    async fn get_user_by_name(&self, username: &str) -> Result<Option<CoreUser>, CoreAuthError> {
        match self.db().user_by_name(username) {
            Ok(u) => Ok(Some(CoreUser {
                id: string_to_user_id(&u.id),
                username: u.username,
                role: Role::Participant,
                groups: vec![],
            })),
            Err(LocalDbError::NotFound(_)) => Ok(None),
            Err(e) => Err(CoreAuthError::Other(e.to_string())),
        }
    }
}

#[async_trait]
impl CoreAuthProvider for AuthProviderEnum {
    async fn verify(&self, creds: &Credentials) -> Result<CoreUser, CoreAuthError> {
        match self {
            Self::Local(local) => CoreAuthProvider::verify(local, creds).await,
            Self::Ldap(ldap) => {
                let token = format!("{}:{}", creds.username, creds.password);
                let res = ldap.verify(&token).await.map_err(|e| CoreAuthError::Other(e.to_string()))?;
                Ok(CoreUser {
                    id: string_to_user_id(&res.id),
                    username: res.username,
                    role: Role::Participant,
                    groups: vec![],
                })
            }
            Self::Oidc(_) => Err(CoreAuthError::Other("OIDC verify via credentials is not supported".into())),
        }
    }

    async fn get_user(&self, id: UserId) -> Result<Option<CoreUser>, CoreAuthError> {
        match self {
            Self::Local(local) => CoreAuthProvider::get_user(local, id).await,
            Self::Ldap(ldap) => {
                let id_str = id.to_string();
                match ldap.user(&id_str).await {
                    Ok(u) => Ok(Some(CoreUser { id, username: u.username, role: Role::Participant, groups: vec![] })),
                    Err(_) => Ok(None),
                }
            }
            Self::Oidc(_) => Ok(None),
        }
    }

    async fn get_user_by_name(&self, username: &str) -> Result<Option<CoreUser>, CoreAuthError> {
        match self {
            Self::Local(local) => CoreAuthProvider::get_user_by_name(local, username).await,
            Self::Ldap(_) => Ok(None),
            Self::Oidc(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn local_auth_satisfies_core_auth_provider() {
        let auth = LocalAuth::new();
        let uid = Uuid::new_v4();
        auth.db().register(&uid.to_string(), "bob", "secret123").expect("register");

        let provider: Arc<dyn CoreAuthProvider> = Arc::new(auth);

        // Good credentials
        let creds = Credentials { username: "bob".to_string(), password: "secret123".to_string() };
        let user = provider.verify(&creds).await.expect("verify bob");
        assert_eq!(user.username, "bob");
        assert_eq!(user.id, UserId::from_uuid(uid));

        // Bad password
        let bad_creds = Credentials { username: "bob".to_string(), password: "wrong".to_string() };
        assert_eq!(provider.verify(&bad_creds).await.unwrap_err(), CoreAuthError::InvalidCredentials);

        // Non-existent user
        let missing_creds = Credentials { username: "nobody".to_string(), password: "wrong".to_string() };
        assert_eq!(
            provider.verify(&missing_creds).await.unwrap_err(),
            CoreAuthError::UserNotFound("nobody".to_string())
        );

        // Lookup by id
        let by_id = provider.get_user(UserId::from_uuid(uid)).await.expect("lookup by id").expect("found");
        assert_eq!(by_id.username, "bob");

        // Lookup by name
        let by_name = provider.get_user_by_name("bob").await.expect("lookup by name").expect("found");
        assert_eq!(by_name.username, "bob");
    }
}

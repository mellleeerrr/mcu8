//! `AuthProvider` strategy adapter (ADR-0009, BACKEND_RULES.md §2.1).
//!
//! A thin, standalone adapter in front of the concrete backends in this crate
//! (`local_db`, `ldap`, `jwt`). It mirrors the `AuthProvider` port in `vks-core`
//! (owned by a parallel agent) but is self-contained so the crate builds without
//! importing `vks-core`.
//!
//! Strategies: `Local` (in-memory user store), `Ldap` (directory bind), `Oidc`
//! (stub — returns `Err(NotImplemented)` until the OIDC adapter lands).
//!
//! The `verify(token)` contract is provider-specific: for `Local`/`Ldap` the
//! token is a `username:password` credential; `Oidc` would carry an OIDC token.
//! A JWT-based path is available directly via [`jwt_issuer`].

use crate::jwt::{JwtConfig, JwtError, JwtIssuer};
use crate::ldap::{LdapAuth, LdapClient, LdapError};
use crate::local_db::{LocalDb, LocalDbError, User};
use async_trait::async_trait;
use thiserror::Error;

/// A user as returned by an auth provider.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub id: String,
    pub username: String,
    /// Which provider produced this identity.
    pub source: AuthSource,
}

/// Where an identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    Local,
    Ldap,
    Oidc,
}

/// Provider-level errors.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("authentication failed")]
    AuthFailed,
    #[error("local db: {0}")]
    LocalDb(#[from] LocalDbError),
    #[error("ldap: {0}")]
    Ldap(#[from] LdapError),
    #[error("jwt: {0}")]
    Jwt(#[from] JwtError),
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// The auth provider port (standalone twin of `vks-core::AuthProvider`).
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// Verify a token/credential and return the authenticated user.
    async fn verify(&self, token: &str) -> Result<AuthenticatedUser, AuthError>;

    /// Look up a user by id.
    async fn user(&self, id: &str) -> Result<User, AuthError>;
}

/// The concrete strategy enum. The Ldap variant is stored as a boxed
/// trait object so the enum is monomorphization-friendly (no `impl Trait`
/// in field position).
pub enum AuthProviderEnum {
    Local(LocalAuth),
    Ldap(Box<dyn AuthProvider>),
    Oidc(OidcAuth),
}

impl AuthProviderEnum {
    /// Wrap a `LdapAuth` into the enum.
    pub fn from_ldap<C: LdapClient + 'static>(auth: LdapAuth<C>) -> Self {
        Self::Ldap(Box::new(LdapProvider::new(auth)))
    }
}

/// Local (in-memory) provider.
#[derive(Clone, Default)]
pub struct LocalAuth {
    db: std::sync::Arc<LocalDb>,
}

impl LocalAuth {
    pub fn new() -> Self {
        Self { db: std::sync::Arc::new(LocalDb::new()) }
    }

    pub fn with_db(db: std::sync::Arc<LocalDb>) -> Self {
        Self { db }
    }

    /// The backing user store (exposed so callers can register users / mint
    /// access tokens after a successful login).
    pub fn db(&self) -> &LocalDb {
        &self.db
    }
}

/// Configuration for creating an AuthProvider instance.
#[derive(Debug, Clone)]
pub struct AuthProviderConfig {
    pub provider_type: String,
    pub ldap_url: Option<String>,
    pub ldap_bind_template: Option<String>,
    pub allow_mock_ldap: bool,
    pub oidc_issuer: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthFactoryError {
    #[error("unknown auth provider: '{0}' (expected 'local', 'ldap', 'oidc')")]
    UnknownProvider(String),
    #[error("ldap configuration error: {0}")]
    LdapConfig(String),
    #[error("mock ldap is prohibited in production (set allow_mock_ldap=true for test harness)")]
    MockLdapProhibited,
    #[error("oidc provider error: {0}")]
    OidcNotConfigured(String),
}

/// Factory function creating a `vks_core::ports::AuthProvider` trait object.
pub fn create_auth_provider(
    config: &AuthProviderConfig,
    local_db: Option<std::sync::Arc<LocalDb>>,
) -> Result<std::sync::Arc<dyn vks_core::ports::AuthProvider>, AuthFactoryError> {
    match config.provider_type.to_lowercase().as_str() {
        "local" => {
            let local_auth = if let Some(db) = local_db { LocalAuth::with_db(db) } else { LocalAuth::new() };
            Ok(std::sync::Arc::new(local_auth))
        }
        "ldap" => {
            let url = config.ldap_url.as_deref().unwrap_or("").trim();
            if url.is_empty() {
                return Err(AuthFactoryError::LdapConfig("LDAP URL is required when auth_provider is 'ldap'".into()));
            }

            if !config.allow_mock_ldap {
                return Err(AuthFactoryError::MockLdapProhibited);
            }

            let template =
                config.ldap_bind_template.clone().unwrap_or_else(|| "uid={user},ou=people,dc=vks,dc=internal".into());
            let ldap_auth = LdapAuth::new(url, template, crate::ldap::MockLdapClient::new());
            Ok(std::sync::Arc::new(AuthProviderEnum::from_ldap(ldap_auth)))
        }
        "oidc" => {
            let _issuer = config.oidc_issuer.as_deref().unwrap_or("").trim();
            Err(AuthFactoryError::OidcNotConfigured(
                "OIDC provider is not yet supported in this build; configure 'local' or 'ldap'".into(),
            ))
        }
        other => Err(AuthFactoryError::UnknownProvider(other.into())),
    }
}

#[async_trait]
impl AuthProvider for LocalAuth {
    async fn verify(&self, token: &str) -> Result<AuthenticatedUser, AuthError> {
        let (username, password) = split_credential(token)?;
        let user = self.db.authenticate_async(&username, &password).await?;
        Ok(AuthenticatedUser { id: user.id, username: user.username, source: AuthSource::Local })
    }

    async fn user(&self, id: &str) -> Result<User, AuthError> {
        Ok(self.db.user(id)?)
    }
}

/// LDAP provider.
pub struct LdapProvider<C: LdapClient> {
    auth: LdapAuth<C>,
}

impl<C: LdapClient> LdapProvider<C> {
    pub fn new(auth: LdapAuth<C>) -> Self {
        Self { auth }
    }
}

#[async_trait]
impl<C: LdapClient> AuthProvider for LdapProvider<C> {
    async fn verify(&self, token: &str) -> Result<AuthenticatedUser, AuthError> {
        let (username, password) = split_credential(token)?;
        self.auth.authenticate(&username, &password).await?;
        Ok(AuthenticatedUser { id: username.clone(), username, source: AuthSource::Ldap })
    }

    async fn user(&self, id: &str) -> Result<User, AuthError> {
        // LDAP has no persistent local user record; return a minimal placeholder.
        Ok(User {
            id: id.to_string(),
            username: id.to_string(),
            password_hash_hex: String::new(),
            salt_hex: String::new(),
        })
    }
}

/// OIDC provider (stub).
///
/// The real OIDC flow (discovery, authorization-code + PKCE, id-token
/// validation) lands in a later phase; for now it is a documented stub that
/// always returns [`AuthError::NotImplemented`].
#[derive(Default)]
pub struct OidcAuth;

#[async_trait]
impl AuthProvider for OidcAuth {
    async fn verify(&self, _token: &str) -> Result<AuthenticatedUser, AuthError> {
        Err(AuthError::NotImplemented("OIDC provider is a stub; wire up discovery + token endpoint in a later phase"))
    }

    async fn user(&self, id: &str) -> Result<User, AuthError> {
        Err(AuthError::NotFound(id.to_string()))
    }
}

/// Split a `username:password` credential token.
fn split_credential(token: &str) -> Result<(String, String), AuthError> {
    match token.split_once(':') {
        Some((u, p)) if !u.is_empty() => Ok((u.to_string(), p.to_string())),
        _ => Err(AuthError::AuthFailed),
    }
}

/// Convenience: build a JWT issuer from config (for callers minting access
/// tokens after a successful login).
pub fn jwt_issuer(config: JwtConfig) -> JwtIssuer {
    JwtIssuer::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::block_on;

    #[test]
    fn local_end_to_end() {
        let provider = LocalAuth::new();
        provider.db().register("u1", "alice", "pw").unwrap();
        let user = block_on(provider.verify("alice:pw")).unwrap();
        assert_eq!(user.id, "u1");
        assert_eq!(user.username, "alice");
        assert_eq!(user.source, AuthSource::Local);
        // user lookup by id
        let fetched = block_on(provider.user("u1")).unwrap();
        assert_eq!(fetched.username, "alice");
    }

    #[test]
    fn local_bad_credential() {
        let provider = LocalAuth::new();
        provider.db().register("u1", "alice", "pw").unwrap();
        assert!(matches!(block_on(provider.verify("alice:wrong")), Err(AuthError::LocalDb(_))));
    }

    #[test]
    fn oidc_stub_not_implemented() {
        let oidc = OidcAuth;
        let err = block_on(oidc.verify("tok")).unwrap_err();
        assert!(matches!(err, AuthError::NotImplemented(_)));
    }

    #[test]
    fn jwt_issuer_helper_roundtrip() {
        let issuer = jwt_issuer(JwtConfig { secret: "s".into(), ttl_secs: 60, issuer: "vks".into() });
        let token = issuer.issue("u1", None).unwrap();
        let v = issuer.verify(&token).unwrap();
        assert_eq!(v.user_id, "u1");
    }

    #[test]
    fn factory_creates_local_provider() {
        let cfg = AuthProviderConfig {
            provider_type: "local".into(),
            ldap_url: None,
            ldap_bind_template: None,
            allow_mock_ldap: false,
            oidc_issuer: None,
        };
        assert!(create_auth_provider(&cfg, None).is_ok());
    }

    #[test]
    fn factory_rejects_mock_ldap_in_production() {
        let cfg = AuthProviderConfig {
            provider_type: "ldap".into(),
            ldap_url: Some("ldaps://ldap.internal:636".into()),
            ldap_bind_template: None,
            allow_mock_ldap: false,
            oidc_issuer: None,
        };
        match create_auth_provider(&cfg, None) {
            Err(AuthFactoryError::MockLdapProhibited) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }

    #[test]
    fn factory_allows_mock_ldap_in_test_mode() {
        let cfg = AuthProviderConfig {
            provider_type: "ldap".into(),
            ldap_url: Some("ldaps://ldap.internal:636".into()),
            ldap_bind_template: None,
            allow_mock_ldap: true,
            oidc_issuer: None,
        };
        assert!(create_auth_provider(&cfg, None).is_ok());
    }

    #[test]
    fn factory_rejects_unconfigured_oidc() {
        let cfg = AuthProviderConfig {
            provider_type: "oidc".into(),
            ldap_url: None,
            ldap_bind_template: None,
            allow_mock_ldap: false,
            oidc_issuer: None,
        };
        match create_auth_provider(&cfg, None) {
            Err(AuthFactoryError::OidcNotConfigured(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }
}

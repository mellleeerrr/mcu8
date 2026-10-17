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
#[derive(Default)]
pub struct LocalAuth {
    db: LocalDb,
}

impl LocalAuth {
    pub fn new() -> Self {
        Self::default()
    }

    /// The backing user store (exposed so callers can register users / mint
    /// access tokens after a successful login).
    pub fn db(&self) -> &LocalDb {
        &self.db
    }
}

#[async_trait]
impl AuthProvider for LocalAuth {
    async fn verify(&self, token: &str) -> Result<AuthenticatedUser, AuthError> {
        let (username, password) = split_credential(token)?;
        let user = self.db.authenticate(&username, &password)?;
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
}

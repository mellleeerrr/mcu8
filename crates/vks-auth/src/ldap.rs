//! LDAP authentication (ADR-0009, BACKEND_RULES.md §1.5).
//!
//! `LdapAuth` holds the connection parameters and a `bind_template` used to build
//! the bind DN from a user id. The actual network I/O is behind the `LdapClient`
//! trait so a mock client can be injected in tests — no live server is required
//! to exercise the DN-building and bind logic.
//!
//! Production target: ldap3 + GSSAPI/Kerberos (FreeIPA/AD), 5 s timeout,
//! circuit breaker per BACKEND_RULES.md §4.5. That transport is a separate
//! adapter implementing [`LdapClient`]; this module keeps the DN + bind logic
//! self-contained and testable.

use async_trait::async_trait;
use thiserror::Error;

/// Errors for LDAP operations.
#[derive(Debug, Error)]
pub enum LdapError {
    #[error("ldap client error: {0}")]
    Client(String),
    #[error("ldap bind failed for dn={0}")]
    BindFailed(String),
    #[error("ldap template invalid: {0}")]
    BadTemplate(String),
}

/// A successful bind result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdapBindResult {
    /// The DN that bound successfully.
    pub dn: String,
    /// Groups the entry belongs to (for RBAC mapping).
    pub groups: Vec<String>,
}

/// Mockable LDAP transport. Implementations own the network I/O; `LdapAuth`
/// builds the DN and delegates the bind here.
#[async_trait]
pub trait LdapClient: Send + Sync {
    /// Bind with the given DN + password. Returns groups on success.
    async fn bind(&self, dn: &str, password: &str) -> Result<LdapBindResult, LdapError>;
}

/// LDAP configuration + auth logic.
#[derive(Debug, Clone)]
pub struct LdapAuth<C: LdapClient> {
    /// LDAP server URL, e.g. `ldaps://ldap.example.com:636`.
    pub url: String,
    /// Bind DN template with a `{user}` placeholder, e.g.
    /// `uid={user},ou=people,dc=example,dc=com`.
    pub bind_template: String,
    client: C,
}

impl<C: LdapClient> LdapAuth<C> {
    /// Wrap a client. The URL + template are plain data (no secrets).
    pub fn new(url: impl Into<String>, bind_template: impl Into<String>, client: C) -> Self {
        Self { url: url.into(), bind_template: bind_template.into(), client }
    }

    /// Build the bind DN for a user id by substituting `{user}`.
    pub fn build_dn(&self, user_id: &str) -> Result<String, LdapError> {
        if !self.bind_template.contains("{user}") {
            return Err(LdapError::BadTemplate(format!(
                "template {} missing `{{user}}` placeholder",
                self.bind_template
            )));
        }
        Ok(self.bind_template.replace("{user}", user_id))
    }

    /// Authenticate a user + password against the directory.
    pub async fn authenticate(&self, user_id: &str, password: &str) -> Result<LdapBindResult, LdapError> {
        let dn = self.build_dn(user_id)?;
        let result = self.client.bind(&dn, password).await.map_err(|_| LdapError::BindFailed(dn.clone()))?;
        Ok(result)
    }
}

/// Mock client for tests and test-harness verification.
#[derive(Default)]
pub struct MockLdapClient {
    pub last_dn: std::sync::Mutex<Option<String>>,
    pub accept: std::sync::Mutex<Option<String>>,
}

impl MockLdapClient {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LdapClient for MockLdapClient {
    async fn bind(&self, dn: &str, password: &str) -> Result<LdapBindResult, LdapError> {
        *self.last_dn.lock().unwrap() = Some(dn.to_string());
        match &*self.accept.lock().unwrap() {
            Some(expected) if expected == password => {
                Ok(LdapBindResult { dn: dn.to_string(), groups: vec!["vks-users".to_string()] })
            }
            Some(expected) => Err(LdapError::Client(format!("bad password for {expected}"))),
            None => Ok(LdapBindResult { dn: dn.to_string(), groups: vec!["vks-users".to_string()] }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::block_on;
    use std::sync::Mutex;
    type MockClient = MockLdapClient;

    #[tokio::test]
    async fn build_dn_substitutes_user() {
        let auth = LdapAuth::new(
            "ldaps://ldap.example.com:636",
            "uid={user},ou=people,dc=example,dc=com",
            MockClient::default(),
        );
        assert_eq!(auth.build_dn("alice").unwrap(), "uid=alice,ou=people,dc=example,dc=com");
    }

    #[tokio::test]
    async fn bad_template_rejected() {
        let auth = LdapAuth::new("ldaps://x", "cn=static", MockClient::default());
        assert!(matches!(auth.build_dn("a"), Err(LdapError::BadTemplate(_))));
    }

    #[test]
    fn authenticate_success() {
        let client = MockClient { last_dn: Mutex::new(None), accept: Mutex::new(Some("pw".to_string())) };
        let auth = LdapAuth::new("ldaps://x", "uid={user},dc=example,dc=com", client);
        let res = block_on(auth.authenticate("bob", "pw")).unwrap();
        assert_eq!(res.dn, "uid=bob,dc=example,dc=com");
        assert_eq!(res.groups, vec!["vks-users"]);
    }

    #[test]
    fn authenticate_bad_password() {
        let client = MockClient { last_dn: Mutex::new(None), accept: Mutex::new(Some("good".to_string())) };
        let auth = LdapAuth::new("ldaps://x", "uid={user},dc=example,dc=com", client);
        let err = block_on(auth.authenticate("bob", "wrong")).unwrap_err();
        assert!(matches!(err, LdapError::BindFailed(_)));
    }
}

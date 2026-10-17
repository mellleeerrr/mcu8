//! `vks-auth` — аутентификация (JWT, API-ключи, LDAP, local_db, provider).
//!
//! Фаза 5 (DEVELOPMENT_PLAN.md):
//! - [`jwt`] — issue + verify JWT (HS256, config-driven secret), ADR-0009/0021.
//! - [`api_key`] — `vks_` + 32-hex API keys, SHA-256 storage, prefix only in logs.
//! - [`ldap`] — LDAP bind via a mockable `LdapClient` port, ADR-0009.
//! - [`local_db`] — built-in (default) in-memory user store with PBKDF2 hashes.
//! - [`provider`] — `AuthProvider` strategy (Local/Ldap/Oidc) + `AuthenticatedUser`.
//!
//! ADR-0009 (JWT/API/LDAP), ADR-0022 (RBAC). Дерево: vks-core, vks-proto.
//!
//! NOTE: the `AuthProvider` port in `vks-core` (owned by a parallel agent) is
//! mirrored here as a standalone adapter in [`provider`] so this crate builds
//! without importing `vks-core`.

pub mod api_key;
pub mod core_adapter;
pub mod jwt;
pub mod ldap;
pub mod local_db;
pub mod provider;

#[cfg(test)]
pub mod test_util {
    //! Синхронный запуск async-future для unit-тестов (через tokio runtime).
    /// Запускает future до готовности на single-thread tokio runtime.
    pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build tokio runtime");
        rt.block_on(fut)
    }
}

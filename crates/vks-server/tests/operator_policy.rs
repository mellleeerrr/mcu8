//! Startup-политика оператора: fresh DB, слабая БД, ротация и рестарт.

use std::sync::Arc;

use vks_auth::local_db::LocalDb;
use vks_common::config::Config;
use vks_core::ports::Storage;
use vks_core::UserId;
use vks_server::operator_policy::{ensure_operator, OperatorOutcome, OperatorPolicyError};
use vks_storage::MemoryStorage;

const STRONG: &str = "strong-production-password";

fn prod_config(password: &str) -> Config {
    Config {
        allow_insecure_defaults: false,
        admin_password: password.into(),
        admin_login: "admin".into(),
        auth_provider: "local".into(),
        jwt_secret: "x".repeat(48),
        database_url: "postgres://vks:pass@db:5432/vks".into(),
        ..Config::default()
    }
}

fn db_with_password(password: &str) -> Arc<LocalDb> {
    let db = Arc::new(LocalDb::new());
    db.register(vks_core::BOOTSTRAP_ADMIN_USER_ID, "admin", password).unwrap();
    db
}

#[tokio::test]
async fn fresh_database_requires_password() {
    let db = Arc::new(LocalDb::new());
    let err = ensure_operator(&prod_config(""), &db, None).await.unwrap_err();
    assert!(matches!(err, OperatorPolicyError::FreshDatabaseNeedsPassword), "{err:?}");
}

#[tokio::test]
async fn fresh_database_registers_strong_password() {
    let db = Arc::new(LocalDb::new());
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let outcome = ensure_operator(&prod_config(STRONG), &db, Some(&storage)).await.unwrap();
    assert_eq!(outcome, OperatorOutcome::Registered);
    assert!(db.authenticate("admin", STRONG).is_ok());
}

#[tokio::test]
async fn weak_database_without_env_password_is_rejected() {
    let db = db_with_password("admin_password");
    let err = ensure_operator(&prod_config(""), &db, None).await.unwrap_err();
    assert!(matches!(err, OperatorPolicyError::StoredPasswordNeedsRotation), "{err:?}");
    assert!(db.authenticate("admin", "admin_password").is_ok(), "must not mutate on rejection");
}

#[tokio::test]
async fn weak_database_is_rotated_and_persisted_once() {
    let db = db_with_password("admin_password");
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());

    let outcome = ensure_operator(&prod_config(STRONG), &db, Some(&storage)).await.unwrap();
    assert_eq!(outcome, OperatorOutcome::Rotated { login: "admin".into() });
    assert!(db.authenticate("admin", "admin_password").is_err());
    assert!(db.authenticate("admin", STRONG).is_ok());

    let creds = storage
        .get_user_credentials(UserId::bootstrap_operator())
        .await
        .unwrap()
        .expect("rotated credentials must be persisted");
    assert_eq!(creds.password_hash_hex, db.user(vks_core::BOOTSTRAP_ADMIN_USER_ID).unwrap().password_hash_hex);

    // Рестарт без env-пароля: слабого пароля больше нет, старт разрешён.
    let outcome = ensure_operator(&prod_config(""), &db, Some(&storage)).await.unwrap();
    assert_eq!(outcome, OperatorOutcome::Restored);
}

#[tokio::test]
async fn placeholder_and_common_weak_database_passwords_are_rejected() {
    for weak in ["CHANGE_ME_STRONG_PASSWORD", "change_me", "password", "secret"] {
        let db = db_with_password(weak);
        let err = ensure_operator(&prod_config(""), &db, None).await.unwrap_err();
        assert!(matches!(err, OperatorPolicyError::StoredPasswordNeedsRotation), "{weak}: {err:?}");
        assert!(db.authenticate("admin", weak).is_ok(), "{weak}: must not mutate on rejection");
    }
}

#[tokio::test]
async fn placeholder_database_password_is_rotated_with_strong_env() {
    let db = db_with_password("CHANGE_ME_STRONG_PASSWORD");
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let outcome = ensure_operator(&prod_config(STRONG), &db, Some(&storage)).await.unwrap();
    assert_eq!(outcome, OperatorOutcome::Rotated { login: "admin".into() });
    assert!(db.authenticate("admin", STRONG).is_ok());
}

#[tokio::test]
async fn existing_strong_password_is_not_overwritten_by_env() {
    let db = db_with_password(STRONG);
    let storage: Arc<dyn Storage> = Arc::new(MemoryStorage::new());
    let outcome = ensure_operator(&prod_config("another-strong-password"), &db, Some(&storage)).await.unwrap();
    assert_eq!(outcome, OperatorOutcome::Restored);
    assert!(db.authenticate("admin", STRONG).is_ok());
    assert!(db.authenticate("admin", "another-strong-password").is_err());
}

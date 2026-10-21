//! Startup-политика оператора: bootstrap, отказ при слабой БД и разовая
//! ротация известных dev-дефолтов. Вынесено из `main` ради тестируемости.

use std::sync::Arc;

use vks_auth::local_db::LocalDb;
use vks_common::config::Config;
use vks_core::ports::storage::UserCredentials;
use vks_core::ports::Storage;
use vks_core::UserId;

/// Результат startup-проверки оператора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorOutcome {
    /// Свежая БД: оператор создан из VKS_ADMIN_PASSWORD.
    Registered,
    /// Существующий оператор; пароль не менялся.
    Restored,
    /// Слабый сохранённый пароль заменён на VKS_ADMIN_PASSWORD и сохранён.
    Rotated { login: String },
}

#[derive(Debug, thiserror::Error)]
pub enum OperatorPolicyError {
    #[error("fresh database has no operator: set a strong VKS_ADMIN_PASSWORD for the first start")]
    FreshDatabaseNeedsPassword,
    #[error("stored operator password is a known dev default; set a strong VKS_ADMIN_PASSWORD once (it will be rotated and persisted) or rotate via the UI")]
    StoredPasswordNeedsRotation,
    #[error("failed to register operator: {0}")]
    Register(String),
    #[error("failed to rotate weak operator password: {0}")]
    Rotate(String),
    #[error("failed to persist rotated operator password: {0}")]
    Persist(String),
}

/// Гарантирует, что в БД есть оператор, и что в production-режиме его пароль
/// не является опубликованным dev-дефолтом. Существующий сильный пароль не
/// перезаписывается; слабый ротируется только при явном VKS_ADMIN_PASSWORD.
pub async fn ensure_operator(
    config: &Config,
    db: &Arc<LocalDb>,
    storage: Option<&Arc<dyn Storage>>,
) -> Result<OperatorOutcome, OperatorPolicyError> {
    let bootstrap_id = vks_core::BOOTSTRAP_ADMIN_USER_ID;

    if db.user(bootstrap_id).is_err() {
        if config.admin_password.trim().is_empty() {
            return Err(OperatorPolicyError::FreshDatabaseNeedsPassword);
        }
        db.register(bootstrap_id, &config.admin_login, &config.admin_password)
            .map_err(|e| OperatorPolicyError::Register(e.to_string()))?;
        return Ok(OperatorOutcome::Registered);
    }

    let admin = db.user(bootstrap_id).map_err(|e| OperatorPolicyError::Register(e.to_string()))?;

    if !config.allow_insecure_defaults && db.authenticates_any(&admin.username, Config::KNOWN_WEAK_PASSWORDS) {
        if config.admin_password.trim().is_empty() || Config::is_weak_password(&config.admin_password) {
            return Err(OperatorPolicyError::StoredPasswordNeedsRotation);
        }
        let updated = db
            .set_password_async(&admin.username, &config.admin_password)
            .await
            .map_err(|e| OperatorPolicyError::Rotate(e.to_string()))?;
        if let Some(storage) = storage {
            let user_id = UserId::parse(&updated.id).unwrap_or_else(|_| UserId::bootstrap_operator());
            let creds = UserCredentials {
                user_id,
                password_hash_hex: updated.password_hash_hex.clone(),
                salt_hex: updated.salt_hex.clone(),
                password_changed_at: Some(db.password_changed_at(&updated.id)),
                generation: db.generation(&updated.id),
            };
            storage.upsert_user_credentials(&creds).await.map_err(|e| OperatorPolicyError::Persist(e.to_string()))?;
        }
        return Ok(OperatorOutcome::Rotated { login: updated.username });
    }

    // Существующий пароль не перезаписывается. Если env-пароль не совпадает,
    // он всё равно не применяется — это не ошибка, но полезно предупредить.
    if !config.admin_password.trim().is_empty() && db.authenticate(&admin.username, &config.admin_password).is_err() {
        tracing::warn!(
            login = %admin.username,
            "VKS_ADMIN_PASSWORD differs from the stored operator password; it is used only for bootstrap/rotation and is ignored for an existing operator"
        );
    }
    Ok(OperatorOutcome::Restored)
}

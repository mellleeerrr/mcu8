//! Built-in (default) user store (ADR-0009, BACKEND_RULES.md §1.3).
//!
//! In-memory user store with password hashing. We pin **PBKDF2-SHA256**
//! (per the allowed dependency set — argon2 is not in the permitted pins) with
//! a fixed high iteration count. Passwords are never stored in plaintext; only
//! the derived hash. A real deployment would back this with PostgreSQL, but the
//! store is isolated behind `LocalDb` so the transport can be swapped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use sha2::Sha256;
use thiserror::Error;

/// PBKDF2 iteration count (high; BACKEND_RULES.md targets Argon2id in prod —
/// this pin uses PBKDF2-SHA256 as the allowed alternative).
pub const PBKDF2_ITERATIONS: u32 = 100_000;

/// A stored user.
#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    /// Username / login name.
    pub username: String,
    /// PBKDF2-SHA256 derived key, hex-encoded.
    pub password_hash_hex: String,
    /// Random 16-byte salt, hex-encoded (32 chars).
    pub salt_hex: String,
}

/// Errors for the local user store.
#[derive(Debug, Error)]
pub enum LocalDbError {
    #[error("user not found: {0}")]
    NotFound(String),
    #[error("authentication failed")]
    AuthFailed,
    #[error("user already exists: {0}")]
    AlreadyExists(String),
}

/// In-memory user store (default provider backing).
///
/// `password_changed_at` tracks the unix-seconds mark of the last password
/// change per user id (registration counts too). Tokens issued before the
/// mark (`iat < mark`) are rejected by `/api/v3` middleware (phase 2 spec
/// §4.3: password change invalidates previously issued tokens).
#[derive(Default)]
pub struct LocalDb {
    users: Mutex<HashMap<String, User>>,
    password_changed_at: Mutex<HashMap<String, i64>>,
    generations: Mutex<HashMap<String, u64>>,
}

impl LocalDb {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a user with a plaintext password (hashed on entry).
    pub fn register(&self, id: &str, username: &str, password: &str) -> Result<User, LocalDbError> {
        let (hash_hex, salt_hex) = hash_password(password);
        let user = User { id: id.to_string(), username: username.to_string(), password_hash_hex: hash_hex, salt_hex };
        let mut map = self.users.lock().unwrap();
        if map.contains_key(id) {
            return Err(LocalDbError::AlreadyExists(id.to_string()));
        }
        // Логины уникальны без учёта регистра Unicode (как и при переименовании):
        // иначе `authenticate` выбирал бы случайный из дублей HashMap.
        let username_key = username.trim().to_lowercase();
        if map.values().any(|user| user.username.to_lowercase() == username_key) {
            return Err(LocalDbError::AlreadyExists(username.to_string()));
        }
        let clone = user.clone();
        map.insert(id.to_string(), user);
        drop(map);
        self.generations.lock().unwrap().insert(id.to_string(), 1);
        self.mark_password_changed(id, now_unix_secs());
        Ok(clone)
    }

    /// Fetch a user by id.
    pub fn user(&self, id: &str) -> Result<User, LocalDbError> {
        self.users.lock().unwrap().get(id).cloned().ok_or_else(|| LocalDbError::NotFound(id.to_string()))
    }

    /// Authenticate by username + password. Returns the user on success.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<User, LocalDbError> {
        let user_opt = {
            let map = self.users.lock().unwrap();
            map.values().find(|u| u.username == username).cloned()
        };
        match user_opt {
            Some(user) => {
                let salt = hex_decode(&user.salt_hex).unwrap_or_default();
                let (candidate_hash, _) = hash_password_with_salt(&salt, password);
                if subtle::ConstantTimeEq::ct_eq(user.password_hash_hex.as_bytes(), candidate_hash.as_bytes()).into() {
                    Ok(user)
                } else {
                    Err(LocalDbError::AuthFailed)
                }
            }
            None => {
                // Constant-time dummy hash to protect against timing-based user enumeration
                let dummy_salt = [0u8; 16];
                let _ = hash_password_with_salt(&dummy_salt, password);
                Err(LocalDbError::NotFound(username.to_string()))
            }
        }
    }

    /// Проверяет, принимает ли пользователь любой из перечисленных паролей.
    /// Нужно только для startup-проверки известных dev-дефолтов; не логирует
    /// и не меняет состояние. Пустой список всегда false.
    pub fn authenticates_any(&self, username: &str, passwords: &[&str]) -> bool {
        passwords.iter().any(|p| self.authenticate(username, p).is_ok())
    }

    /// Переименовать логин существующего пользователя (admin UI «Учётная
    /// запись»). Коллизия логинов без учёта регистра Unicode → `AlreadyExists`.
    pub fn rename_user(&self, id: &str, new_username: &str) -> Result<User, LocalDbError> {
        let new_username = new_username.trim();
        if new_username.is_empty() {
            return Err(LocalDbError::NotFound(new_username.to_string()));
        }
        let mut map = self.users.lock().unwrap();
        if map.values().any(|u| u.id != id && u.username.to_lowercase() == new_username.to_lowercase()) {
            return Err(LocalDbError::AlreadyExists(new_username.to_string()));
        }
        let user = map.get_mut(id).ok_or_else(|| LocalDbError::NotFound(id.to_string()))?;
        user.username = new_username.to_string();
        Ok(user.clone())
    }

    /// Fetch a user by username (exact match; uniqueness is case-insensitive).
    pub fn user_by_name(&self, username: &str) -> Result<User, LocalDbError> {
        let map = self.users.lock().unwrap();
        map.values()
            .find(|u| u.username == username)
            .cloned()
            .ok_or_else(|| LocalDbError::NotFound(username.to_string()))
    }

    /// Record that `user_id`'s password changed at `ts` (unix seconds).
    ///
    /// Explicit-timestamp variant of the internal mark (tests/tooling):
    /// allows setting the mark without rehashing a password.
    pub fn record_password_change_at(&self, user_id: &str, ts: i64) {
        self.mark_password_changed(user_id, ts);
    }

    /// Unix seconds of the last password change for `user_id`; `0` = unknown
    /// (no invalidation).
    pub fn password_changed_at(&self, user_id: &str) -> i64 {
        self.password_changed_at.lock().unwrap().get(user_id).copied().unwrap_or(0)
    }

    /// Monotonic generation counter of `user_id` (default 1).
    pub fn generation(&self, user_id: &str) -> u64 {
        self.generations.lock().unwrap().get(user_id).copied().unwrap_or(1)
    }

    /// Explicitly bump monotonic generation counter (returns new generation).
    pub fn bump_generation(&self, user_id: &str) -> u64 {
        let mut map = self.generations.lock().unwrap();
        let gen = map.entry(user_id.to_string()).or_insert(1);
        *gen += 1;
        *gen
    }

    /// Remove a user's credential (login) from the in-memory store.
    ///
    /// Поколение не удаляется: монотонный счётчик должен остаться, чтобы
    /// выданные ранее токены оставались невалидными после удаления логина.
    pub fn remove_user(&self, id: &str) -> bool {
        let removed = self.users.lock().unwrap().remove(id).is_some();
        if removed {
            self.password_changed_at.lock().unwrap().insert(id.to_string(), now_unix_secs());
            self.bump_generation(id);
        }
        removed
    }

    /// Replace `username`'s password (fresh salt + PBKDF2 hash) and bump the
    /// change mark and monotonic generation, invalidating previously issued tokens.
    pub fn set_password(&self, username: &str, new_password: &str) -> Result<User, LocalDbError> {
        let (hash_hex, salt_hex) = hash_password(new_password);
        let (id, updated) = {
            let mut map = self.users.lock().unwrap();
            let Some((id, user)) = map.iter_mut().find(|(_, u)| u.username == username) else {
                return Err(LocalDbError::NotFound(username.to_string()));
            };
            user.password_hash_hex = hash_hex;
            user.salt_hex = salt_hex;
            (id.clone(), user.clone())
        };
        self.bump_generation(&id);
        self.mark_password_changed(&id, now_unix_secs() + 1);
        Ok(updated)
    }

    /// Asynchronously replace `username`'s password, offloading PBKDF2 to a worker thread (M1.3).
    pub async fn set_password_async(&self, username: &str, new_password: &str) -> Result<User, LocalDbError> {
        let pwd = new_password.to_string();
        let (hash_hex, salt_hex) =
            tokio::task::spawn_blocking(move || hash_password(&pwd)).await.map_err(|_| LocalDbError::AuthFailed)?;
        let (id, updated) = {
            let mut map = self.users.lock().unwrap();
            let Some((id, user)) = map.iter_mut().find(|(_, u)| u.username == username) else {
                return Err(LocalDbError::NotFound(username.to_string()));
            };
            user.password_hash_hex = hash_hex;
            user.salt_hex = salt_hex;
            (id.clone(), user.clone())
        };
        self.bump_generation(&id);
        self.mark_password_changed(&id, now_unix_secs() + 1);
        Ok(updated)
    }

    /// Asynchronously authenticate by username + password, offloading PBKDF2 to a worker thread (M1.3).
    pub async fn authenticate_async(&self, username: &str, password: &str) -> Result<User, LocalDbError> {
        let user_opt = {
            let map = self.users.lock().unwrap();
            map.values().find(|u| u.username == username).cloned()
        };
        let uname = username.to_string();
        let pwd = password.to_string();
        tokio::task::spawn_blocking(move || match user_opt {
            Some(user) => {
                let salt = hex_decode(&user.salt_hex).unwrap_or_default();
                let (candidate_hash, _) = hash_password_with_salt(&salt, &pwd);
                if subtle::ConstantTimeEq::ct_eq(user.password_hash_hex.as_bytes(), candidate_hash.as_bytes()).into() {
                    Ok(user)
                } else {
                    Err(LocalDbError::AuthFailed)
                }
            }
            None => {
                let dummy_salt = [0u8; 16];
                let _ = hash_password_with_salt(&dummy_salt, &pwd);
                Err(LocalDbError::NotFound(uname))
            }
        })
        .await
        .map_err(|_| LocalDbError::AuthFailed)?
    }

    /// Asynchronously register a user, offloading PBKDF2 to a worker thread (M1.3).
    pub async fn register_async(&self, id: &str, username: &str, password: &str) -> Result<User, LocalDbError> {
        let pwd = password.to_string();
        let (hash_hex, salt_hex) =
            tokio::task::spawn_blocking(move || hash_password(&pwd)).await.map_err(|_| LocalDbError::AuthFailed)?;
        let user = User { id: id.to_string(), username: username.to_string(), password_hash_hex: hash_hex, salt_hex };
        let mut map = self.users.lock().unwrap();
        if map.contains_key(id) {
            return Err(LocalDbError::AlreadyExists(id.to_string()));
        }
        let clone = user.clone();
        map.insert(id.to_string(), user);
        drop(map);
        self.generations.lock().unwrap().insert(id.to_string(), 1);
        self.mark_password_changed(id, now_unix_secs());
        Ok(clone)
    }

    /// Restore user credentials and marks from durable storage without rehashing (M1.3).
    pub fn restore_user(
        &self,
        id: &str,
        username: &str,
        password_hash_hex: &str,
        salt_hex: &str,
        password_changed_at: Option<i64>,
        generation: Option<u64>,
    ) {
        let user = User {
            id: id.to_string(),
            username: username.to_string(),
            password_hash_hex: password_hash_hex.to_string(),
            salt_hex: salt_hex.to_string(),
        };
        self.users.lock().unwrap().insert(id.to_string(), user);
        if let Some(ts) = password_changed_at {
            self.password_changed_at.lock().unwrap().insert(id.to_string(), ts);
        }
        if let Some(gen) = generation {
            self.generations.lock().unwrap().insert(id.to_string(), gen);
        }
    }

    /// Internal: set the change mark for `user_id`.
    fn mark_password_changed(&self, user_id: &str, ts: i64) {
        self.password_changed_at.lock().unwrap().insert(user_id.to_string(), ts);
    }
}

/// Current unix time in seconds (0 on clock-before-epoch).
fn now_unix_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Derive a PBKDF2-SHA256 hash with a fresh random salt. Returns (hash_hex, salt_hex).
fn hash_password(password: &str) -> (String, String) {
    let salt = random_salt();
    let (hash, salt_hex) = hash_password_with_salt(&salt, password);
    (hash, salt_hex)
}

/// Derive a PBKDF2-SHA256 hash with a given salt. Returns (hash_hex, salt_hex).
fn hash_password_with_salt(salt: &[u8], password: &str) -> (String, String) {
    let mut derived = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ITERATIONS, &mut derived);
    let salt_hex = hex_encode(salt);
    (hex_encode(&derived), salt_hex)
}

fn random_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    salt
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    let bytes = hex.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let hi = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => return None,
        };
        let lo = match bytes[i + 1] {
            b'0'..=b'9' => bytes[i + 1] - b'0',
            b'a'..=b'f' => bytes[i + 1] - b'a' + 10,
            b'A'..=b'F' => bytes[i + 1] - b'A' + 10,
            _ => return None,
        };
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_user_revokes_login_and_bumps_generation() {
        let db = LocalDb::new();
        db.register("u-1", "ivan", "secret").expect("register");
        let before = db.generation("u-1");
        assert!(db.remove_user("u-1"));
        assert!(db.user("u-1").is_err(), "логин удалён");
        assert!(db.user_by_name("ivan").is_err(), "имя освобождено");
        assert!(db.generation("u-1") > before, "поколение инкрементировано");
        assert!(!db.remove_user("u-1"), "повторное удаление — false");
    }

    #[test]
    fn register_and_authenticate_ok() {
        let db = LocalDb::new();
        db.register("u1", "alice", "correct-horse-battery").unwrap();
        let user = db.authenticate("alice", "correct-horse-battery").unwrap();
        assert_eq!(user.id, "u1");
        assert_eq!(user.username, "alice");
    }

    #[test]
    fn wrong_password_fails() {
        let db = LocalDb::new();
        db.register("u2", "bob", "right-pass").unwrap();
        assert!(matches!(db.authenticate("bob", "wrong-pass"), Err(LocalDbError::AuthFailed)));
    }

    #[test]
    fn unknown_user_not_found() {
        let db = LocalDb::new();
        assert!(matches!(db.authenticate("ghost", "x"), Err(LocalDbError::NotFound(_))));
    }

    #[test]
    fn user_lookup_by_id() {
        let db = LocalDb::new();
        db.register("u3", "carol", "pass1234").unwrap();
        let u = db.user("u3").unwrap();
        assert_eq!(u.username, "carol");
    }

    #[test]
    fn set_password_rehashes_and_bumps_mark() {
        let db = LocalDb::new();
        db.register("u9", "dave", "old-pass").unwrap();
        let before = db.password_changed_at("u9");
        assert!(before > 0, "registration must seed the password-change mark");
        let user = db.set_password("dave", "new-pass").unwrap();
        assert_eq!(user.id, "u9");
        assert!(matches!(db.authenticate("dave", "old-pass"), Err(LocalDbError::AuthFailed)));
        assert!(db.authenticate("dave", "new-pass").is_ok());
        assert!(db.password_changed_at("u9") >= before);
    }

    #[test]
    fn set_password_unknown_user_not_found() {
        let db = LocalDb::new();
        assert!(matches!(db.set_password("ghost", "x"), Err(LocalDbError::NotFound(_))));
        assert_eq!(db.password_changed_at("ghost"), 0);
    }

    #[test]
    fn record_password_change_at_sets_explicit_mark() {
        let db = LocalDb::new();
        db.register("u10", "erin", "pw").unwrap();
        db.record_password_change_at("u10", 1_700_000_000);
        assert_eq!(db.password_changed_at("u10"), 1_700_000_000);
    }

    #[tokio::test]
    async fn async_authenticate_and_set_password() {
        let db = LocalDb::new();
        db.register_async("u20", "frank", "frank_pass").await.unwrap();
        let user = db.authenticate_async("frank", "frank_pass").await.unwrap();
        assert_eq!(user.id, "u20");

        assert!(matches!(db.authenticate_async("frank", "wrong_pass").await, Err(LocalDbError::AuthFailed)));

        db.set_password_async("frank", "new_frank_pass").await.unwrap();
        assert!(db.authenticate_async("frank", "new_frank_pass").await.is_ok());
        assert!(matches!(db.authenticate_async("frank", "frank_pass").await, Err(LocalDbError::AuthFailed)));
    }

    #[test]
    fn restore_user_preserves_hashes_and_marks() {
        let db = LocalDb::new();
        let registered = db.register("u30", "grace", "grace_secret").unwrap();
        let mark = db.password_changed_at("u30");
        let gen = db.generation("u30");

        let db2 = LocalDb::new();
        db2.restore_user("u30", "grace", &registered.password_hash_hex, &registered.salt_hex, Some(mark), Some(gen));

        assert_eq!(db2.password_changed_at("u30"), mark);
        assert_eq!(db2.generation("u30"), gen);
        let auth = db2.authenticate("grace", "grace_secret").unwrap();
        assert_eq!(auth.id, "u30");
    }

    #[test]
    fn authenticates_any_detects_weak_defaults_only() {
        let db = LocalDb::new();
        db.register("u-weak", "weak_user", "admin_password").unwrap();
        assert!(db.authenticates_any("weak_user", &["admin", "admin_password"]));
        assert!(!db.authenticates_any("weak_user", &["admin", "strong-pass"]));
        assert!(!db.authenticates_any("missing_user", &["admin_password"]));
        assert!(!db.authenticates_any("weak_user", &[]));
    }
}

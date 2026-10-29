//! Built-in (default) user store (ADR-0009, BACKEND_RULES.md §1.3).
//!
//! In-memory user store with password hashing. New passwords use **Argon2id**
//! (`m=19456 KiB, t=2, p=1` — OWASP baseline) and are stored as a PHC string in
//! `password_hash_hex` (the `salt_hex` column stays empty: the PHC string
//! carries its own salt). Pre-existing PBKDF2-SHA256 rows still authenticate
//! and are lazily rehashed to Argon2id on the next successful login (including
//! through the shipped `LocalAuth` provider) without bumping the token
//! generation (see [`LocalDb::needs_rehash`]).
//!
//! Passwords are never stored in plaintext; only the derived hash. A real
//! deployment would back this with PostgreSQL, but the store is isolated
//! behind `LocalDb` so the transport can be swapped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use thiserror::Error;

/// PBKDF2 iteration count for verifying legacy rows (kept for migration).
pub const PBKDF2_ITERATIONS: u32 = 100_000;

/// Argon2id parameters (OWASP 2024 baseline; BACKEND_RULES.md §1.3).
pub const ARGON2_MEMORY_KIB: u32 = 19_456;
pub const ARGON2_TIME_COST: u32 = 2;
pub const ARGON2_PARALLELISM: u32 = 1;

/// Longest accepted password (UTF-8 bytes): bounds Argon2 input and request size.
pub const MAX_PASSWORD_BYTES: usize = 128;

/// At most four concurrent password hashes; extra requests wait in a bounded
/// queue of [`AUTH_QUEUE_CAPACITY`] and, once it is full, fail immediately with
/// [`LocalDbError::Busy`] → HTTP 429. The queue is bounded by *waiters*, not by
/// a wall-clock timeout: a slow hash must not turn a correct login into a
/// spurious 429, while a flood still gets shed deterministically.
pub const AUTH_HASH_PERMITS: usize = 4;
pub const AUTH_QUEUE_CAPACITY: usize = 128;

fn argon2() -> Argon2<'static> {
    let params = Params::new(ARGON2_MEMORY_KIB, ARGON2_TIME_COST, ARGON2_PARALLELISM, None)
        .expect("static Argon2id parameters are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Number of tasks currently holding or waiting for a hash permit. Must be a
/// single module-level static: separate function-local statics would not see
/// each other and the counter would only ever grow.
static HASH_WAITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn hash_gate() -> &'static std::sync::Arc<tokio::sync::Semaphore> {
    static GATE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(AUTH_HASH_PERMITS)))
}

/// RAII slot in the bounded hashing queue.
///
/// The counter must survive future cancellation: a client that disconnects
/// while waiting for a permit must still release its queue slot, otherwise
/// repeated aborts leak the counter and permanently return `Busy`.
struct QueueSlot;

impl QueueSlot {
    fn acquire() -> Result<Self, LocalDbError> {
        use std::sync::atomic::Ordering;
        if HASH_WAITERS.fetch_add(1, Ordering::SeqCst) >= AUTH_QUEUE_CAPACITY {
            HASH_WAITERS.fetch_sub(1, Ordering::SeqCst);
            return Err(LocalDbError::Busy);
        }
        Ok(Self)
    }
}

impl Drop for QueueSlot {
    fn drop(&mut self) {
        HASH_WAITERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Test-only high-water mark of concurrently executing hash jobs and the
/// number currently running. Regression guard for the re-review N1: a
/// cancelled request must not admit another hash while its blocking job runs.
#[cfg(test)]
pub(crate) static MAX_CONCURRENT_HASHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
pub(crate) static ACTIVE_HASHES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
struct ActiveHashGuard;
#[cfg(test)]
impl Drop for ActiveHashGuard {
    fn drop(&mut self) {
        ACTIVE_HASHES.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

async fn hash_permit() -> Result<(QueueSlot, tokio::sync::OwnedSemaphorePermit), LocalDbError> {
    let slot = QueueSlot::acquire()?;
    let permit = std::sync::Arc::clone(hash_gate()).acquire_owned().await.map_err(|_| LocalDbError::Busy)?;
    Ok((slot, permit))
}

/// Runs a blocking password-hash/verify job with the gate held *inside* the
/// blocking task. `spawn_blocking` tasks cannot be cancelled, so moving the
/// permit into the closure keeps the "≤ 4 concurrent Argon2/MiB" invariant
/// even when the requesting future is dropped mid-hash.
async fn run_gated<T, F>(job: F) -> Result<T, LocalDbError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let gate = hash_permit().await?;
    tokio::task::spawn_blocking(move || {
        let _gate = gate;
        #[cfg(test)]
        let _active = {
            let now = ACTIVE_HASHES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            MAX_CONCURRENT_HASHES.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            ActiveHashGuard
        };
        job()
    })
    .await
    .map_err(|_| LocalDbError::AuthFailed)
}

/// A stored user.
#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    /// Username / login name.
    pub username: String,
    /// Hash column: Argon2id PHC string for new rows, PBKDF2 hex for legacy rows.
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
    #[error("password exceeds {MAX_PASSWORD_BYTES} bytes")]
    PasswordTooLong,
    #[error("password hashing queue is saturated")]
    Busy,
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
    /// Synchronous registration convenience used by unit tests and bootstrap
    /// fixtures ONLY. Production request paths MUST use
    /// [`LocalDb::register_async`] so the 4-permit Argon2 gate applies.
    #[doc(hidden)]
    pub fn register(&self, id: &str, username: &str, password: &str) -> Result<User, LocalDbError> {
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(LocalDbError::PasswordTooLong);
        }
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
                if verify_password(&user, password) {
                    Ok(user)
                } else {
                    Err(LocalDbError::AuthFailed)
                }
            }
            None => {
                // Same Argon2id work as a real verify: a cheaper dummy would
                // ~2.5x faster and leaks account existence through timing.
                let _ = hash_password(password);
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

    /// Replace `username`'s password (fresh Argon2id PHC string) and bump the
    /// change mark and monotonic generation, invalidating previously issued tokens.
    pub fn set_password(&self, username: &str, new_password: &str) -> Result<User, LocalDbError> {
        if new_password.len() > MAX_PASSWORD_BYTES {
            return Err(LocalDbError::PasswordTooLong);
        }
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

    /// Asynchronously replace `username`'s password, hashing Argon2id on a blocking worker.
    pub async fn set_password_async(&self, username: &str, new_password: &str) -> Result<User, LocalDbError> {
        if new_password.len() > MAX_PASSWORD_BYTES {
            return Err(LocalDbError::PasswordTooLong);
        }
        let pwd = new_password.to_string();
        let (hash_hex, salt_hex) = run_gated(move || hash_password(&pwd)).await?;
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

    /// Asynchronously authenticate by username + password (Argon2id/PBKDF2 verify on a blocking worker).
    pub async fn authenticate_async(&self, username: &str, password: &str) -> Result<User, LocalDbError> {
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(LocalDbError::AuthFailed);
        }
        let user_opt = {
            let map = self.users.lock().unwrap();
            map.values().find(|u| u.username == username).cloned()
        };
        let uname = username.to_string();
        let pwd = password.to_string();
        run_gated(move || match user_opt {
            Some(user) => {
                if verify_password(&user, &pwd) {
                    Ok(user)
                } else {
                    Err(LocalDbError::AuthFailed)
                }
            }
            None => {
                let _ = hash_password(&pwd);
                Err(LocalDbError::NotFound(uname))
            }
        })
        .await?
    }

    /// True when `user` still has a legacy PBKDF2 credential that should be
    /// upgraded to Argon2id after a successful login.
    #[must_use]
    pub fn needs_rehash(user: &User) -> bool {
        let hash = user.password_hash_hex.as_str();
        let mut parts = hash.split('$');
        if parts.next() != Some("") || parts.next() != Some("argon2id") {
            return true;
        }
        let _version = parts.next();
        let mut memory = 0u32;
        let mut time = 0u32;
        let mut parallelism = 0u32;
        for field in parts.next().unwrap_or_default().split(',') {
            match field.split_once('=') {
                Some(("m", value)) => memory = value.parse().unwrap_or(0),
                Some(("t", value)) => time = value.parse().unwrap_or(0),
                Some(("p", value)) => parallelism = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        memory != ARGON2_MEMORY_KIB || time != ARGON2_TIME_COST || parallelism != ARGON2_PARALLELISM
    }

    /// Lazily upgrades a legacy PBKDF2 credential to Argon2id after a verified
    /// login. The token generation and password-change mark are intentionally
    /// untouched: rehashing is not a password change and must not revoke
    /// sessions or fail the login.
    pub async fn rehash_async(&self, user_id: &str, password: &str) -> Result<User, LocalDbError> {
        self.rehash_async_if_unchanged(user_id, None, password).await
    }

    /// Compare-and-swap lazy rehash: only upgrades when the stored hash still
    /// equals `expected` (None = no CAS). Prevents a concurrent password change
    /// from being overwritten by a stale login hash.
    pub async fn rehash_async_if_unchanged(
        &self,
        user_id: &str,
        expected: Option<&str>,
        password: &str,
    ) -> Result<User, LocalDbError> {
        let pwd = password.to_string();
        let (hash_hex, salt_hex) = run_gated(move || hash_password(&pwd)).await?;
        let mut map = self.users.lock().unwrap();
        let user = map.get_mut(user_id).ok_or_else(|| LocalDbError::NotFound(user_id.to_string()))?;
        if expected.is_some_and(|hash| user.password_hash_hex != hash) {
            return Err(LocalDbError::AuthFailed);
        }
        user.password_hash_hex = hash_hex;
        user.salt_hex = salt_hex;
        Ok(user.clone())
    }

    /// Asynchronously register a user, offloading PBKDF2 to a worker thread (M1.3).
    pub async fn register_async(&self, id: &str, username: &str, password: &str) -> Result<User, LocalDbError> {
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(LocalDbError::PasswordTooLong);
        }
        let pwd = password.to_string();
        let (hash_hex, salt_hex) = run_gated(move || hash_password(&pwd)).await?;
        let user = User { id: id.to_string(), username: username.to_string(), password_hash_hex: hash_hex, salt_hex };
        let mut map = self.users.lock().unwrap();
        if map.contains_key(id) {
            return Err(LocalDbError::AlreadyExists(id.to_string()));
        }
        // Same case-insensitive login uniqueness as the sync `register`: the
        // check and the insert happen under one lock, so two concurrent
        // `register_async` calls cannot create duplicate logins.
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

/// Hash a password with Argon2id. Returns `(phc_string, "")`: the PHC string
/// embeds algorithm, parameters and salt, so the legacy `salt_hex` column is
/// intentionally empty for new credentials (legacy rows keep using it).
fn hash_password(password: &str) -> (String, String) {
    let salt = SaltString::generate(&mut OsRng);
    let phc = argon2().hash_password(password.as_bytes(), &salt).expect("Argon2id hashing cannot fail").to_string();
    (phc, String::new())
}

/// Verifies a password against either the current Argon2id PHC string or a
/// legacy PBKDF2-SHA256 `(hash_hex, salt_hex)` pair.
fn verify_password(user: &User, password: &str) -> bool {
    if user.password_hash_hex.starts_with("$argon2") {
        return PasswordHash::new(&user.password_hash_hex)
            .ok()
            .is_some_and(|hash| argon2().verify_password(password.as_bytes(), &hash).is_ok());
    }
    let salt = hex_decode(&user.salt_hex).unwrap_or_default();
    let (candidate_hash, _) = hash_password_with_salt(&salt, password);
    subtle::ConstantTimeEq::ct_eq(user.password_hash_hex.as_bytes(), candidate_hash.as_bytes()).into()
}

/// Derive a PBKDF2-SHA256 hash with a given salt. Returns (hash_hex, salt_hex).
fn hash_password_with_salt(salt: &[u8], password: &str) -> (String, String) {
    let mut derived = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, PBKDF2_ITERATIONS, &mut derived);
    let salt_hex = hex_encode(salt);
    (hex_encode(&derived), salt_hex)
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
    use std::sync::Arc;

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
    fn new_credentials_are_argon2id_phc_and_bounded() {
        let db = LocalDb::new();
        let user = db.register("u40", "heidi", "correct horse battery staple").unwrap();
        assert!(user.password_hash_hex.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{}", user.password_hash_hex);
        assert!(user.salt_hex.is_empty(), "PHC string embeds the salt");
        assert!(db.authenticate("heidi", "correct horse battery staple").is_ok());
        assert!(!LocalDb::needs_rehash(&user));

        let long = "x".repeat(MAX_PASSWORD_BYTES + 1);
        assert!(matches!(db.register("u41", "long", &long), Err(LocalDbError::PasswordTooLong)));
        assert!(matches!(db.set_password("heidi", &long), Err(LocalDbError::PasswordTooLong)));
    }

    #[test]
    fn legacy_pbkdf2_row_still_authenticates() {
        let db = LocalDb::new();
        // PBKDF2-SHA256(100000) of "correct horse battery staple" with
        // salt 000102…0f, produced by the pre-T02 hashing code.
        db.restore_user(
            "u42",
            "legacy",
            "49d49c25f597846209f0d92e7770ab64e1c75e94b4ce6c509265ee67175d2a1e",
            "000102030405060708090a0b0c0d0e0f",
            Some(7),
            Some(3),
        );
        let user = db.authenticate("legacy", "correct horse battery staple").unwrap();
        assert!(LocalDb::needs_rehash(&user), "legacy row must be flagged for upgrade");
        assert!(matches!(db.authenticate("legacy", "wrong"), Err(LocalDbError::AuthFailed)));
    }

    #[tokio::test]
    async fn lazy_rehash_upgrades_legacy_row_without_bumping_generation() {
        let db = LocalDb::new();
        db.restore_user(
            "u43",
            "legacy2",
            "49d49c25f597846209f0d92e7770ab64e1c75e94b4ce6c509265ee67175d2a1e",
            "000102030405060708090a0b0c0d0e0f",
            Some(11),
            Some(5),
        );
        let before = db.authenticate_async("legacy2", "correct horse battery staple").await.unwrap();
        assert!(LocalDb::needs_rehash(&before));
        let gen = db.generation("u43");
        let mark = db.password_changed_at("u43");
        let upgraded = db.rehash_async("u43", "correct horse battery staple").await.unwrap();
        assert!(!LocalDb::needs_rehash(&upgraded), "lazy rehash must produce Argon2id");
        assert_eq!(db.generation("u43"), gen, "rehash is not a password change");
        assert_eq!(db.password_changed_at("u43"), mark);
        assert!(db.authenticate_async("legacy2", "correct horse battery staple").await.is_ok());
    }

    /// Regression for the P0 waiter-counter leak: aborting futures that are
    /// queued for a hash permit must release their queue slots (RAII guard),
    /// never make every later hash return `Busy` forever.
    #[tokio::test]
    async fn cancelled_waiters_release_their_queue_slots() {
        let mut held = Vec::new();
        for _ in 0..AUTH_HASH_PERMITS {
            held.push(hash_permit().await.expect("initial permit"));
        }
        let mut tasks = Vec::new();
        for _ in 0..40 {
            tasks.push(tokio::spawn(async {
                let _ = hash_permit().await;
            }));
        }
        // Let them park on the semaphore, then cancel while still queued.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        drop(held);
        let _fresh = tokio::time::timeout(std::time::Duration::from_secs(2), hash_permit())
            .await
            .expect("cancelled waiters must not pin the queue")
            .expect("queue slot must be available after cancellation");
    }

    #[tokio::test]
    async fn cas_rehash_refuses_to_overwrite_a_newer_password() {
        let db = LocalDb::new();
        db.restore_user(
            "u44",
            "legacy3",
            "49d49c25f597846209f0d92e7770ab64e1c75e94b4ce6c509265ee67175d2a1e",
            "000102030405060708090a0b0c0d0e0f",
            Some(1),
            Some(1),
        );
        let stale = db.user_by_name("legacy3").unwrap().password_hash_hex;
        // Model a concurrent password change that landed after the login read.
        db.set_password_async("legacy3", "brand-new-password").await.unwrap();
        let error = db
            .rehash_async_if_unchanged("u44", Some(&stale), "correct horse battery staple")
            .await
            .expect_err("stale CAS must be rejected");
        assert!(matches!(error, LocalDbError::AuthFailed));
        assert!(db.authenticate_async("legacy3", "brand-new-password").await.is_ok());
    }

    /// Regression for the re-review N1: the semaphore permit is owned by the
    /// blocking job, so aborting request futures must NOT let a cancelled hash
    /// overlap a new one (old bug: up to ~7 concurrent hashes after cancels).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_hashes_never_exceed_the_permit_limit() {
        let db: Arc<LocalDb> = Arc::new(LocalDb::new());
        for index in 0..6 {
            db.register_async(&format!("u-cancel-{index}"), &format!("cancel{index}"), "correct horse battery")
                .await
                .unwrap();
        }
        // Abort half of the requests while their hashes are in flight.
        let mut aborted: Vec<tokio::task::JoinHandle<Result<User, LocalDbError>>> = Vec::new();
        for index in 0..6 {
            let db = db.clone();
            aborted.push(tokio::spawn(async move {
                db.authenticate_async(&format!("cancel{index}"), "correct horse battery").await
            }));
        }
        // Let the spawned hashes actually reach `spawn_blocking` first: aborting
        // immediately (before any yield) never starts them, which made this
        // regression test pass even with the buggy permit placement.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        for task in aborted.drain(..3) {
            task.abort();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Storm the gate while the detached hashes may still be finishing.
        let mut storm: Vec<tokio::task::JoinHandle<Result<User, LocalDbError>>> = Vec::new();
        for index in 0..6 {
            let db = db.clone();
            storm.push(tokio::spawn(async move {
                db.authenticate_async(&format!("cancel{index}"), "correct horse battery").await
            }));
        }
        for task in storm {
            assert!(task.await.expect("join").is_ok(), "a valid login must not become 429/Busy");
        }
        for task in aborted {
            let _ = task.await;
        }
        assert!(
            MAX_CONCURRENT_HASHES.load(std::sync::atomic::Ordering::SeqCst) <= AUTH_HASH_PERMITS,
            "at most {} concurrent hashes allowed, observed {}",
            AUTH_HASH_PERMITS,
            MAX_CONCURRENT_HASHES.load(std::sync::atomic::Ordering::SeqCst)
        );
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

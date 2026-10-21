//! Межпроцессный advisory lock на SQLite-файл.
//!
//! Сервер держит lock всё время жизни, CLI (`vks-db-dedup`) обязан взять тот
//! же lock перед backup/--apply. Это защищает от записи в живую БД, пока
//! сервер держит состояние в памяти и перезапишет документ следующим commit.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DbLockError {
    #[error("database is locked by another process: {0}")]
    AlreadyLocked(PathBuf),
    #[error("failed to open database lock file {path}: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("failed to lock database {path}: {source}")]
    Lock { path: PathBuf, source: io::Error },
}

/// Держит эксклюзивный `flock(LOCK_EX|LOCK_NB)` до Drop.
#[derive(Debug)]
pub struct DbLock {
    _file: File,
    path: PathBuf,
}

impl DbLock {
    /// Блокирует `<db>.lock` (создаётся 0600). Повторный захват из другого
    /// процесса/дескриптора возвращает `AlreadyLocked`.
    pub fn acquire(db_path: &Path) -> Result<Self, DbLockError> {
        let path = lock_path(db_path);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|source| DbLockError::Open { path: path.clone(), source })?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let source = io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(DbLockError::AlreadyLocked(path));
            }
            return Err(DbLockError::Lock { path, source });
        }
        Ok(Self { _file: file, path })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DbLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// `<db>.lock` рядом с БД (не в /tmp: lock должен переживать рестарт).
/// Относительный путь канонизируется, иначе `vks-db-dedup vks_data.db` из
/// другого CWD взял бы другой lock и обошёл защиту.
#[must_use]
pub fn lock_path(db_path: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(db_path).ok().or_else(|| {
        let parent = db_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::canonicalize(parent).ok().map(|parent| parent.join(db_path.file_name().unwrap_or_default()))
    });
    let db_path = canonical.as_deref().unwrap_or(db_path);
    let name = db_path.file_name().and_then(|name| name.to_str()).unwrap_or("vks_data.db");
    db_path.with_file_name(format!("{name}.lock"))
}

/// Путь SQLite из `sqlite:///path?mode=rwc`; `None` для :memory:/postgres.
#[must_use]
pub fn sqlite_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("sqlite://").or_else(|| url.strip_prefix("sqlite:"))?;
    let rest = rest.split('?').next().unwrap_or(rest);
    if rest.is_empty() || rest == ":memory:" || rest.contains(":memory:") {
        return None;
    }
    Some(PathBuf::from(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_rejected_until_first_is_dropped() {
        let dir = std::env::temp_dir().join(format!("vks-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("vks_data.db");
        let first = DbLock::acquire(&db).expect("first lock");
        let err = DbLock::acquire(&db).expect_err("second lock must fail");
        assert!(matches!(err, DbLockError::AlreadyLocked(_)), "{err:?}");
        drop(first);
        let second = DbLock::acquire(&db).expect("lock after drop");
        drop(second);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parses_sqlite_urls_only() {
        assert_eq!(sqlite_path_from_url("sqlite:///tmp/a.db?mode=rwc"), Some(PathBuf::from("/tmp/a.db")));
        assert_eq!(sqlite_path_from_url("sqlite:/tmp/b.db"), Some(PathBuf::from("/tmp/b.db")));
        assert_eq!(sqlite_path_from_url("sqlite::memory:"), None);
        assert_eq!(sqlite_path_from_url("postgres://db/vks"), None);
    }
}

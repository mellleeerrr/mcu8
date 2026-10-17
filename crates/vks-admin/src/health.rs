// Health — liveness (/healthz) + readiness (/readyz).
//
// BACKEND_RULES §4.3: `/healthz` (liveness) 200 всегда;
// `/readyz` (readiness): DB + NATS + media OK → 200, иначе 503.
//
// Чистые функции, возвращающие struct — axum-handler оборачивает.

use serde::{Deserialize, Serialize};

/// Liveness (liveness: процесс жив).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Liveness {
    /// `true` = process alive (always 200).
    pub alive: bool,
}

impl Liveness {
    /// Создаёт liveness (alive).
    pub fn alive() -> Self {
        Self { alive: true }
    }

    /// Создаёт liveness (dead).
    pub fn dead() -> Self {
        Self { alive: false }
    }

    /// HTTP status: 200 alive, 5003 dead.
    pub fn status_code(&self) -> u16 {
        if self.alive {
            200
        } else {
            503
        }
    }
}

/// Readiness (DB + NATS + media OK → ready).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readiness {
    /// PostgreSQL (pool + query OK).
    pub db: bool,
    /// NATS (connected + publish OK).
    pub nats: bool,
    /// Media (SFU/MCU instances healthy).
    pub media: bool,
}

impl Readiness {
    /// Создаёт readiness из флага.
    pub fn new(db: bool, nats: bool, media: bool) -> Self {
        Self { db, nats, media }
    }

    /// Все зависимости OK.
    pub fn all_ok() -> Self {
        Self { db: true, nats: true, media: true }
    }

    /// Ready ли (все флаги true).
    pub fn ready(&self) -> bool {
        self.db && self.nats && self.media
    }

    /// HTTP status: 200 ready, 503 not ready.
    pub fn status_code(&self) -> u16 {
        if self.ready() {
            200
        } else {
            503
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_alive_200() {
        let l = Liveness::alive();
        assert_eq!(l.status_code(), 200);
    }

    #[test]
    fn liveness_dead_503() {
        let l = Liveness::dead();
        assert_eq!(l.status_code(), 503);
    }

    #[test]
    fn readiness_all_ok_200() {
        let r = Readiness::all_ok();
        assert!(r.ready());
        assert_eq!(r.status_code(), 200);
    }

    #[test]
    fn readiness_missing_dep_503() {
        let r = Readiness::new(false, true, true);
        assert!(!r.ready());
        assert_eq!(r.status_code(), 503);
        let r2 = Readiness::new(true, false, true);
        assert!(!r2.ready());
        let r3 = Readiness::new(true, true, false);
        assert!(!r3.ready());
    }

    #[test]
    fn readiness_serialization() {
        let r = Readiness::all_ok();
        let json = serde_json::to_value(r).unwrap();
        assert_eq!(json["db"], true);
        assert_eq!(json["nats"], true);
        assert_eq!(json["media"], true);
    }
}

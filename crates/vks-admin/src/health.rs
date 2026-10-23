// Health — liveness (/healthz) + readiness (/readyz).
//
// BACKEND_RULES §4.3: `/healthz` (liveness) 200 всегда;
// `/readyz` (readiness): DB (реальный ping хранилища) + media OK → 200, иначе 503.
// NATS-проверка вернётся вместе с реальным NATS (ADR-0014, долг S3).
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

    /// HTTP status: 200 alive, 503 dead.
    pub fn status_code(&self) -> u16 {
        if self.alive {
            200
        } else {
            503
        }
    }
}

/// Readiness (DB + media + cluster + gateways OK → ready).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Readiness {
    /// Хранилище: ping OK (`SELECT 1`); без хранилища (in-memory) — true.
    pub db: bool,
    /// Media (SFU/MCU instances healthy).
    pub media: bool,
    /// Кластер: лизы валидны, координация активна.
    #[serde(default = "default_true")]
    pub cluster: bool,
    /// Шлюзы: SIP/H.323 доступны.
    #[serde(default = "default_true")]
    pub gateway: bool,
}

fn default_true() -> bool {
    true
}

impl Readiness {
    /// Создаёт readiness из флагов.
    pub fn new(db: bool, media: bool) -> Self {
        Self { db, media, cluster: true, gateway: true }
    }

    /// Все зависимости OK (тестовый хелпер).
    pub fn all_ok() -> Self {
        Self { db: true, media: true, cluster: true, gateway: true }
    }

    /// Вычисляет готовность по реальному состоянию процесса.
    ///
    /// - `db` — ping сконфигурированного хранилища; `storage: None` — in-memory
    ///   режим, деградации нет.
    /// - `media` — медиа-движки являются in-process компонентами без внешних
    ///   зависимостей (их отказ = паника процесса, её ловит liveness).
    /// - `cluster` — отсутствие сбоев лизов и кластерной координации (M0.6).
    /// - `gateway` — отсутствие отказов SIP/H.323 шлюзов (M0.6 / M5.1).
    pub async fn from_state(state: &crate::state::AppState) -> Self {
        let db = if state.has_fault("db") {
            false
        } else {
            match &state.storage {
                None => true,
                Some(storage) => storage.ping().await.is_ok(),
            }
        };
        let media =
            if state.has_fault("media") { false } else { state.is_media_healthy() && state.mode.lock().is_ok() };
        let cluster = !state.has_fault("cluster") && !state.has_fault("lease");
        let gateway = !state.has_fault("gateway");
        Self { db, media, cluster, gateway }
    }

    /// Ready ли (все флаги true).
    pub fn ready(&self) -> bool {
        self.db && self.media && self.cluster && self.gateway
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
        let r = Readiness::new(false, true);
        assert!(!r.ready());
        assert_eq!(r.status_code(), 503);
        let r2 = Readiness::new(true, false);
        assert!(!r2.ready());
        assert_eq!(r2.status_code(), 503);
    }

    #[tokio::test]
    async fn from_state_without_storage_is_ready() {
        // In-memory режим (хранилище не сконфигурировано) — готов: db=true.
        let state = crate::state::AppState::new();
        let r = Readiness::from_state(&state).await;
        assert!(r.db && r.media);
        assert_eq!(r.status_code(), 200);
    }

    #[tokio::test]
    async fn fault_injection_degrades_readiness() {
        let state = crate::state::AppState::new();
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.inject_fault("db");
        let r_db = Readiness::from_state(&state).await;
        assert!(!r_db.db);
        assert_eq!(r_db.status_code(), 503);

        state.clear_fault("db");
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.inject_fault("media");
        let r_med = Readiness::from_state(&state).await;
        assert!(!r_med.media);
        assert_eq!(r_med.status_code(), 503);

        state.clear_fault("media");
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.set_media_healthy(false);
        assert_eq!(Readiness::from_state(&state).await.status_code(), 503);
        state.set_media_healthy(true);
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.inject_fault("cluster");
        let r_cluster = Readiness::from_state(&state).await;
        assert!(!r_cluster.cluster);
        assert_eq!(r_cluster.status_code(), 503);
        state.clear_fault("cluster");
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.inject_fault("lease");
        let r_lease = Readiness::from_state(&state).await;
        assert!(!r_lease.cluster);
        assert_eq!(r_lease.status_code(), 503);
        state.clear_fault("lease");
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);

        state.inject_fault("gateway");
        let r_gw = Readiness::from_state(&state).await;
        assert!(!r_gw.gateway);
        assert_eq!(r_gw.status_code(), 503);
        state.clear_fault("gateway");
        assert_eq!(Readiness::from_state(&state).await.status_code(), 200);
    }

    #[test]
    fn readiness_serialization() {
        let r = Readiness::all_ok();
        let json = serde_json::to_value(r).unwrap();
        assert_eq!(json["db"], true);
        assert_eq!(json["media"], true);
        assert_eq!(json["cluster"], true);
        assert_eq!(json["gateway"], true);
        // NATS-фикция удалена (T1.3): поля нет, пока NATS не подключён (ADR-0014).
        assert!(json.get("nats").is_none());
    }
}

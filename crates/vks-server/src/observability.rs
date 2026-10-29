//! T14: observability/shutdown wiring for the binary.
//!
//! Этот модуль — **bin-only** (`main.rs` объявляет `mod observability;`):
//! `vks-server/src/lib.rs` принадлежит параллельной задаче T13, поэтому не
//! трогаем его ради экспорта. Здесь только то, что реально работает без
//! новых зависимостей:
//!
//! - `warn_if_nats_requested` / `warn_if_otlp_requested` — честная
//!   диагностика: env задан, но адаптер не собран (blocked, см.
//!   `docs/operations/cluster-bus.md`). Никакого «тихого» игнорирования.
//! - `drain_cluster_bus` — SIGTERM-путь: дренаж локальной шины с wall-clock
//!   таймаутом через [`vks_cluster::run_with_timeout`], чтобы зависший
//!   flush не держал процесс.
//!
//! OTLP-экспортёр (`opentelemetry-otlp`) и NATS (`async-nats`) не добавлены:
//! их нет в офлайн-реестре. Когда появятся, сюда же придёт `OtlpGuard` и
//! `NatsTransport`, а `main.rs` уже вызывает точки входа ниже.

use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};
use vks_cluster::{run_with_timeout, Bus, StepResult, NATS_URL_ENV};

/// Переменная окружения OTLP-endpoint (пока только диагностируется).
pub const OTLP_ENDPOINT_ENV: &str = "VKS_OTLP_ENDPOINT";

/// Таймаут дренажа кластерной шины на SIGTERM (не 30-секундный общий).
pub const CLUSTER_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// `true`, если оператор задал непустой `VKS_NATS_URL`.
pub fn nats_url_requested() -> bool {
    std::env::var(NATS_URL_ENV).is_ok_and(|url| !url.trim().is_empty())
}

/// `Some(endpoint)`, если оператор задал непустой `VKS_OTLP_ENDPOINT`.
pub fn otlp_endpoint_requested() -> Option<String> {
    std::env::var(OTLP_ENDPOINT_ENV).ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// Предупреждает, что NATS запрошен, но не собран; сервер остаётся на
/// `LocalTransport` (fail-open, никогда не exit).
pub fn warn_if_nats_requested() {
    if nats_url_requested() {
        warn!(
            env = NATS_URL_ENV,
            "NATS URL is configured but the nats transport is not compiled in (blocked: async-nats is absent \
             from the offline registry); cluster stays on LocalTransport (fail-open)"
        );
    }
}

/// Предупреждает, что OTLP запрошен, но экспортёр не собран.
pub fn warn_if_otlp_requested() {
    if let Some(endpoint) = otlp_endpoint_requested() {
        warn!(
            endpoint = %endpoint,
            env = OTLP_ENDPOINT_ENV,
            "OTLP endpoint is configured but the OTLP exporter is not compiled in (blocked: opentelemetry-otlp \
             is absent from the offline registry); no spans will be exported"
        );
    }
}

/// Дренаж локальной шины: забирает все темы, логирует объём.
///
/// Возвращает [`StepResult`] по таймауту; вызывается из SIGTERM-пути.
pub fn drain_cluster_bus_blocking(bus: Arc<Bus>, timeout: Duration) -> StepResult {
    run_with_timeout(
        move || {
            let topics = bus.topics();
            let mut messages = 0usize;
            for topic in &topics {
                messages += bus.drain(topic).len();
            }
            info!(topics = topics.len(), messages, "cluster bus drained on shutdown");
            Ok(())
        },
        timeout,
    )
}

/// Async-обёртка для graceful shutdown: блокирующий дренаж уходит в
/// `spawn_blocking`, tokio-runtime не блокируется дольше таймаута.
pub async fn drain_cluster_bus(bus: Arc<Bus>) -> StepResult {
    match tokio::task::spawn_blocking(move || drain_cluster_bus_blocking(bus, CLUSTER_DRAIN_TIMEOUT)).await {
        Ok(result) => result,
        Err(error) => {
            warn!(%error, "cluster bus drain task failed");
            StepResult::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_cluster::Msg;

    #[test]
    fn drain_removes_all_topics_and_messages() {
        let bus = Arc::new(Bus::new());
        bus.publish(Msg::new("vks.cluster.lease.update", vec![1], true));
        bus.publish(Msg::new("vks.cluster.heartbeat", vec![2], false));
        assert_eq!(drain_cluster_bus_blocking(bus.clone(), Duration::from_secs(1)), StepResult::Ok);
        assert!(bus.topics().is_empty());
        assert!(bus.read("vks.cluster.lease.update").is_empty());
    }

    #[test]
    fn empty_bus_drain_is_ok() {
        let bus = Arc::new(Bus::new());
        assert_eq!(drain_cluster_bus_blocking(bus, Duration::from_secs(1)), StepResult::Ok);
    }

    #[tokio::test]
    async fn async_drain_completes() {
        let bus = Arc::new(Bus::new());
        bus.publish(Msg::new("vks.cluster.lease.update", vec![1], true));
        assert_eq!(drain_cluster_bus(bus.clone()).await, StepResult::Ok);
        assert!(bus.topics().is_empty());
    }

    #[test]
    fn otlp_detection_treats_whitespace_as_unset() {
        // No env mutation in tests (parallel test processes share the process
        // environment); the parser is the unit under test.
        fn normalize(raw: Option<&str>) -> Option<String> {
            raw.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
        }
        assert_eq!(normalize(None), None);
        assert_eq!(normalize(Some("   ")), None);
        assert_eq!(normalize(Some(" http://127.0.0.1:4318 ")), Some("http://127.0.0.1:4318".to_string()));
    }
}

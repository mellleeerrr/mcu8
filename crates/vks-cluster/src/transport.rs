//! Транспорт кластерной шины: локальный fallback и (позже) NATS.
//!
//! Основание: `docs/architecture/CLUSTERING.md` §1/§5, ADR-0014,
//! `docs/audit/2026-10-29/04_infra_l5l7.md` §4.4.
//!
//! Инварианты:
//! - `VKS_NATS_URL` пуст/не задан → `LocalTransport` (текущий in-memory `Bus`).
//! - Ошибка конфигурации/подключения NATS **никогда** не роняет старт
//!   (fail-open): узел продолжает работать локально, в лог — `warn!`.
//! - `Transport` синхронный: `ClusterManager` и `Bus` — синхронные, а будущий
//!   NATS-адаптер держит соединение в фоновом runtime и общается через каналы.
//!   Это не даёт async заразить существующий API.
//! - `flush`/`shutdown` — часть graceful shutdown: вызываются с таймаутом
//!   снаружи (CLUSTERING.md §3, шаг `StorageFlush`).
//!
//! Что реально сейчас: `LocalTransport` (Bus), парсинг/валидация `VKS_NATS_URL`,
//! fail-open выбор транспорта, идемпотентные `flush`/`shutdown`.
//! Что блокировано: `NatsTransport` требует `async-nats`, которого нет в
//! офлайн-реестре (см. `docs/operations/cluster-bus.md`).

use std::fmt;
use std::sync::Arc;

use crate::bus::{Bus, Msg, PublishResult};

/// Переменная окружения с URL NATS. Пусто/не задано → локальная шина.
pub const NATS_URL_ENV: &str = "VKS_NATS_URL";

/// Переменная окружения с таймаутом подключения к NATS (мс).
pub const NATS_CONNECT_TIMEOUT_ENV: &str = "VKS_NATS_CONNECT_TIMEOUT_MS";

/// Таймаут подключения к NATS по умолчанию (CLUSTERING.md §6: NATS 5s).
pub const DEFAULT_NATS_CONNECT_TIMEOUT_MS: u64 = 5_000;

/// Тип транспорта (для логов, метрик и `/api/v1/cluster/status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// In-memory `Bus` (single-node, дефолт).
    Local,
    /// Реальный NATS (P2; пока не собран).
    Nats,
}

impl TransportKind {
    /// Строковое имя для логов/метрик.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Local => "local",
            TransportKind::Nats => "nats",
        }
    }
}

impl fmt::Display for TransportKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ошибка транспорта.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// NATS-адаптер не собран (blocked: `async-nats` нет в офлайн-реестре).
    #[error("NATS transport is not compiled in (blocked: async-nats is absent from the offline registry)")]
    NotCompiled,
    /// Ошибка конфигурации (пустой/невалидный URL, таймаут).
    #[error("invalid cluster transport configuration: {0}")]
    Config(String),
    /// Подключение/публикация не удались.
    #[error("cluster transport unavailable: {0}")]
    Unavailable(String),
    /// Транспорт уже закрыт (после shutdown).
    #[error("cluster transport is closed")]
    Closed,
    /// Операция не уложилась в таймаут.
    #[error("cluster transport timed out after {0} ms")]
    Timeout(u64),
}

/// Синхронный контракт кластерной шины.
///
/// Реализации обязаны быть потокобезопасными: `ClusterManager` дергает их из
/// нескольких tokio-задач и heartbeat-цикла.
///
/// Feature `nats`/`NatsTransport` появятся вместе с `async-nats`; до этого
/// [`connect_nats`] всегда возвращает [`TransportError::NotCompiled`].
pub trait Transport: Send + Sync + fmt::Debug {
    /// Тип транспорта.
    fn kind(&self) -> TransportKind;

    /// `true`, если сообщения реально уходят за пределы процесса.
    fn is_remote(&self) -> bool {
        self.kind() != TransportKind::Local
    }

    /// Публикует сообщение (dedup по `Msg::id` — на стороне реализации).
    fn publish(&self, msg: Msg) -> Result<PublishResult, TransportError>;

    /// Читает накопленные сообщения темы (копии, в порядке публикации).
    fn read(&self, topic: &str) -> Vec<Msg>;

    /// Забирает и очищает очередь темы (graceful drain).
    fn drain(&self, topic: &str) -> Vec<Msg>;

    /// Досылает in-flight сообщения и снимает подписки (шаг StorageFlush).
    fn flush(&self) -> Result<(), TransportError>;

    /// Закрывает транспорт; идемпотентно.
    fn shutdown(&self) -> Result<(), TransportError>;
}

/// Локальная реализация: обёртка над in-memory [`Bus`].
///
/// Это дефолтный и единственный доступный без `nats`-фичи транспорт;
/// семантика (dedup, DLQ, ordering) полностью сохраняется.
#[derive(Clone, Default)]
pub struct LocalTransport {
    bus: Arc<Bus>,
}

impl fmt::Debug for LocalTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Bus` держит `Mutex` и не выводит себя в Debug: показываем только тип,
        // чтобы `Debug` не блокировал шину и не печатал payload'ы.
        f.debug_struct("LocalTransport").field("kind", &TransportKind::Local).finish_non_exhaustive()
    }
}

impl LocalTransport {
    /// Создаёт локальный транспорт с новой шиной.
    pub fn new() -> Self {
        Self { bus: Arc::new(Bus::new()) }
    }

    /// Создаёт локальный транспорт поверх существующей шины (общий `Bus`).
    pub fn with_bus(bus: Arc<Bus>) -> Self {
        Self { bus }
    }

    /// Ссылка на шину (для метрик/статуса, обратная совместимость).
    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }
}

impl Transport for LocalTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Local
    }

    fn publish(&self, msg: Msg) -> Result<PublishResult, TransportError> {
        Ok(self.bus.publish(msg))
    }

    fn read(&self, topic: &str) -> Vec<Msg> {
        self.bus.read(topic)
    }

    fn drain(&self, topic: &str) -> Vec<Msg> {
        self.bus.drain(topic)
    }

    fn flush(&self) -> Result<(), TransportError> {
        // In-memory: доставлять нечего, очереди уже консистентны.
        Ok(())
    }

    fn shutdown(&self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Конфигурация NATS, извлечённая из окружения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsConfig {
    /// URL сервера (`nats://host:4222` или `tls://host:4222`).
    pub url: String,
    /// Таймаут подключения, мс.
    pub connect_timeout_ms: u64,
}

impl NatsConfig {
    /// Парсит и валидирует URL NATS.
    ///
    /// Разрешены только `nats://` и `tls://`; пустой URL — ошибка
    /// конфигурации (пустое значение означает «локальная шина», см.
    /// [`select_transport`]).
    pub fn parse(url: &str, connect_timeout_ms: u64) -> Result<Self, TransportError> {
        let url = url.trim();
        if url.is_empty() {
            return Err(TransportError::Config(format!("{NATS_URL_ENV} is empty")));
        }
        let has_scheme = url.starts_with("nats://") || url.starts_with("tls://");
        if !has_scheme {
            return Err(TransportError::Config(format!(
                "{NATS_URL_ENV} must start with nats:// or tls://, got {url:?}"
            )));
        }
        let authority = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
        if authority.is_empty() || authority.starts_with(':') {
            return Err(TransportError::Config(format!("{NATS_URL_ENV} has no host: {url:?}")));
        }
        let connect_timeout_ms =
            if connect_timeout_ms == 0 { DEFAULT_NATS_CONNECT_TIMEOUT_MS } else { connect_timeout_ms };
        Ok(Self { url: url.to_string(), connect_timeout_ms })
    }

    /// Читает `VKS_NATS_URL` / `VKS_NATS_CONNECT_TIMEOUT_MS` из окружения.
    ///
    /// `Ok(None)` — NATS не сконфигурирован (штатный single-node режим).
    pub fn from_env() -> Result<Option<Self>, TransportError> {
        match std::env::var(NATS_URL_ENV) {
            Ok(url) if url.trim().is_empty() => Ok(None),
            Ok(url) => {
                let timeout = std::env::var(NATS_CONNECT_TIMEOUT_ENV)
                    .ok()
                    .and_then(|raw| raw.trim().parse::<u64>().ok())
                    .unwrap_or(DEFAULT_NATS_CONNECT_TIMEOUT_MS);
                Self::parse(&url, timeout).map(Some)
            }
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(TransportError::Config(format!("{NATS_URL_ENV} is not valid UTF-8")))
            }
        }
    }
}

/// Результат выбора транспорта при старте.
#[derive(Debug, Clone)]
pub struct TransportSelection {
    /// Выбранный транспорт.
    pub transport: Arc<dyn Transport>,
    /// Причина fallback (None — выбран запрошенный транспорт).
    pub fallback_reason: Option<String>,
}

impl TransportSelection {
    fn local(reason: Option<String>) -> Self {
        Self { transport: Arc::new(LocalTransport::new()), fallback_reason: reason }
    }
}

/// Выбирает транспорт по значению `VKS_NATS_URL` (fail-open).
///
/// - `None`/пусто → `LocalTransport`, без предупреждения (штатный режим).
/// - Невалидный URL → `LocalTransport` + `fallback_reason` (никогда не паника).
/// - Валидный URL → [`connect_nats`]; пока NATS-адаптер не собран (blocked),
///   это `LocalTransport` + причина `nats_unavailable: ...` (fail-open).
pub fn select_transport(url: Option<&str>) -> TransportSelection {
    let Some(url) = url else {
        return TransportSelection::local(None);
    };
    if url.trim().is_empty() {
        return TransportSelection::local(None);
    }
    if let Err(error) = NatsConfig::parse(url, DEFAULT_NATS_CONNECT_TIMEOUT_MS) {
        tracing::warn!(%error, "VKS_NATS_URL rejected; falling back to LocalTransport");
        return TransportSelection::local(Some(format!("invalid_config: {error}")));
    }
    // NATS-адаптер не собран: `async-nats` отсутствует в офлайн-реестре.
    // Это документированный fail-open, а не ошибка конфигурации.
    match connect_nats(url) {
        Ok(transport) => TransportSelection { transport, fallback_reason: None },
        Err(error) => {
            tracing::warn!(%error, "NATS connect failed; falling back to LocalTransport (fail-open)");
            TransportSelection::local(Some(format!("nats_unavailable: {error}")))
        }
    }
}

/// Подключается к NATS (когда адаптер будет собран).
///
/// Блокировано до появления `async-nats` в офлайн-реестре: без фичи возвращает
/// [`TransportError::NotCompiled`]. Сигнатура фиксирована, чтобы вызывающий код
/// (`main.rs`, тесты) не менялся при включении фичи.
pub fn connect_nats(_url: &str) -> Result<Arc<dyn Transport>, TransportError> {
    Err(TransportError::NotCompiled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(topic: &str) -> Msg {
        Msg::new(topic, vec![1, 2, 3], true)
    }

    #[test]
    fn local_transport_delivers_and_drains() {
        let transport = LocalTransport::new();
        assert_eq!(transport.kind(), TransportKind::Local);
        assert!(!transport.is_remote());
        transport.bus().subscribe("vks.cluster.lease.update");

        assert_eq!(transport.publish(msg("vks.cluster.lease.update")).expect("publish"), PublishResult::Delivered);
        let read = transport.read("vks.cluster.lease.update");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].payload, vec![1, 2, 3]);

        let drained = transport.drain("vks.cluster.lease.update");
        assert_eq!(drained.len(), 1);
        assert!(transport.read("vks.cluster.lease.update").is_empty());
    }

    #[test]
    fn local_transport_keeps_dedup_and_dlq_semantics() {
        let transport = LocalTransport::new();
        let first = msg("vks.cluster.heartbeat");
        let id = first.id.clone();
        assert_eq!(transport.publish(first).expect("publish"), PublishResult::Delivered);
        assert_eq!(
            transport
                .publish(Msg { id, topic: "vks.cluster.heartbeat".into(), payload: vec![9], critical: true })
                .expect("publish dup"),
            PublishResult::Duplicate
        );
        assert_eq!(transport.bus().duplicate_count(), 1);
    }

    #[test]
    fn local_flush_and_shutdown_are_idempotent() {
        let transport = LocalTransport::new();
        assert_eq!(transport.flush(), Ok(()));
        assert_eq!(transport.flush(), Ok(()));
        assert_eq!(transport.shutdown(), Ok(()));
        assert_eq!(transport.shutdown(), Ok(()));
    }

    #[test]
    fn shared_bus_is_visible_through_transport() {
        let bus = Arc::new(Bus::new());
        bus.subscribe("t");
        let transport = LocalTransport::with_bus(bus.clone());
        transport.publish(msg("t")).expect("publish");
        assert_eq!(bus.queue_len("t"), 1);
        assert_eq!(transport.read("t").len(), 1);
    }

    #[test]
    fn nats_config_accepts_supported_schemes() {
        let cfg = NatsConfig::parse(" nats://127.0.0.1:4222 ", 0).expect("nats url");
        assert_eq!(cfg.url, "nats://127.0.0.1:4222");
        assert_eq!(cfg.connect_timeout_ms, DEFAULT_NATS_CONNECT_TIMEOUT_MS);

        let tls = NatsConfig::parse("tls://nats.internal:4222", 250).expect("tls url");
        assert_eq!(tls.connect_timeout_ms, 250);
    }

    #[test]
    fn nats_config_rejects_empty_or_unsupported_urls() {
        for bad in ["", "   ", "http://127.0.0.1:4222", "127.0.0.1:4222", "nats://", "nats://:4222"] {
            let err = NatsConfig::parse(bad, 100).expect_err("must reject {bad:?}");
            assert!(matches!(err, TransportError::Config(_)), "unexpected error for {bad:?}: {err}");
        }
    }

    #[test]
    fn select_transport_defaults_to_local_without_url() {
        let selection = select_transport(None);
        assert_eq!(selection.transport.kind(), TransportKind::Local);
        assert!(selection.fallback_reason.is_none());

        let empty = select_transport(Some("  "));
        assert_eq!(empty.transport.kind(), TransportKind::Local);
        assert!(empty.fallback_reason.is_none());
    }

    #[test]
    fn select_transport_fails_open_on_invalid_url() {
        let selection = select_transport(Some("http://not-nats:4222"));
        assert_eq!(selection.transport.kind(), TransportKind::Local);
        let reason = selection.fallback_reason.expect("fallback reason");
        assert!(reason.starts_with("invalid_config:"), "reason: {reason}");
    }

    #[test]
    fn connect_nats_is_explicitly_not_compiled() {
        // The stub must never pretend to have a NATS connection.
        assert_eq!(connect_nats("nats://127.0.0.1:4222").expect_err("must fail"), TransportError::NotCompiled);
    }

    #[test]
    fn select_transport_with_valid_url_fails_open_while_nats_is_unavailable() {
        let selection = select_transport(Some("nats://127.0.0.1:4222"));
        assert_eq!(selection.transport.kind(), TransportKind::Local);
        let reason = selection.fallback_reason.expect("fallback reason");
        assert!(reason.starts_with("nats_unavailable:"), "reason: {reason}");
    }
}

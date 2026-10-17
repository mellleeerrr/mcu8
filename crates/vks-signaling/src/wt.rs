//! WebTransport/QUIC транспорт (SIGNALING_PROTOCOL — второй обязательный, R5).
//!
//! Эта волна — **абстракция + mock**: трейт `WebTransportSession` (порт)
//! и in-memory `MockWt` для тестов handler. Реальный QUIC-стек (`wtransport`)
//! подключается опционально (feature `wtransport`) — базовый билд
//! остаётся без I/O.

/// Транспорт-порт для WebTransport/QUIC (обёртка над байтовыми `Envelope`-фреймами).
///
/// Реализации: `MockWt` (in-memory, тесты) и future `WtSession`
/// (feature `wtransport`, реальный QUIC-стек).
pub trait WebTransportSession {
    /// Отправляет байты `Envelope` (один QUIC-datagram-фрейм).
    fn send(&mut self, bytes: Vec<u8>);

    /// Читает следующий `Envelope`-фрейм; `None` — транспорт закрыт.
    fn recv(&mut self) -> Option<Vec<u8>>;

    /// Закрытие транспорта (идемпотентно).
    fn close(&mut self);

    /// Транспорт закрыт?
    fn is_closed(&self) -> bool;
}

/// In-memory mock WebTransport (для тестов handler'а).
///
/// Один "конец" с двумя очередями: `sent` (всё, что шло на транспорт)
/// и `pending` (что ждёт `recv`). Позволяет тестам прописать
/// incoming-фрейменты и забирать исходящие.
#[derive(Debug, Default)]
pub struct MockWt {
    sent: Vec<Vec<u8>>,
    pending: std::collections::VecDeque<Vec<u8>>,
    closed: bool,
}

impl MockWt {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ставит incoming-фрейм в очередь на `recv`.
    pub fn enqueue(&mut self, bytes: Vec<u8>) {
        self.pending.push_back(bytes);
    }

    /// Выезжает все исходящие фреймы (в порядке отправки).
    #[must_use]
    pub fn take_sent(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.sent)
    }
}

impl WebTransportSession for MockWt {
    fn send(&mut self, bytes: Vec<u8>) {
        if self.closed {
            tracing::debug!("mock wt send after close, dropping frame");
            return;
        }
        self.sent.push(bytes);
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        if self.closed && self.pending.is_empty() {
            return None;
        }
        self.pending.pop_front()
    }

    fn close(&mut self) {
        self.closed = true;
    }

    fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(feature = "wtransport")]
pub mod wtransport_impl {
    use super::WebTransportSession;

    /// Заглушка реального QUIC-стек'а (feature `wtransport`).
    ///
    /// В этой волне — только интерфейс: держит endpoint/uri и реализует
    /// порт как no-op (реальное QUIC-hello + 0-RTT придёт вместе со
    /// стр0m/QUIC-стек'ом в фазе 2, когда появится `wtransport` crate
    /// в workspace).
    pub struct WtSession {
        endpoint: String,
        closed: bool,
    }

    impl WtSession {
        #[must_use]
        pub fn new(endpoint: impl Into<String>) -> Self {
            Self { endpoint: endpoint.into(), closed: false }
        }

        #[must_use]
        pub fn endpoint(&self) -> &str {
            &self.endpoint
        }
    }

    impl WebTransportSession for WtSession {
        fn send(&mut self, _bytes: Vec<u8>) {
            if !self.closed {
                tracing::debug!(endpoint = %self.endpoint, "wtransport send (stub)");
            }
        }

        fn recv(&mut self) -> Option<Vec<u8>> {
            if self.closed {
                None
            } else {
                tracing::debug!(endpoint = %self.endpoint, "wtransport recv (stub, waiting)");
                None
            }
        }

        fn close(&mut self) {
            self.closed = true;
        }

        fn is_closed(&self) -> bool {
            self.closed
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn wt_session_stub_send_recv_close() {
            let mut s = WtSession::new("wt://example.com:443");
            assert!(!s.is_closed());
            assert!(s.recv().is_none());
            s.send(vec![1, 2, 3]);
            s.close();
            assert!(s.is_closed());
            assert!(s.recv().is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_wt_roundtrip_and_close() {
        let mut m = MockWt::new();
        m.enqueue(vec![1, 2]);
        assert_eq!(m.recv(), Some(vec![1, 2]));
        m.send(vec![9, 9]);
        assert_eq!(m.take_sent(), vec![vec![9, 9]]);
        assert!(!m.is_closed());
        m.close();
        assert!(m.is_closed());
        assert_eq!(m.recv(), None);
    }

    #[test]
    fn mock_wt_send_after_close_is_dropped() {
        let mut m = MockWt::new();
        m.close();
        m.send(vec![7]);
        assert!(m.take_sent().is_empty());
    }
}

//! WebSocket транспорт (SIGNALING_PROTOCOL — основной, R5).
//!
//! Эта волна — **абстракция + mock**: трейт `WsSession` (порт) и
//! in-memory `MockWs` для тестов handler; реального I/O в базовом билде нет
//! (модель mock-портов, CODE_STYLE §4).

/// Транспорт-порт для WebSocket (обёртка над байтовыми `Envelope`-фреймами).
///
/// Реализация: `MockWs` (in-memory, тесты).
pub trait WsSession {
    /// Отправляет байты `Envelope` (один WS-binary-фрейм).
    fn send(&mut self, bytes: Vec<u8>);

    /// Читает следующий `Envelope`-фрейм; `None` — транспорт закрыт.
    fn recv(&mut self) -> Option<Vec<u8>>;

    /// Закрытие транспорта (идемпотентно).
    fn close(&mut self);

    /// Транспорт закрыт?
    fn is_closed(&self) -> bool;
}

/// In-memory mock WebSocket (для тестов handler'а).
///
/// Один "конец" с двумя очередями: `sent` (всё, что шло на транспорт)
/// и `pending` (что ждёт `recv`). Тесты ставят incoming-фрейменты
/// через `enqueue` и забирают исходящие через `take_sent`.
#[derive(Debug, Default)]
pub struct MockWs {
    sent: Vec<Vec<u8>>,
    pending: std::collections::VecDeque<Vec<u8>>,
    closed: bool,
}

impl MockWs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ставит incoming-фрейм в очередь на `recv`.
    pub fn enqueue(&mut self, bytes: Vec<u8>) {
        self.pending.push_back(bytes);
    }

    /// Забирает все исходящие фреймы (в порядке отправки).
    #[must_use]
    pub fn take_sent(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.sent)
    }
}

impl WsSession for MockWs {
    fn send(&mut self, bytes: Vec<u8>) {
        if self.closed {
            tracing::debug!("mock ws send after close, dropping frame");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_ws_roundtrip_and_close() {
        let mut m = MockWs::new();
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
    fn mock_ws_send_after_close_is_dropped() {
        let mut m = MockWs::new();
        m.close();
        m.send(vec![7]);
        assert!(m.take_sent().is_empty());
    }
}

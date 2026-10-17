//! Bandwidth estimation (TWCC).
//!
//! Вводятся transport-wide feedback-отчёты (RTCP Transport-Wide CC,
//! RFC 8888): по каждому пакету — arrival time. Из arrival-delta и
//! sent-timestamp-delta строится слоновое окно, из которого считается
//! оценка пропускной способности.
//!
//! Алгоритм детерминирован (без RNG/внутренних таймеров) — синтетический
//! feedback воспроизводит тот же bps. В реальном пайплайне (фаза 3) сюда
//! добавляются REMB (MEDIA_PIPELINE §2.4: `effective = min(remb, twcc)`)
//! и delay-based correction; здесь — baseline throughput-estimator.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Одна строка transport-wide CC feedback: packet sequence → arrival time.
///
/// `sent_seq` — transport-wide sequence (порядок отправки, gapless).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwccReportedPacket {
    /// Transport-wide sequence number пакета.
    pub sent_seq: u16,
    /// Время прибытия (монотонные часы стороны, приславшей report).
    pub arrival: Instant,
}

/// Конфигурация окна [`BweEstimator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BweConfig {
    /// Максимальное количество пакетов в окне.
    pub window_packets: usize,
    /// Максимальная длительность окна.
    pub window_duration: Duration,
    /// Стартовая оценка (bps) до первого feedback.
    pub start_bps: u64,
}

impl Default for BweConfig {
    fn default() -> Self {
        Self { window_packets: 150, window_duration: Duration::from_millis(500), start_bps: 500_000 }
    }
}

/// Opaque handle на feedback-report, возвращаный из [`BweEstimator::report`].
///
/// Хранит (1) отправленный диапазон в пределах текущей окна (2) количество
/// прибуждших пакетов (3) длительность окна (4) текуте оценку bps.
#[derive(Debug, Clone, PartialEq)]
pub struct BweResult {
    /// Оценка бпускной б (bps).
    pub bps: u64,
    /// Пакетов, прибужших за отчётное окно.
    pub packets_in_window: usize,
    /// Длительность окна, по которому считан estimate.
    pub window: Duration,
}

/// Простой, детерминированный TWCC bandwidth estimator.
///
/// Модель: throughput = `sent_bytes / window_time`. Мы не храним payload
/// (SFU forwarding: размер пакета фиксирован), поэтому каждый прибуждший
/// пакет считается `bytes_per_packet`; arrival times задают окно.
///
/// Window — скользящее: отбрасываются пакеты старше `window_duration`
/// либо превышающие `window_packets`.
#[derive(Debug, Clone)]
pub struct BweEstimator {
    config: BweConfig,
    /// Bytes на пакет (средний RTP packet size, включ. headers).
    bytes_per_packet: u64,
    /// Оконный буфер прибужших пакетов (arrival order).
    window: VecDeque<TwccReportedPacket>,
    /// Последний прибужший пакет (для delta времени).
    #[allow(dead_code)]
    last_arrival: Option<Instant>,
    /// Последний transport-wide seq.
    #[allow(dead_code)]
    last_sent_seq: Option<u16>,
    /// Текуте оценка bps.
    bps: u64,
}

impl Default for BweEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl BweEstimator {
    /// Создаёт estimator со стандартной конфигурацией.
    pub fn new() -> Self {
        Self::with_config(BweConfig::default())
    }

    /// Создаёт estimator с конфигурацией.
    pub fn with_config(config: BweConfig) -> Self {
        let start = config.start_bps;
        Self {
            config,
            bytes_per_packet: 1200,
            window: VecDeque::with_capacity(config.window_packets),
            last_arrival: None,
            last_sent_seq: None,
            bps: start,
        }
    }

    /// Задаёт средний размер RTP-пакета (bytes). Используется при расчёте
    /// throughput-оценки.
    pub fn set_bytes_per_packet(&mut self, bytes_per_packet: u64) {
        self.bytes_per_packet = bytes_per_packet.max(40);
    }

    /// Возвращает текуте оценку (ps).
    pub fn estimated_bps(&self) -> u64 {
        self.bps
    }

    /// Вводит один transport-wide CC report (batch прибужших пакетов).
    ///
    /// `arrival` — монотонные часы стороны, приславшей report (например
    /// `Instant::now()` на клиенте). Пакеты в `reported` — в порядке
    /// arrival; `sent_seq` — transport-wide sequence.
    ///
    /// Окно обновляется, б estimate пересчитывается как
    /// `window_bytes * 8 / window_time`.
    pub fn report(&mut self, reported: &[TwccReportedPacket]) -> BweResult {
        for p in reported {
            self.window.push_back(*p);
            self.last_arrival = Some(p.arrival);
            self.last_sent_seq = Some(p.sent_seq);
        }
        self.trim_window();
        self.recompute();
        let window = self.window_duration();
        let packets = self.window.len();
        BweResult { bps: self.bps, packets_in_window: packets, window }
    }

    /// Обнуляет состояние (новая сессия / reconnect).
    pub fn reset(&mut self) {
        self.window.clear();
        self.last_arrival = None;
        self.last_sent_seq = None;
        self.bps = self.config.start_bps;
    }

    fn trim_window(&mut self) {
        // Ограничение по числу пакетов.
        while self.window.len() > self.config.window_packets {
            self.window.pop_front();
        }
        // Ограничение по длительности: отбрасываем пакеты старше окна.
        if self.config.window_duration.is_zero() {
            return;
        }
        let window = self.config.window_duration;
        while let (Some(&front), Some(&back)) = (self.window.front(), self.window.back()) {
            if back.arrival.duration_since(front.arrival) <= window {
                break;
            }
            self.window.pop_front();
        }
    }

    fn recompute(&mut self) {
        // Нужен хотя бы один пакет с известным arrival time, иначе — start.
        if self.window.len() < 2 {
            self.bps = self.config.start_bps;
            return;
        }
        let window = self.window_duration();
        if window.is_zero() {
            self.bps = self.config.start_bps;
            return;
        }
        // Throughput: bytes за window_time, ×8 → bps.
        let bytes = self.window.len() as u64 * self.bytes_per_packet;
        let micros = window.as_micros().max(1);
        let bps = bytes * 8_000_000u64 / micros as u64;
        self.bps = bps;
    }

    fn window_duration(&self) -> Duration {
        match (self.window.front(), self.window.back()) {
            (Some(front), Some(back)) => back.arrival.duration_since(front.arrival),
            _ => Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Строит `n` пакетов с arrival-шагом `step_ms` (мс), начиная от `t0`.
    fn packets(n: usize, start_ms: u64, step_ms: u64, bytes: u64) -> Vec<TwccReportedPacket> {
        let _ = bytes;
        (0..n)
            .map(|i| TwccReportedPacket {
                sent_seq: i as u16,
                arrival: Instant::now() - Duration::from_millis(start_ms) + Duration::from_millis(i as u64 * step_ms),
            })
            .collect()
    }

    #[test]
    fn start_bps_until_feedback() {
        let mut e = BweEstimator::new();
        assert_eq!(e.estimated_bps(), 500_000);
        // Один пакет — окно < 2, оценка = start.
        e.report(&packets(1, 0, 20, 1200));
        assert_eq!(e.estimated_bps(), 500_000);
    }

    #[test]
    fn bps_tracks_arrival_rate() {
        // 1200 B @ 20 мс интервал = 1200*8/0.02 = 480 000 bps ≈ 480 kbps.
        let mut e = BweEstimator::new();
        let r = e.report(&packets(30, 0, 20, 1200));
        assert!(r.bps > 300_000 && r.bps < 700_000, "got {r:?}");
    }

    #[test]
    fn bps_rises_with_faster_arrival() {
        // Более быстрый arrival → более высокая оценка.
        let mut slow = BweEstimator::new();
        let mut fast = BweEstimator::new();
        slow.report(&packets(30, 0, 40, 1200)); // 25 кпс
        fast.report(&packets(30, 0, 10, 1200)); // 100 кпс
        assert!(fast.estimated_bps() > slow.estimated_bps());
    }

    #[test]
    fn bytes_per_packet_scales_estimate() {
        let mut small = BweEstimator::new();
        let mut large = BweEstimator::new();
        large.set_bytes_per_packet(4000);
        small.report(&packets(30, 0, 20, 1200));
        large.report(&packets(30, 0, 20, 4000));
        assert!(large.estimated_bps() > small.estimated_bps());
    }

    #[test]
    fn window_bounded_by_packets() {
        let mut e = BweEstimator::with_config(BweConfig { window_packets: 5, ..Default::default() });
        e.report(&packets(10, 0, 20, 1200));
        let r = e.report(&[]);
        assert!(r.packets_in_window <= 5, "got {r:?}");
    }

    #[test]
    fn window_bounded_by_duration() {
        let cfg = BweConfig { window_duration: Duration::from_millis(60), ..Default::default() };
        let mut e = BweEstimator::with_config(cfg);
        // 1000 мс @ 20 мс шаг = 50 пакетов, окно 60 мс → ≤ 4 пакета.
        e.report(&packets(50, 0, 20, 1200));
        let r = e.report(&[]);
        assert!(r.packets_in_window <= 4, "got {r:?}");
        assert!(r.window <= Duration::from_millis(60), "got {r:?}");
    }

    #[test]
    fn reset_restores_start_bps() {
        let mut e = BweEstimator::new();
        e.report(&packets(30, 0, 20, 1200));
        e.reset();
        assert_eq!(e.estimated_bps(), 500_000);
    }

    #[test]
    fn empty_reports_do_not_panic() {
        let mut e = BweEstimator::new();
        let r = e.report(&[]);
        assert_eq!(r.packets_in_window, 0);
        assert_eq!(r.bps, 500_000);
    }
}

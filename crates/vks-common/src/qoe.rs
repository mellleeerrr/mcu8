//! QoE aggregation: MOS/jitter/RTT/loss per participant (CLUSTERING.md:12).
//!
//! SLO v1: 99% QoE (MOS > 3.5), RTT p95 < 300ms.
//!
//! Что реально: per-participant сбор, p95, mean MOS, SLO evaluation.
//! Что мокнуто: реальные client QoE reports (здесь — параметризованные).

use std::collections::BTreeMap;

/// QoE report от участника.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QoeReport {
    /// MOS (1.0–5.0).
    pub mos: f64,
    /// Jitter, ms.
    pub jitter_ms: f64,
    /// RTT, ms.
    pub rtt_ms: f64,
    /// Packet loss, %.
    pub loss_pct: f64,
}

/// SLO threshold: MOS > 3.5 (CLUSTERING.md:12).
pub const MOS_THRESHOLD: f64 = 3.5;
/// SLO threshold: RTT p95 < 300ms.
pub const RTT_P95_MAX_MS: f64 = 300.0;
/// SLO: 99% of reports must satisfy the condition.
pub const QOE_SLO_PCT: f64 = 99.0;

/// Aggregator QoE reports per participant.
pub struct QoeAggregator {
    /// participant_id → Vec<QoeReport>.
    reports: BTreeMap<String, Vec<QoeReport>>,
}

impl QoeAggregator {
    /// Создаёт пустой aggregator.
    pub fn new() -> Self {
        Self { reports: BTreeMap::new() }
    }

    /// Add QoE report from participant.
    pub fn add_report(&mut self, participant_id: &str, report: QoeReport) {
        self.reports.entry(participant_id.to_string()).or_default().push(report);
    }

    /// Количество участников с reports.
    pub fn participant_count(&self) -> usize {
        self.reports.len()
    }

    /// Все reports (flat, sorted by participant then insertion order).
    fn all_reports(&self) -> Vec<QoeReport> {
        self.reports.values().flat_map(|v| v.iter().copied()).collect()
    }

    /// p95 RTT (percentile 95) в ms. Deterministic: sorted, index-based.
    pub fn p95_rtt(&self) -> Option<f64> {
        let mut rtts: Vec<f64> = self.reports.values().flat_map(|v| v.iter().map(|r| r.rtt_ms)).collect();
        if rtts.is_empty() {
            return None;
        }
        rtts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = (rtts.len() as f64 * 0.95) as usize;
        let idx = idx.min(rtts.len() - 1);
        Some(rtts[idx])
    }

    /// Mean MOS across all reports.
    pub fn mean_mos(&self) -> Option<f64> {
        let all = self.all_reports();
        if all.is_empty() {
            return None;
        }
        Some(all.iter().map(|r| r.mos).sum::<f64>() / all.len() as f64)
    }

    /// Процент reports с MOS > threshold.
    pub fn mos_above_threshold_pct(&self, threshold: f64) -> f64 {
        let all = self.all_reports();
        if all.is_empty() {
            return 100.0;
        }
        let above = all.iter().filter(|r| r.mos > threshold).count() as f64;
        above / all.len() as f64 * 100.0
    }

    /// SLO v1: 99% MOS > 3.5 AND RTT p95 < 300ms (CLUSTERING.md:12).
    pub fn slo_met(&self) -> bool {
        let mos_ok = self.mos_above_threshold_pct(MOS_THRESHOLD) >= QOE_SLO_PCT;
        let rtt_ok = self.p95_rtt().map(|v| v < RTT_P95_MAX_MS).unwrap_or(true);
        mos_ok && rtt_ok
    }
}

impl Default for QoeAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(mos: f64, rtt: f64) -> QoeReport {
        QoeReport { mos, jitter_ms: 10.0, rtt_ms: rtt, loss_pct: 1.0 }
    }

    #[test]
    fn p95_rtt_computed() {
        let mut agg = QoeAggregator::new();
        for i in 1..=100 {
            agg.add_report("p1", report(4.0, i as f64));
        }
        // p95 of 1..=100: idx = 100 * 0.95 = 95 → sorted[95] = 96.
        assert_eq!(agg.p95_rtt(), Some(96.0));
    }

    #[test]
    fn p95_empty_is_none() {
        let agg = QoeAggregator::new();
        assert_eq!(agg.p95_rtt(), None);
    }

    #[test]
    fn mean_mos() {
        let mut agg = QoeAggregator::new();
        agg.add_report("p1", report(4.0, 100.0));
        agg.add_report("p1", report(2.0, 100.0));
        assert_eq!(agg.mean_mos(), Some(3.0));
    }

    #[test]
    fn slo_met_when_all_good() {
        let mut agg = QoeAggregator::new();
        for _ in 0..100 {
            agg.add_report("p1", report(4.5, 100.0));
        }
        assert!(agg.slo_met());
    }

    #[test]
    fn slo_fail_low_mos() {
        let mut agg = QoeAggregator::new();
        // 98 good + 2 bad → 98% < 99% → fail.
        for _ in 0..98 {
            agg.add_report("p1", report(4.0, 100.0));
        }
        agg.add_report("p1", report(2.0, 100.0));
        agg.add_report("p1", report(2.0, 100.0));
        assert!(!agg.slo_met());
    }

    #[test]
    fn slo_fail_high_rtt() {
        let mut agg = QoeAggregator::new();
        // 100 reports, 5 at 500ms → p95 = 500 > 300 → fail.
        for _ in 0..95 {
            agg.add_report("p1", report(4.0, 100.0));
        }
        for _ in 0..5 {
            agg.add_report("p1", report(4.0, 500.0));
        }
        assert!(!agg.slo_met());
    }

    #[test]
    fn mos_above_threshold_pct() {
        let mut agg = QoeAggregator::new();
        agg.add_report("p1", report(4.0, 100.0));
        agg.add_report("p1", report(2.0, 100.0));
        assert_eq!(agg.mos_above_threshold_pct(3.5), 50.0);
    }
}

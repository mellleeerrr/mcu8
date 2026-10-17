//! SLO evaluation (CLUSTERING.md:12, DEVELOPMENT_PLAN.md Фаза 7).
//!
//! SLO v1: 99.5% signaling uptime, 99% QoE (MOS > 3.5), RTT p95 < 300ms,
//! room failover < 60s.
//!
//! Что реально: SLO evaluation из QoeAggregator + uptime, SLO struct.
//! Что мокнуто: реальные SLI источники (Prometheus queries), failover timer.

use crate::qoe::{QoeAggregator, MOS_THRESHOLD, QOE_SLO_PCT, RTT_P95_MAX_MS};

/// Target value for an SLO.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SloTarget {
    /// Percentage (e.g. 99.5% uptime).
    Percent(f64),
    /// Max ms (e.g. RTT p95 < 300ms).
    MaxMs(f64),
    /// MOS threshold (e.g. > 3.5).
    MosThreshold(f64),
}

impl std::fmt::Display for SloTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SloTarget::Percent(p) => write!(f, "{}%", p),
            SloTarget::MaxMs(ms) => write!(f, "< {}ms", ms),
            SloTarget::MosThreshold(m) => write!(f, "> {}%m", m),
        }
    }
}

/// Result of SLO evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct SloResult {
    /// Name of SLO (e.g. "signaling_uptime").
    pub name: String,
    /// Target.
    pub target: SloTarget,
    /// Actual value (e.g. 99.2%, 280ms, 4.1).
    pub actual: f64,
    /// Met or not.
    pub met: bool,
}

/// SLO evaluator: combines QoeAggregator + uptime + failover time.
pub struct SloEvaluator;

impl SloEvaluator {
    /// Evaluate all SLOs (CLUSTERING.md:12).
    ///
    /// - signaling uptime ≥ 99.5%
    /// - QoE: 99% MOS > 3.5
    /// - RTT p95 < 300ms
    /// - room failover < 60s
    pub fn evaluate(qoe: &QoeAggregator, signaling_uptime_pct: f64, room_failover_secs: Option<f64>) -> Vec<SloResult> {
        let mut out = Vec::new();

        // 1) Signaling uptime.
        let target = 99.5f64;
        out.push(SloResult {
            name: "signaling_uptime".to_string(),
            target: SloTarget::Percent(target),
            actual: signaling_uptime_pct,
            met: signaling_uptime_pct >= target,
        });

        // 2) QoE MOS > 3.5.
        let pct = qoe.mos_above_threshold_pct(MOS_THRESHOLD);
        out.push(SloResult {
            name: "qoe_mos".to_string(),
            target: SloTarget::MosThreshold(MOS_THRESHOLD),
            actual: pct,
            met: pct >= QOE_SLO_PCT,
        });

        // 3) RTT p95 < 300ms.
        let rtt = qoe.p95_rtt().unwrap_or(0.0);
        out.push(SloResult {
            name: "rtt_p95".to_string(),
            target: SloTarget::MaxMs(RTT_P95_MAX_MS),
            actual: rtt,
            met: rtt < RTT_P95_MAX_MS,
        });

        // 4) Room failover < 60s.
        if let Some(failover) = room_failover_secs {
            out.push(SloResult {
                name: "room_failover".to_string(),
                target: SloTarget::MaxMs(60.0),
                actual: failover,
                met: failover < 60.0,
            });
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(mos: f64, rtt: f64) -> crate::qoe::QoeReport {
        crate::qoe::QoeReport { mos, jitter_ms: 10.0, rtt_ms: rtt, loss_pct: 1.0 }
    }

    #[test]
    fn all_slos_met() {
        let mut qoe = QoeAggregator::new();
        for _ in 0..100 {
            qoe.add_report("p1", report(4.5, 100.0));
        }
        let results = SloEvaluator::evaluate(&qoe, 99.9, Some(30.0));
        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|r| r.met));
    }

    #[test]
    fn signaling_uptime_fail() {
        let qoe = QoeAggregator::new();
        let results = SloEvaluator::evaluate(&qoe, 99.0, None);
        assert_eq!(results.len(), 3);
        assert!(!results[0].met); // 99.0 < 99.5
    }

    #[test]
    fn rtt_p95_fail() {
        let mut qoe = QoeAggregator::new();
        for _ in 0..95 {
            qoe.add_report("p1", report(4.5, 100.0));
        }
        for _ in 0..5 {
            qoe.add_report("p1", report(4.5, 500.0));
        }
        let results = SloEvaluator::evaluate(&qoe, 99.9, None);
        assert!(!results[2].met); // rtt_p95
    }

    #[test]
    fn failover_not_reported_when_none() {
        let qoe = QoeAggregator::new();
        let results = SloEvaluator::evaluate(&qoe, 99.9, None);
        assert_eq!(results.len(), 3);
    }
}

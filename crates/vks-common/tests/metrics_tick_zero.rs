//! With `VKS_MCU_TICK_METRICS=1`, startup must materialise both phases at
//! count=0/sum=0 (a real zero state, not a synthetic `observe(0.0)` sample),
//! and `observe_mcu_tick` must record only the phase that actually ran.
use std::time::Duration;

use vks_common::metrics::{init_default, observe_mcu_tick, McuTickPhase};

fn tick_family() -> prometheus::proto::MetricFamily {
    prometheus::gather()
        .into_iter()
        .find(|family| family.name() == "vks_mcu_tick_duration_seconds")
        .expect("enabled startup must expose the MCU tick family")
}

fn histogram(phase: &str) -> prometheus::proto::Histogram {
    tick_family()
        .get_metric()
        .iter()
        .find(|metric| metric.get_label().iter().any(|label| label.name() == "phase" && label.value() == phase))
        .unwrap_or_else(|| panic!("phase {phase} must exist at startup"))
        .get_histogram()
        .as_ref()
        .expect("histogram payload")
        .clone()
}

#[test]
fn enabled_family_starts_at_zero_and_records_one_real_tick() {
    // Single-test binary: the `OnceLock` gate is first read below.
    std::env::set_var("VKS_MCU_TICK_METRICS", "1");
    init_default();

    for phase in ["audio", "video"] {
        let histogram = histogram(phase);
        assert_eq!(histogram.get_sample_count(), 0, "{phase} must start at count 0");
        assert_eq!(histogram.get_sample_sum(), 0.0, "{phase} must start at sum 0");
        assert!(histogram.get_bucket().iter().all(|bucket| bucket.cumulative_count() == 0));
    }

    observe_mcu_tick(McuTickPhase::Audio, Duration::from_millis(1));
    assert_eq!(histogram("audio").get_sample_count(), 1);
    assert_eq!(histogram("video").get_sample_count(), 0, "observing audio must not touch video");
    let audio = histogram("audio");
    let le_2ms = audio
        .get_bucket()
        .iter()
        .find(|bucket| (bucket.upper_bound() - 0.002).abs() < 1e-12)
        .expect("exact 2 ms bucket");
    assert_eq!(le_2ms.cumulative_count(), 1, "1 ms sample must fall in le=2ms");
    assert_eq!(audio.get_sample_sum(), 0.001);
}

//! The MCU tick histogram is opt-in: with `VKS_MCU_TICK_METRICS` unset the
//! family must be absent from the default registry, not present with zero
//! samples. This binary has a single test so removing the variable before the
//! process-wide `OnceLock` is first read is safe.
#[test]
fn mcu_tick_family_is_absent_by_default() {
    std::env::remove_var("VKS_MCU_TICK_METRICS");
    vks_common::metrics::init_default();
    let text = vks_common::metrics::encode();
    assert!(!text.contains("vks_mcu_tick_duration_seconds"), "default-off server must not expose the MCU tick family");
}

//! The MCU tick histogram is opt-in: with `VKS_MCU_TICK_METRICS` unset the
//! family must be absent from the default registry, not present with zero
//! samples, even if a caller records a tick. This binary has a single test so
//! removing the variable before the process-wide `OnceLock` is first read is
//! safe.
use std::time::Duration;

use vks_common::metrics::{init_default, observe_mcu_tick, McuTickPhase};

#[test]
fn mcu_tick_family_is_absent_by_default() {
    std::env::remove_var("VKS_MCU_TICK_METRICS");
    init_default();
    // Positive control: the export is non-empty and contains an always-on
    // family, so the absence assertion below cannot pass vacuously.
    let text = vks_common::metrics::encode();
    assert!(text.contains("vks_build_info"), "default export must contain build info:\n{text}");
    // Exercise the defence-in-depth gate inside `observe_mcu_tick`, not just
    // the startup materialisation.
    observe_mcu_tick(McuTickPhase::Audio, Duration::from_millis(1));
    observe_mcu_tick(McuTickPhase::Video, Duration::from_millis(1));
    let text = vks_common::metrics::encode();
    assert!(!text.contains("vks_mcu_tick_duration_seconds"), "default-off server must not expose the MCU tick family");
}

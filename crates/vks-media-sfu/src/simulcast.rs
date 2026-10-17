//! Simulcast selection (MEDIA_PIPELINE §2.3).
//!
//! Клиент шлёт 3 spatial layer (low/med/high) — разные SSRC.
//! `SimulcastSelector::select(bps)` выбирает слой:
//! High ≥ 800 kbps, Med ≥ 200 kbps, иначе Low.
//!
//! Анти-flapping: hysteresis — слой поднимается только когда bps
//! превышает threshold текущего слоя на `hysteresis` (default 5%).
//! При падении bps под threshold выбранного слоя — деградация без
//! hysteresis (быстрый отклик на congestion).

/// Пространственный слой simulcast (3 уровня).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimulcastLayer {
    /// Низкий (например, 320×180 @ 15fps).
    Low,
    /// Средний (например, 640×360 @ 30fps).
    Med,
    /// Высокий (например, 1280×720 @ 30fps).
    High,
}

impl SimulcastLayer {
    /// Название (для логов).
    pub fn as_str(&self) -> &'static str {
        match self {
            SimulcastLayer::Low => "low",
            SimulcastLayer::Med => "med",
            SimulcastLayer::High => "high",
        }
    }

    /// Threshold перехода на этот слой (bps): bps ≥ threshold → слой
    /// допустим. `Low` — 0 (всегда допустим).
    pub fn threshold_bps(&self) -> u64 {
        match self {
            SimulcastLayer::Low => 0,
            SimulcastLayer::Med => 200_000,
            SimulcastLayer::High => 800_000,
        }
    }

    /// Следующий вверх/вниз слой (None на краях).
    pub fn up(self) -> Option<Self> {
        match self {
            SimulcastLayer::Low => Some(SimulcastLayer::Med),
            SimulcastLayer::Med => Some(SimulcastLayer::High),
            SimulcastLayer::High => None,
        }
    }
}

/// Конфигурация [`SimulcastSelector`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SimulcastConfig {
    /// Hysteresis подъёма: bps должен быть > threshold × (1 + hysteresis),
    /// чтобы сменить слой вверх. 0.05 = +5%.
    pub hysteresis: f64,
    /// Стартовый слой (без feedback).
    pub initial: SimulcastLayer,
}

impl Default for SimulcastConfig {
    fn default() -> Self {
        Self { hysteresis: 0.05, initial: SimulcastLayer::Med }
    }
}

/// Выбранное состояние: текущий слой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Выбранный слой.
    pub layer: SimulcastLayer,
    /// Изменился ли слой за этот вызов (для PLI/FIR на keyframe,
    /// MEDIA_PIPELINE §2.3).
    pub changed: bool,
}

/// Simulcast-селектор: выбирает spatial layer по текущему bps.
///
/// Детерминирован: тот же бps + тот же internal state → тот же слой.
#[derive(Debug, Clone)]
pub struct SimulcastSelector {
    config: SimulcastConfig,
    current: SimulcastLayer,
}

impl Default for SimulcastSelector {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulcastSelector {
    /// Создаёт селектор со стандартной конфигурацией.
    pub fn new() -> Self {
        Self::with_config(SimulcastConfig::default())
    }

    /// Создаёт селектор с конфигурацией.
    pub fn with_config(config: SimulcastConfig) -> Self {
        Self { config, current: config.initial }
    }

    /// Текущий выбранный слой.
    pub fn current(&self) -> SimulcastLayer {
        self.current
    }

    /// Выбирает слой для `bps` (ps).
    ///
    /// Правила:
    ///
    /// - Если `bps` < threshold текущего layer — дегаурируем до первого
    ///   слоя, где `bps` ≥ threshold (быстро, без hysteresis).
    /// - Если `bps` ≥ threshold следующего слоя × (1 + hysteresis) —
    ///   поднимаем на один слой (hysteresis anti-flap).
    pub fn select(&mut self, bps: u64) -> Selection {
        let before = self.current;
        // Downgrade: bps упал под threshold текущего.
        while self.current != SimulcastLayer::Low && bps < self.current.threshold_bps() {
            self.current = downgrade(self.current);
        }
        // Upgrade: bps выше threshold следующего × (1+hysteresis), один шаг.
        if let Some(next) = self.current.up() {
            let needed = threshold_with_hysteresis(next.threshold_bps(), self.config.hysteresis);
            if bps >= needed {
                self.current = next;
            }
        }
        Selection { layer: self.current, changed: self.current != before }
    }

    /// Сбрасывает в initial layer (новая сущность).
    pub fn reset(&mut self) {
        self.current = self.config.initial;
    }
}

/// Сдвиг слоя на один вниз (Low → Low).
fn downgrade(layer: SimulcastLayer) -> SimulcastLayer {
    match layer {
        SimulcastLayer::High => SimulcastLayer::Med,
        SimulcastLayer::Med => SimulcastLayer::Low,
        SimulcastLayer::Low => SimulcastLayer::Low,
    }
}

/// Threshold с hysteresis: `threshold × (1 + h)` (u64).
fn threshold_with_hysteresis(threshold: u64, hysteresis: f64) -> u64 {
    if hysteresis <= 0.0 {
        return threshold;
    }
    (threshold as f64 * (1.0 + hysteresis)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_per_layer() {
        assert_eq!(SimulcastLayer::Low.threshold_bps(), 0);
        assert_eq!(SimulcastLayer::Med.threshold_bps(), 200_000);
        assert_eq!(SimulcastLayer::High.threshold_bps(), 800_000);
    }

    #[test]
    fn fresh_select_upgrades_to_high_upon_sufficient_bps() {
        let mut s = SimulcastSelector::new(); // initial Med
                                              // Мед @ 900 kbps ≥ 800k×1.05=840k → High.
        let sel = s.select(900_000);
        assert_eq!(sel.layer, SimulcastLayer::High);
        assert!(sel.changed);
    }

    #[test]
    fn fresh_select_keeps_med_below_high_threshold() {
        let mut s = SimulcastSelector::new(); // initial Med
                                              // 700 kbps < 840k hysteresis → остаётся Med.
        let sel = s.select(700_000);
        assert_eq!(sel.layer, SimulcastLayer::Med);
        assert!(!sel.changed);
    }

    #[test]
    fn downgrade_on_drop_without_hysteresis() {
        let mut s = SimulcastSelector::new();
        s.select(900_000); // High
                           // 150 kbps < 200k (Med threshold) → Low.
        let sel = s.select(150_000);
        assert_eq!(sel.layer, SimulcastLayer::Low);
        assert!(sel.changed);
    }

    #[test]
    fn downgrade_to_med_on_mid_drop() {
        let mut s = SimulcastSelector::new();
        s.select(900_000); // High
                           // 500 kbps ≥ 200k (Med) но < 800k (High) → Med.
        let sel = s.select(500_000);
        assert_eq!(sel.layer, SimulcastLayer::Med);
        assert!(sel.changed);
    }

    #[test]
    fn hysteresis_prevents_flap_around_threshold() {
        let mut s = SimulcastSelector::new(); // Med
                                              // Подъём на High: 900k ≥ 840k.
        s.select(900_000);
        // 820k ≥ 800k (High threshold) но < 840k hysteresis для Med→High…
        // High текуте, 820k ≥ 800k → остаётся High.
        let sel = s.select(820_000);
        assert_eq!(sel.layer, SimulcastLayer::High);
        assert!(!sel.changed);
        // 790k < 800k → дегаурирует на Med (без hysteresis при drop).
        let sel = s.select(790_000);
        assert_eq!(sel.layer, SimulcastLayer::Med);
        // Мед @ 810k < 840k (800k×1.05) → остаётся Med (hysteresis).
        let sel = s.select(810_000);
        assert_eq!(sel.layer, SimulcastLayer::Med);
        assert!(!sel.changed);
        // Мед @ 900k ≥ 840k → High.
        let sel = s.select(900_000);
        assert_eq!(sel.layer, SimulcastLayer::High);
        assert!(sel.changed);
    }

    #[test]
    fn low_is_always_allowed() {
        let mut s = SimulcastSelector::new();
        let sel = s.select(0);
        assert_eq!(sel.layer, SimulcastLayer::Low);
    }

    #[test]
    fn zero_hysteresis_upgrades_exactly_at_threshold() {
        let cfg = SimulcastConfig { hysteresis: 0.0, initial: SimulcastLayer::Low };
        let mut s = SimulcastSelector::with_config(cfg);
        // Low → Med: 200k ≥ 200k (no hysteresis) → Med.
        let sel = s.select(200_000);
        assert_eq!(sel.layer, SimulcastLayer::Med);
        // Med → High: 800k ≥ 800k → High.
        let sel = s.select(800_000);
        assert_eq!(sel.layer, SimulcastLayer::High);
    }

    #[test]
    fn reset_returns_to_initial() {
        let mut s = SimulcastSelector::new(); // Med
        s.select(900_000); // High
        s.reset();
        assert_eq!(s.current(), SimulcastLayer::Med);
    }
}

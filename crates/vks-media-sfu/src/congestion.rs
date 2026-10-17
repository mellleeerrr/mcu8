//! Congestion ladder (MEDIA_PIPELINE §2.5).
//!
//! Деграурация: spatial down (High→Med→Low) → temporal down → AudioOnly.
//! Восстановление с hysteresis (+5% от threshold) для anti-flap:
//! выход на следующий уровень вверх требует bps ≥ threshold × 1.05;
//! выход вниз — мгновенно при bps < threshold.

use crate::simulcast::SimulcastLayer;

/// Уровень congestion ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CongestionLevel {
    /// Полное качество: high spatial + полный temporal.
    Normal,
    /// Spatial down (high → med → low).
    DegradeSpatial(SimulcastLayer),
    /// Temporal down (drop temporal): видео на пониженной частоте.
    DegradeTemporal,
    /// Только аудио.
    AudioOnly,
}

impl CongestionLevel {
    /// Название (для логов).
    pub fn as_str(&self) -> &'static str {
        match self {
            CongestionLevel::Normal => "normal",
            CongestionLevel::DegradeSpatial(l) => l.as_str(),
            CongestionLevel::DegradeTemporal => "temporal",
            CongestionLevel::AudioOnly => "audio_only",
        }
    }
}

/// Конфигурация [`CongestionLadder`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CongestionConfig {
    /// Bps, при падении ниже которого — шаг вниз (деградация).
    ///
    /// Пороги в порядке: spatial high→med→low, temporal, audio-only.
    /// `degrade_thresholds` — порог деградации с уровня `i` на `i+1`.
    pub degrade_thresholds: [u64; 4],
    /// Hysteresis восстановления (0.05 = +5%).
    pub recovery_hysteresis: f64,
}

impl Default for CongestionConfig {
    fn default() -> Self {
        // Normal→SpatialMed @ <500k, SpatialMed→SpatialLow @ <250k,
        // SpatialLow→Temporal @ <120k, Temporal→AudioOnly @ <60k.
        Self { degrade_thresholds: [500_000, 250_000, 120_000, 60_000], recovery_hysteresis: 0.05 }
    }
}

/// Результат шага ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderStep {
    /// Новый уровень.
    pub level: CongestionLevel,
    /// Изменился ли уровень за этот вызов.
    pub changed: bool,
}

/// Congestion ladder: спускается поотосто при спте при падении bps,
/// поднимается с hysteresis.
#[derive(Debug, Clone)]
pub struct CongestionLadder {
    config: CongestionConfig,
    current: CongestionLevel,
}

impl Default for CongestionLadder {
    fn default() -> Self {
        Self::new()
    }
}

impl CongestionLadder {
    /// Создаёт ladder со стандартной конфигурацией.
    pub fn new() -> Self {
        Self::with_config(CongestionConfig::default())
    }

    /// Создаёт ladder с конфигурацией.
    pub fn with_config(config: CongestionConfig) -> Self {
        Self { config, current: CongestionLevel::Normal }
    }

    /// Текущий уровень.
    pub fn current(&self) -> CongestionLevel {
        self.current
    }

    /// Возвращает новый уровень для `bps` (ps).
    ///
    /// Дегаурация: пока `bps < degrade_threshold[level_index]` — шаг вниз.
    /// Восстановление: пока `bps ≥ threshold[level_index] × (1+hysteresis)`
    /// — шаг вверх.
    pub fn update(&mut self, bps: u64) -> LadderStep {
        let before = self.current;
        // Дегаурация (многошаговая, пока порог не пройден).
        while let Some((next, threshold)) = self.degrade_step(self.current) {
            if bps < threshold {
                self.current = next;
            } else {
                break;
            }
        }
        // Восстановление (один шаг вверх за вызов, anti-flap).
        if let Some((prev, threshold)) = self.recovery_step(self.current) {
            let needed = with_hysteresis(threshold, self.config.recovery_hysteresis);
            if bps >= needed {
                self.current = prev;
            }
        }
        LadderStep { level: self.current, changed: self.current != before }
    }

    /// Сброс на Normal (новая сущность / reconnect).
    pub fn reset(&mut self) {
        self.current = CongestionLevel::Normal;
    }

    /// Следующий вниз шаг + порог дегаурации с текущего уровня.
    ///
    /// `Normal → Med` использует порог `degrade_thresholds[0]`; `Med → Low`
    /// — `degrade_thresholds[1]`; `Low → Temporal` — `[2]`; `Temporal →
    /// AudioOnly` — `[3]`.
    fn degrade_step(&self, level: CongestionLevel) -> Option<(CongestionLevel, u64)> {
        match level {
            CongestionLevel::Normal => {
                Some((CongestionLevel::DegradeSpatial(SimulcastLayer::Med), self.config.degrade_thresholds[0]))
            }
            CongestionLevel::DegradeSpatial(SimulcastLayer::Med) => {
                Some((CongestionLevel::DegradeSpatial(SimulcastLayer::Low), self.config.degrade_thresholds[1]))
            }
            CongestionLevel::DegradeSpatial(_) => {
                Some((CongestionLevel::DegradeTemporal, self.config.degrade_thresholds[2]))
            }
            CongestionLevel::DegradeTemporal => Some((CongestionLevel::AudioOnly, self.config.degrade_thresholds[3])),
            CongestionLevel::AudioOnly => None,
        }
    }

    /// Предыдущий вверх шаг + порог восстановления в текущий уровень.
    ///
    /// Восстановление использует тот же порог, что и дегаурация с
    /// предыдущего уровня, но с +hysteresis (см. [`CongestionLadder::update`]).
    fn recovery_step(&self, level: CongestionLevel) -> Option<(CongestionLevel, u64)> {
        match level {
            CongestionLevel::Normal => None,
            CongestionLevel::DegradeSpatial(SimulcastLayer::Med) => {
                Some((CongestionLevel::Normal, self.config.degrade_thresholds[0]))
            }
            CongestionLevel::DegradeSpatial(SimulcastLayer::Low) => {
                Some((CongestionLevel::DegradeSpatial(SimulcastLayer::Med), self.config.degrade_thresholds[1]))
            }
            // High (недостижимо через ladder, но исчерпывающе) — recovery
            // к Med с порогом [1].
            CongestionLevel::DegradeSpatial(_) => {
                Some((CongestionLevel::DegradeSpatial(SimulcastLayer::Med), self.config.degrade_thresholds[1]))
            }
            CongestionLevel::DegradeTemporal => {
                Some((CongestionLevel::DegradeSpatial(SimulcastLayer::Low), self.config.degrade_thresholds[2]))
            }
            CongestionLevel::AudioOnly => Some((CongestionLevel::DegradeTemporal, self.config.degrade_thresholds[3])),
        }
    }
}

/// Порог с hysteresis: `t × (1 + h)`.
fn with_hysteresis(threshold: u64, hysteresis: f64) -> u64 {
    if hysteresis <= 0.0 {
        return threshold;
    }
    (threshold as f64 * (1.0 + hysteresis)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_normal() {
        let l = CongestionLadder::new();
        assert_eq!(l.current(), CongestionLevel::Normal);
    }

    #[test]
    fn degrades_spatial_then_temporal_then_audio() {
        let mut l = CongestionLadder::new();
        // <500k → SpatialMed.
        let s = l.update(400_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Med));
        assert!(s.changed);
        // <250k → SpatialLow.
        let s = l.update(200_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Low));
        assert!(s.changed);
        // <120k → Temporal.
        let s = l.update(100_000);
        assert_eq!(s.level, CongestionLevel::DegradeTemporal);
        assert!(s.changed);
        // <60k → AudioOnly.
        let s = l.update(40_000);
        assert_eq!(s.level, CongestionLevel::AudioOnly);
        assert!(s.changed);
        // Ещё ниже — остаётся AudioOnly.
        let s = l.update(1_000);
        assert_eq!(s.level, CongestionLevel::AudioOnly);
        assert!(!s.changed);
    }

    #[test]
    fn recovers_with_hysteresis_one_step_at_a_time() {
        let mut l = CongestionLadder::new();
        l.update(40_000); // AudioOnly
                          // AudioOnly→Temporal: 60k×1.05=63k. 70k ≥ 63k → Temporal.
        let s = l.update(70_000);
        assert_eq!(s.level, CongestionLevel::DegradeTemporal);
        // Temporal→SpatialLow: 120k×1.05=126k. 130k → SpatialLow.
        let s = l.update(130_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Low));
        // SpatialLow→SpatialMed: 250k×1.05=262.5k. 270k → SpatialMed.
        let s = l.update(270_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Med));
        // SpatialMed→Normal: 500k×1.05=525k. 530k → Normal.
        let s = l.update(530_000);
        assert_eq!(s.level, CongestionLevel::Normal);
    }

    #[test]
    fn hysteresis_blocks_recovery_at_exact_threshold() {
        let mut l = CongestionLadder::new();
        l.update(40_000); // AudioOnly
                          // 60k < 63k (hysteresis) — остаётся AudioOnly.
        let s = l.update(60_000);
        assert_eq!(s.level, CongestionLevel::AudioOnly);
        assert!(!s.changed);
        // 63k ≥ 63k → Temporal.
        let s = l.update(63_000);
        assert_eq!(s.level, CongestionLevel::DegradeTemporal);
    }

    #[test]
    fn no_flap_around_degrade_threshold() {
        let mut l = CongestionLadder::new();
        // 490k Normal (не деграурирует, 490k ≥ 500k? нет: 490k < 500k → Med).
        let s = l.update(490_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Med));
        // 500k: de-гратация не срабатывает (500k ≥ 500k), восстановление
        // требует 525k → остаётся Med.
        let s = l.update(500_000);
        assert_eq!(s.level, CongestionLevel::DegradeSpatial(SimulcastLayer::Med));
        assert!(!s.changed);
        // 525k → Normal.
        let s = l.update(525_000);
        assert_eq!(s.level, CongestionLevel::Normal);
    }

    #[test]
    fn reset_back_to_normal() {
        let mut l = CongestionLadder::new();
        l.update(40_000);
        l.reset();
        assert_eq!(l.current(), CongestionLevel::Normal);
    }

    #[test]
    fn level_names() {
        assert_eq!(CongestionLevel::Normal.as_str(), "normal");
        assert_eq!(CongestionLevel::AudioOnly.as_str(), "audio_only");
        assert_eq!(CongestionLevel::DegradeTemporal.as_str(), "temporal");
        assert_eq!(CongestionLevel::DegradeSpatial(SimulcastLayer::High).as_str(), "high");
    }
}

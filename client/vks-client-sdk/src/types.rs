//! SDK event types (для async-канал → Dioxus signals).
//!
//! В8 — `Conference` эмитит events через `flume`-async-канал;
//! Dioxus подписывается через signals.

/// События конференции (для UI).
#[derive(Debug, Clone, PartialEq)]
pub enum ConferenceEvent {
    /// Успешный join.
    Joined { room_id: String },
    /// Потеря соединения (auto-reconnect).
    ConnectionLost,
    /// Успешный reconnect.
    Reconnected,
    /// Изменение режима (SFU ↔ MCU).
    ModeChanged { mode: String },
    /// Новый участник.
    ParticipantJoined { id: String },
    /// Участник вышел.
    ParticipantLeft { id: String },
    /// Качество (MOS/RTT/loss).
    QualityReport { mos: f32, rtt_ms: u32, loss_pct: u8 },
    /// Ошибка.
    Error { code: String, message: String },
    /// Leave.
    Left,
}

impl ConferenceEvent {
    /// Человекочитаемое имя (для логов / UI).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Joined { .. } => "joined",
            Self::ConnectionLost => "connection_lost",
            Self::Reconnected => "reconnected",
            Self::ModeChanged { .. } => "mode_changed",
            Self::ParticipantJoined { .. } => "participant_joined",
            Self::ParticipantLeft { .. } => "participant_left",
            Self::QualityReport { .. } => "quality_report",
            Self::Error { .. } => "error",
            Self::Left => "left",
        }
    }
}

/// QoE-отчёт (MOS/RTT/jitter/loss).
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::excessive_precision)]
pub struct QualityReport {
    /// Mean Opinion Score (1-5).
    pub mos: f32,
    /// Round-trip time (ms).
    pub rtt_ms: u32,
    /// Jitter (ms).
    pub jitter_ms: u32,
    /// Packet loss (0-100).
    pub loss_pct: u8,
}

impl QualityReport {
    /// Порог «плохо» (MOS < 3).
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        self.mos < 3.0
    }

    /// Порог «очень плохо» (MOS < 2).
    #[must_use]
    pub fn is_bad(&self) -> bool {
        self.mos < 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names() {
        assert_eq!(ConferenceEvent::Joined { room_id: "r".into() }.name(), "joined");
        assert_eq!(ConferenceEvent::ConnectionLost.name(), "connection_lost");
        assert_eq!(ConferenceEvent::Reconnected.name(), "reconnected");
        assert_eq!(ConferenceEvent::ModeChanged { mode: "mcu".into() }.name(), "mode_changed");
        assert_eq!(ConferenceEvent::QualityReport { mos: 4.0, rtt_ms: 30, loss_pct: 1 }.name(), "quality_report");
        assert_eq!(ConferenceEvent::Left.name(), "left");
    }

    #[test]
    fn quality_degraded_thresholds() {
        assert!(!QualityReport { mos: 4.5, rtt_ms: 20, jitter_ms: 5, loss_pct: 0 }.is_degraded());
        assert!(QualityReport { mos: 2.5, rtt_ms: 200, jitter_ms: 50, loss_pct: 20 }.is_degraded());
        assert!(QualityReport { mos: 1.5, rtt_ms: 400, jitter_ms: 100, loss_pct: 40 }.is_bad());
    }
}

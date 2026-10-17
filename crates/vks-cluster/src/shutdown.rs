//! Graceful shutdown sequence (CLUSTERING.md §3, DEVELOPMENT_PLAN.md Фаза 7).
//!
//! Ordered steps: 1) StopAccepting, 2) Maintaining (new owner), 3) MediaDrain,
//! 4) StorageFlush, 5) Exit0. Timeout 30 ticks → mark `Killed`.
//!
//! Что реально: ordered execution, timeout detection, per-step reporting.
//! Что мокнуто: реальный RTP/DTLS flush, NATS queue drain, SIGKILL.

/// Шаг graceful shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownStep {
    /// 1. Stop accepting new connections.
    StopAccepting,
    /// 2. Active WS → ServerMaintaining{new_owner, grace_period}.
    Maintaining,
    /// 3. Media drain: RTP flush, DTLS close.
    MediaDrain,
    /// 4. Storage flush: NATS queue drain.
    StorageFlush,
    /// 5. Exit 0.
    Exit0,
}

impl ShutdownStep {
    /// Полная ordered sequence (CLUSTERING.md §3).
    pub fn sequence() -> [ShutdownStep; 5] {
        [
            ShutdownStep::StopAccepting,
            ShutdownStep::Maintaining,
            ShutdownStep::MediaDrain,
            ShutdownStep::StorageFlush,
            ShutdownStep::Exit0,
        ]
    }

    /// Название шага (для логов/метрик).
    pub fn name(&self) -> &'static str {
        match self {
            ShutdownStep::StopAccepting => "stop_accepting",
            ShutdownStep::Maintaining => "maintaining",
            ShutdownStep::MediaDrain => "media_drain",
            ShutdownStep::StorageFlush => "storage_flush",
            ShutdownStep::Exit0 => "exit0",
        }
    }
}

/// Результат одного шага.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepResult {
    /// Шаг выполнен успешно.
    Ok,
    /// Шаг не завершён (timeout или error) → Killed.
    Failed,
}

/// Итог shutdown sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// Все 5 шагов выполнены → Exit 0.
    Completed,
    /// Timeout 30 ticks → SIGKILL.
    Killed,
}

/// Записи о выполненных шагах.
#[derive(Debug, Clone, Default)]
pub struct ShutdownLog {
    /// (step, result) в порядке выполнения.
    pub steps: Vec<(ShutdownStep, StepResult)>,
}

/// Graceful shutdown sequencer.
pub struct ShutdownSequence {
    /// Timeout для всей последовательности (ticks).
    pub timeout_ticks: u64,
}

impl ShutdownSequence {
    /// Создаёт sequencer с timeout 30 ticks (CLUSTERING.md §3).
    pub fn new() -> Self {
        Self { timeout_ticks: 30 }
    }

    /// Созадёт sequencer с кастомным timeout.
    pub fn with_timeout(timeout_ticks: u64) -> Self {
        Self { timeout_ticks }
    }

    /// Запускает sequence. `f` вызывается для каждого шага по порядку.
    ///
    /// `f(step, now_tick) -> StepResult`. Если `f` вернул `Failed` на любом
    /// шаге → последовательность прерывается, итог `Killed`. Если все шаги
    /// `Ok` → `Completed`. Если total ticks >= timeout_ticks → `Killed`.
    ///
    /// Возврат: (outcome, log, elapsed_ticks).
    pub fn run<F>(&self, start_tick: u64, f: F) -> (ShutdownOutcome, ShutdownLog, u64)
    where
        F: Fn(ShutdownStep, u64) -> StepResult,
    {
        let mut log = ShutdownLog::default();
        let mut now = start_tick;
        let seq = ShutdownStep::sequence();

        for step in seq {
            // Tick advances each step by 1 (modeling real-time elapsed).
            now += 1;
            let result = f(step, now);
            log.steps.push((step, result));
            if result == StepResult::Failed {
                return (ShutdownOutcome::Killed, log, now - start_tick);
            }
            if now - start_tick >= self.timeout_ticks {
                return (ShutdownOutcome::Killed, log, now - start_tick);
            }
        }
        (ShutdownOutcome::Completed, log, now - start_tick)
    }
}

impl Default for ShutdownSequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_runs_all_five_in_order() {
        let seq = ShutdownSequence::new();
        let (outcome, log, elapsed) = seq.run(0, |_, _| StepResult::Ok);
        assert_eq!(outcome, ShutdownOutcome::Completed);
        assert_eq!(
            log.steps.iter().map(|(s, _)| s.name().to_string()).collect::<Vec<_>>(),
            vec![
                "stop_accepting".to_string(),
                "maintaining".to_string(),
                "media_drain".to_string(),
                "storage_flush".to_string(),
                "exit0".to_string(),
            ]
        );
        assert_eq!(log.steps.len(), 5);
        assert!(log.steps.iter().all(|(_, r)| *r == StepResult::Ok));
        assert_eq!(elapsed, 5);
    }

    #[test]
    fn failing_step_kills() {
        let seq = ShutdownSequence::new();
        let (outcome, log, _) =
            seq.run(0, |step, _| if step == ShutdownStep::MediaDrain { StepResult::Failed } else { StepResult::Ok });
        assert_eq!(outcome, ShutdownOutcome::Killed);
        // Steps before MediaDrain ran, MediaDrain failed, no more after.
        assert_eq!(log.steps.len(), 3);
        assert_eq!(log.steps[2].0, ShutdownStep::MediaDrain);
        assert_eq!(log.steps[2].1, StepResult::Failed);
    }

    #[test]
    fn timeout_kills_before_completion() {
        // Timeout 3 ticks: sequence has 5 steps → only 3 run.
        let seq = ShutdownSequence::with_timeout(3);
        let (outcome, log, _) = seq.run(0, |_, _| StepResult::Ok);
        assert_eq!(outcome, ShutdownOutcome::Killed);
        assert_eq!(log.steps.len(), 3);
    }

    #[test]
    fn sequence_constant_is_ordered() {
        let seq = ShutdownStep::sequence();
        assert_eq!(
            seq,
            [
                ShutdownStep::StopAccepting,
                ShutdownStep::Maintaining,
                ShutdownStep::MediaDrain,
                ShutdownStep::StorageFlush,
                ShutdownStep::Exit0,
            ]
        );
    }
}

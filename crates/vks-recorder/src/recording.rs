//! Запись комнаты: [`RecordingSession`], [`Segment`], [`RecordingMeta`].
//!
//! Источник: `docs/DEVELOPMENT_PLAN.md` Фаза 7 (recorder: композит + multi-track),
//! `docs/rules/BACKEND_RULES.md` §3.3 (encryption at rest, retention 90 дней).
//!
//! Ядро самодостаточное и детерминированное: время задаётся внешним
//! [`Clock`] (по умолчанию «замерзшее» в 0), кодирование/файлы/сети не
//! используются — сегменты несут только метаданные (`data_size`,
//! `duration_ms`), как описывает README.

use thiserror::Error;
use uuid::Uuid;

/// Время в миллисекундах (монотонная шкала сервера).
pub type ClockMs = u64;

/// Функция-часы: возвращает «сейчас» в мс. Параметризация времени
/// делает запись детерминированной в тестах.
pub type Clock = dyn Fn() -> ClockMs + Send + Sync;

/// Вид сегмента записи (DEVELOPMENT_PLAN.md Фаза 7).
///
/// - [`SegmentKind::CompositeVideo`] — одно MP4-композиционное видео (MCU).
/// - [`SegmentKind::Audio`] — общий аудиомикс (MCU) либо общий audio track (SFU).
/// - [`SegmentKind::TrackTrack`] — отдельный трек участника (SFU, multi-track).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// Композиционное видео (MCU, 1 MP4).
    CompositeVideo,
    /// Аудио (общий микс MCU или общий audio track SFU).
    Audio,
    /// Отдельный трек участника (SFU multi-track).
    TrackTrack,
}

impl SegmentKind {
    /// Машинно-читаемое имя (для логов/хранения).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompositeVideo => "composite_video",
            Self::Audio => "audio",
            Self::TrackTrack => "track_track",
        }
    }
}

/// Ошибка операций записи.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecordingError {
    /// Вызов [`RecordingSession::append_segment`] в состоянии
    /// [`RecordingState::Stopped`] (сегмент прилетел после `stop()`).
    #[error("recording is stopped")]
    Stopped,
    /// Вызов [`RecordingSession::stop`] в состоянии
    /// [`RecordingState::Idle`] (остановка записи, которая не начиналась).
    #[error("recording is not started")]
    NotStarted,
}

/// Состояние записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    /// Запись ещё не началась.
    Idle,
    /// Идёт запись; принимаются сегменты.
    Running,
    /// Запись остановлена; метаданные зафиксированы.
    Stopped,
}

/// Режим записи комнаты (DEVELOPMENT_PLAN.md Фаза 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecordingMode {
    /// MCU: единый композит — одно видео (1 MP4) + общий audio mix.
    Mcu,
    /// SFU: multi-track — отдельные файлы per participant (видео/аудио)
    /// + общий audio track.
    SFu,
}

impl RecordingMode {
    /// Машинно-читаемое имя (для логов/хранения).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mcu => "mcu",
            Self::SFu => "sfu",
        }
    }
}

/// Сегмент записи: атомарная единица данных, которая «вложена» в запись.
///
/// Ядро хранит только метаданные (`data_size`, `duration_ms`); бинарный
/// контент (MP4/chunks) живёт в реальном кодеке/S3 и не попадает в это ядро.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Порядковый номер сегмента внутри записи (0-based, строится самим
    /// [`RecordingSession`] — поле здесь только для внешних, уже
    /// пронумерованных сегментов).
    pub index: u64,
    /// Вид сегмента (композит / аудио / трек участника).
    pub kind: SegmentKind,
    /// Размер сегмента в байтах (для `size_bytes` метаданных).
    pub data_size: u64,
    /// Длительность сегмента в мс (вклад в `duration_ms`).
    pub duration_ms: u64,
}

impl Segment {
    /// Создаёт сегмент.
    #[must_use]
    pub const fn new(index: u64, kind: SegmentKind, data_size: u64, duration_ms: u64) -> Self {
        Self { index, kind, data_size, duration_ms }
    }
}

/// Метаданные завершённой записи (что «запись» несёт в `Storage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingMeta {
    /// Уникальный идентификатор записи (UUIDv4).
    pub id: Uuid,
    /// Комната, которая записывалась.
    pub room_id: Uuid,
    /// Режим записи (MCU-композит или SFU multi-track).
    pub mode: RecordingMode,
    /// Момент старта (мс).
    pub started_at_ms: ClockMs,
    /// Момент остановки (мс).
    pub ended_at_ms: ClockMs,
    /// Полная длительность записи (мс), равна `ended_at_ms - started_at_ms`.
    pub duration_ms: ClockMs,
    /// Количество сегментов.
    pub segments: usize,
    /// Общий размер записанных данных (байты, сумма `data_size`).
    pub size_bytes: u64,
}

impl RecordingMeta {
    /// Создаёт метаданные напрямую (для тестов/внешних источников).
    #[must_use]
    pub const fn new(
        id: Uuid,
        room_id: Uuid,
        mode: RecordingMode,
        started_at_ms: ClockMs,
        ended_at_ms: ClockMs,
        segments: usize,
        size_bytes: u64,
    ) -> Self {
        Self {
            id,
            room_id,
            mode,
            started_at_ms,
            ended_at_ms,
            duration_ms: ended_at_ms.saturating_sub(started_at_ms),
            segments,
            size_bytes,
        }
    }
}

/// Активная запись комнаты.
///
/// Детерминированный: часы принимаются как [`Clock`]; по умолчанию —
/// «замерзшие» в 0 (тесты могут передать `Box::new(|| 12_345)`).
pub struct RecordingSession {
    room_id: Uuid,
    mode: RecordingMode,
    state: RecordingState,
    segments: Vec<Segment>,
    started_at_ms: Option<ClockMs>,
    ended_at_ms: Option<ClockMs>,
    id: Option<Uuid>,
    cached_meta: Option<RecordingMeta>,
    container_writer: Option<crate::container::PlayableContainerWriter>,
    finalized_container: Option<Vec<u8>>,
    now_ms: Box<Clock>,
}

impl RecordingSession {
    /// Создаёт сессию с заданными часами. Начальное состояние —
    /// [`RecordingState::Idle`].
    pub fn new(room_id: Uuid, now_ms: Box<Clock>) -> Self {
        Self {
            room_id,
            mode: RecordingMode::Mcu,
            state: RecordingState::Idle,
            segments: Vec::new(),
            started_at_ms: None,
            ended_at_ms: None,
            id: None,
            cached_meta: None,
            container_writer: None,
            finalized_container: None,
            now_ms,
        }
    }

    /// Создаёт сессию с «замерзшим» в 0 часами (удобно в тестах,
    /// где время задаётся явными значениями).
    #[must_use]
    pub fn new_at_zero(room_id: Uuid) -> Self {
        Self::new(room_id, Box::new(|| 0))
    }

    /// Начинает запись в заданном режиме. В состоянии [`RecordingState::Running`]
    /// повторный `start` игнорируется (без ошибки).
    pub fn start(&mut self, mode: RecordingMode) -> Result<(), RecordingError> {
        if self.state == RecordingState::Running {
            return Ok(());
        }
        if self.state == RecordingState::Stopped {
            // Уже была запись — не перезапускаем (одна запись на комнату
            // за одну «жизнь» сессии; перезапуск — новая сессия).
            return Err(RecordingError::Stopped);
        }
        self.mode = mode;
        let now = (self.now_ms)();
        self.started_at_ms = Some(now);
        self.ended_at_ms = None;
        self.cached_meta = None;
        self.id = Some(Uuid::new_v4());
        self.state = RecordingState::Running;
        Ok(())
    }

    /// Прикрепляет к сессии генератор воспроизводимого MP4 контейнера.
    pub fn enable_container(&mut self, config: crate::container::ContainerConfig) {
        self.container_writer = Some(crate::container::PlayableContainerWriter::new(config));
    }

    /// Записывает видео-сэмпл в прикреплённый контейнер и сегмент в сессию.
    pub fn write_video_sample(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
        is_keyframe: bool,
    ) -> Result<(), crate::container::ContainerError> {
        if self.state != RecordingState::Running {
            return Err(crate::container::ContainerError::AlreadyFinalized);
        }
        if let Some(ref mut writer) = self.container_writer {
            writer.write_video_sample(data, timestamp_ms, is_keyframe)?;
        }
        let _ = self.append_segment(Segment::new(
            self.segments.len() as u64,
            SegmentKind::CompositeVideo,
            data.len() as u64,
            timestamp_ms,
        ));
        Ok(())
    }

    /// Записывает аудио-сэмпл в прикреплённый контейнер и сегмент в сессию.
    pub fn write_audio_sample(
        &mut self,
        data: &[u8],
        timestamp_ms: u64,
    ) -> Result<(), crate::container::ContainerError> {
        if self.state != RecordingState::Running {
            return Err(crate::container::ContainerError::AlreadyFinalized);
        }
        if let Some(ref mut writer) = self.container_writer {
            writer.write_audio_sample(data, timestamp_ms)?;
        }
        let _ = self.append_segment(Segment::new(
            self.segments.len() as u64,
            SegmentKind::Audio,
            data.len() as u64,
            timestamp_ms,
        ));
        Ok(())
    }

    /// Возвращает байты готового MP4 файла, если контейнер был подключён и запись остановлена.
    pub fn container_bytes(&self) -> Option<&[u8]> {
        self.finalized_container.as_deref()
    }

    /// Останавливает запись и возвращает финальные метаданные.
    /// Повторный вызов в состоянии [`RecordingState::Stopped`] идемпотентно
    /// возвращает зафиксированные метаданные без искажения времени.
    pub fn stop(&mut self) -> Result<RecordingMeta, RecordingError> {
        if self.state == RecordingState::Stopped {
            if let Some(ref meta) = self.cached_meta {
                return Ok(meta.clone());
            }
        }
        let started = self.started_at_ms.ok_or(RecordingError::NotStarted)?;
        let now = (self.now_ms)();
        let ended = now.max(started);
        let segments = self.segments.len();
        let size_bytes = self.segments.iter().map(|s| s.data_size).sum();
        let id = self.id.expect("id set on start");
        self.ended_at_ms = Some(ended);
        self.state = RecordingState::Stopped;

        if let Some(mut writer) = self.container_writer.take() {
            if let Ok(bytes) = writer.finalize() {
                self.finalized_container = Some(bytes);
            }
        }

        let meta = RecordingMeta {
            id,
            room_id: self.room_id,
            mode: self.mode,
            started_at_ms: started,
            ended_at_ms: ended,
            duration_ms: ended.saturating_sub(started),
            segments,
            size_bytes,
        };
        self.cached_meta = Some(meta.clone());
        Ok(meta)
    }

    /// Добавляет сегмент. В состоянии [`RecordingState::Stopped`] —
    /// ошибка [`RecordingError::Stopped`]. В [`RecordingState::Idle`] —
    /// ошибка [`RecordingError::NotStarted`].
    pub fn append_segment(&mut self, segment: Segment) -> Result<(), RecordingError> {
        match self.state {
            RecordingState::Running => {
                // Перенумеровываем индекс, если он не задан явно (0).
                let index = self.segments.len() as u64;
                let segment = Segment { index, ..segment };
                self.segments.push(segment);
                Ok(())
            }
            RecordingState::Idle => Err(RecordingError::NotStarted),
            RecordingState::Stopped => Err(RecordingError::Stopped),
        }
    }

    /// Текущее состояние.
    #[must_use]
    pub const fn state(&self) -> RecordingState {
        self.state
    }

    /// Комната сессии.
    #[must_use]
    pub const fn room_id(&self) -> Uuid {
        self.room_id
    }

    /// Режим сессии (значим после `start`).
    #[must_use]
    pub const fn mode(&self) -> RecordingMode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room() -> Uuid {
        Uuid::new_v4()
    }

    #[test]
    fn start_stop_produces_zero_duration_at_frozen_clock() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::Mcu).unwrap();
        let meta = s.stop().unwrap();
        assert_eq!(meta.room_id, s.room_id());
        assert_eq!(meta.mode, RecordingMode::Mcu);
        assert_eq!(meta.duration_ms, 0);
        assert_eq!(meta.segments, 0);
        assert_eq!(meta.size_bytes, 0);
        assert_eq!(meta.started_at_ms, 0);
        assert_eq!(meta.ended_at_ms, 0);
    }

    #[test]
    fn append_and_stop_aggregate_size_and_count() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::SFu).unwrap();
        s.append_segment(Segment::new(0, SegmentKind::TrackTrack, 100, 1_000)).unwrap();
        s.append_segment(Segment::new(0, SegmentKind::Audio, 50, 500)).unwrap();
        let meta = s.stop().unwrap();
        assert_eq!(meta.segments, 2);
        assert_eq!(meta.size_bytes, 150);
    }

    #[test]
    fn append_before_start_errors() {
        let mut s = RecordingSession::new_at_zero(room());
        let err = s.append_segment(Segment::new(0, SegmentKind::Audio, 10, 10)).unwrap_err();
        assert_eq!(err, RecordingError::NotStarted);
    }

    #[test]
    fn append_after_stop_errors() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::Mcu).unwrap();
        s.stop().unwrap();
        let err = s.append_segment(Segment::new(0, SegmentKind::Audio, 10, 10)).unwrap_err();
        assert_eq!(err, RecordingError::Stopped);
    }

    #[test]
    fn stop_without_start_errors() {
        let mut s = RecordingSession::new_at_zero(room());
        let err = s.stop().unwrap_err();
        assert_eq!(err, RecordingError::NotStarted);
    }

    #[test]
    fn restart_after_stop_errors() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::Mcu).unwrap();
        s.stop().unwrap();
        let err = s.start(RecordingMode::Mcu).unwrap_err();
        assert_eq!(err, RecordingError::Stopped);
    }

    #[test]
    fn start_while_running_is_noop() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::Mcu).unwrap();
        s.start(RecordingMode::Mcu).unwrap();
        assert_eq!(s.state(), RecordingState::Running);
    }

    #[test]
    fn segments_are_reindexed_in_order() {
        let mut s = RecordingSession::new_at_zero(room());
        s.start(RecordingMode::SFu).unwrap();
        s.append_segment(Segment::new(99, SegmentKind::TrackTrack, 10, 10)).unwrap();
        s.append_segment(Segment::new(99, SegmentKind::TrackTrack, 10, 10)).unwrap();
        let meta = s.stop().unwrap();
        assert_eq!(meta.segments, 2);
    }

    #[test]
    fn custom_clock_produces_expected_duration() {
        // Часы: первое обращение (start) → 0, второе (stop) → 1_500.
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls_box = std::sync::Arc::clone(&calls);
        let mut s = RecordingSession::new(
            room(),
            Box::new(move || {
                let n = calls_box.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    0
                } else {
                    1_500
                }
            }),
        );
        s.start(RecordingMode::Mcu).unwrap();
        s.append_segment(Segment::new(0, SegmentKind::CompositeVideo, 42, 1_500)).unwrap();
        let meta = s.stop().unwrap();
        assert_eq!(meta.duration_ms, 1_500);
        assert_eq!(meta.started_at_ms, 0);
        assert_eq!(meta.ended_at_ms, 1_500);
        assert_eq!(meta.size_bytes, 42);
        assert_eq!(meta.segments, 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn kind_and_mode_have_stable_strings() {
        assert_eq!(SegmentKind::CompositeVideo.as_str(), "composite_video");
        assert_eq!(SegmentKind::Audio.as_str(), "audio");
        assert_eq!(SegmentKind::TrackTrack.as_str(), "track_track");
        assert_eq!(RecordingMode::Mcu.as_str(), "mcu");
        assert_eq!(RecordingMode::SFu.as_str(), "sfu");
    }

    #[test]
    fn stop_is_idempotent_and_preserves_timestamps() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let calls_box = std::sync::Arc::clone(&calls);
        let mut s = RecordingSession::new(
            room(),
            Box::new(move || {
                let n = calls_box.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match n {
                    0 => 1_000,
                    1 => 3_000,
                    _ => 999_999, // Should NOT be called on subsequent stops
                }
            }),
        );
        s.start(RecordingMode::Mcu).unwrap();
        let meta1 = s.stop().unwrap();
        assert_eq!(meta1.started_at_ms, 1_000);
        assert_eq!(meta1.ended_at_ms, 3_000);
        assert_eq!(meta1.duration_ms, 2_000);

        // Second stop must be idempotent: returns exact same meta and doesn't advance clock
        let meta2 = s.stop().unwrap();
        assert_eq!(meta1, meta2);
        assert_eq!(meta2.started_at_ms, 1_000);
        assert_eq!(meta2.ended_at_ms, 3_000);
        assert_eq!(meta2.duration_ms, 2_000);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn meta_underflow_safety() {
        // ended < started must saturate to 0 rather than panic with integer underflow
        let meta = RecordingMeta::new(Uuid::new_v4(), room(), RecordingMode::Mcu, 5_000, 3_000, 0, 0);
        assert_eq!(meta.duration_ms, 0);
        assert_eq!(meta.started_at_ms, 5_000);
        assert_eq!(meta.ended_at_ms, 3_000);
    }
}

//! MCU engine — impl [`MediaEngine`](vks_core::MediaEngine) (MCU).
//!
//! In-memory MCU that wires [`Compositor`] + [`AudioMixer`] + [`Vad`] +
//! [`LayoutBridge`]. The `MediaEngine` trait is the only surface the core
//! and signaling talk to (ARCHITECTURE §2 MCU/SFU transparency): in MCU
//! mode every subscriber gets one composite track instead of N tracks.
//!
//! Frame source is pluggable (mock by default) so tests drive it: the
//! engine composites whatever frames the source provides for the
//! current slot rects at the current crossfade time.
//!
//! No `unsafe`; deterministic for a given input sequence.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use vks_core::ports::media_engine::{
    LayoutDescriptor, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
use vks_core::{ParticipantId, RoomId, RoomMode};
use vks_layout::GridLayout;
use vks_layout::{Canvas, Layout, LayoutKind};

use crate::audio_mixer::AudioMixer;
use crate::compositor::{Compositor, CANVAS_SIZE};
use crate::frame::Frame;
use crate::layout_bridge::{LayoutBridge, SlotRects};
use crate::vad::Vad;

/// MCU media rendering and backpressure statistics (M3.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McuRenderStats {
    /// Number of frames successfully composited and rendered.
    pub frames_rendered: u64,
    /// Number of frames dropped due to backpressure / deadline overrun.
    pub frames_dropped: u64,
    /// Duration of the last compositing pass (microseconds).
    pub last_duration_us: u64,
}

/// Per-room MCU state.
#[derive(Debug)]
struct RoomState {
    /// Режим комнаты (для отчёта через `MediaEngine::mode` при per-room override).
    #[allow(dead_code)]
    mode: RoomMode,
    caps: RoomCaps,
    /// Publisher → their tracks (kept for listing / future decode).
    publisher_tracks: BTreeMap<ParticipantId, Vec<MediaTrack>>,
    /// Порядок прихода паблишеров (S12): сетка и заполнение слотов идут по
    /// порядку прихода, а не по сортировке UUID.
    join_order: Vec<ParticipantId>,
    /// Subscribers (MCU: they all receive the single composite track).
    subscribers: BTreeSet<ParticipantId>,
    /// Applied layout descriptor (core-level, from apply_layout).
    layout: Option<LayoutDescriptor>,
    /// The layout the bridge is currently rendering (after crossfade).
    current_layout: Layout,
    /// The bridge (holds the previous/current rects for crossfade).
    bridge: LayoutBridge,
    /// Elapsed ms since the last layout change (drives the crossfade).
    elapsed_ms: u64,
    /// Monotonic timestamp of when layout was applied (ms).
    layout_applied_at_ms: Option<u64>,
    /// Monotonic timestamp of the last render pass (ms).
    last_render_ms: Option<u64>,
    /// Monotonic timestamp of the last audio processing pass (ms, 20ms cadence).
    last_audio_ms: Option<u64>,
    /// Rendering and backpressure statistics.
    stats: McuRenderStats,
    /// Last composed frame, per subscriber (mock: the same composite for
    /// every subscriber).
    composite: Arc<Frame>,
    /// Кадры участников по слотам (последний тик) — нужны персональной
    /// сборке кадра для самопросмотра (ADR-0029, фаза 2).
    frames: BTreeMap<u32, Arc<Frame>>,
    /// Самопросмотр включён в комнате (ADR-0029, фаза 2; по умолчанию выключен).
    self_view: bool,
    /// Audio mix of the current frame (full mix for recording/monitoring).
    mixed_audio: Vec<f32>,
    /// Per-participant N-1 mix-minus audio (without self-echo).
    participant_audio: BTreeMap<ParticipantId, Vec<f32>>,
    /// Per-participant VAD instances (independent debounce and energy tracking).
    vads: BTreeMap<ParticipantId, Vad>,
    /// Last committed speaking state per participant.
    speaking_states: BTreeMap<ParticipantId, bool>,
    /// Изменения состояния «говорит» (S12): список для внешнего потребителя
    /// (ростер админки + push `SpeakingStateChanged`).
    speaking_changes: Vec<(ParticipantId, bool)>,
    /// Current active/dominant speaker with silence hysteresis.
    active_speaker: Option<ParticipantId>,
    /// «Важный» участник (Codian §2.2): принудительно в главном окне, важнее
    /// активного спикера. Ставится администратором, снимается `None`.
    important: Option<ParticipantId>,
}

impl RoomState {
    fn new(mode: RoomMode, caps: RoomCaps) -> Self {
        Self {
            mode,
            caps,
            publisher_tracks: BTreeMap::new(),
            join_order: Vec::new(),
            subscribers: BTreeSet::new(),
            layout: None,
            current_layout: Layout::new(LayoutKind::Grid),
            bridge: LayoutBridge::new(),
            elapsed_ms: 0,
            layout_applied_at_ms: None,
            last_render_ms: None,
            last_audio_ms: None,
            stats: McuRenderStats::default(),
            frames: BTreeMap::new(),
            self_view: false,
            composite: Arc::new(Frame::new(
                CANVAS_SIZE.0,
                CANVAS_SIZE.1,
                vec![0u8; CANVAS_SIZE.0 as usize * CANVAS_SIZE.1 as usize],
            )),
            mixed_audio: Vec::new(),
            participant_audio: BTreeMap::new(),
            vads: BTreeMap::new(),
            speaking_states: BTreeMap::new(),
            speaking_changes: Vec::new(),
            active_speaker: None,
            important: None,
        }
    }

    /// The current (crossfaded) slot rects for the current layout.
    fn slot_rects(&self) -> SlotRects {
        self.bridge.current(self.elapsed_ms)
    }

    /// Builds a [`vks_layout::Layout`] from the core [`LayoutDescriptor`]:
    /// preserves assigned slot IDs and supports Grid, Focus, and Speaker layouts.
    fn layout_from_descriptor(desc: &LayoutDescriptor) -> Layout {
        let canvas = Canvas::new(CANVAS_SIZE.0, CANVAS_SIZE.1);
        let kind = match desc.layout_id.to_lowercase().as_str() {
            "focus" => LayoutKind::Focus,
            "speaker" => LayoutKind::Speaker,
            "custom" => LayoutKind::Custom,
            _ => LayoutKind::Grid,
        };

        let mut slot_to_pid = BTreeMap::new();
        for (pid, slot_id) in &desc.assignments {
            let vpid = vks_layout::ParticipantId::new(pid.as_uuid().as_u128());
            slot_to_pid.insert(*slot_id, vpid);
        }

        // Точная геометрия из PNM-раскладки (`preset_selected_b_4_(2)`,
        // `overlap`, `pip`): compositor рисует участника ровно в присланном
        // прямоугольнике. Без неё — прежняя авто-сетка (legacy/SFU).
        if !desc.geometry.is_empty() {
            let mut layout = Layout::new(LayoutKind::Custom);
            layout.version = desc.version;
            let mut cells = desc.geometry.clone();
            cells.sort_by_key(|cell| cell.slot);
            for cell in &cells {
                let x = cell.x.min(canvas.width.saturating_sub(1));
                let y = cell.y.min(canvas.height.saturating_sub(1));
                let width = cell.width.max(1).min(canvas.width.saturating_sub(x));
                let height = cell.height.max(1).min(canvas.height.saturating_sub(y));
                let pid = slot_to_pid.get(&cell.slot).copied();
                layout.slots.push(vks_layout::Slot::with_attributes(
                    cell.slot,
                    pid,
                    vks_layout::Rect::new(x, y, width, height),
                    cell.z_order as i32,
                    false,
                    false,
                ));
            }
            // Назначения вне geometry отбрасываются sanitize_assignments до
            // построения Layout: иначе участник получил бы full-canvas поверх
            // сетки (T03: никакого full-canvas для лишних паблишеров).
            return layout;
        }

        let max_slot = slot_to_pid.keys().copied().max().unwrap_or(0);
        let assigned_count = slot_to_pid.len();

        // M0.4 / M3.2: If arbitrary/sparse custom slot IDs are used (e.g. 7, 42),
        // compute tiles for assigned_count rather than creating 40+ holes.
        // For contiguous/dense 0..N slots (or when empty), use standard grid sizing.
        let is_sparse_custom = assigned_count > 0 && (max_slot as usize + 1) > assigned_count * 2;
        let total_slots =
            if is_sparse_custom { assigned_count } else { (max_slot as usize + 1).max(assigned_count).clamp(1, 128) };

        let rects = match kind {
            // B8: фокусная геометрия живёт в `vks-layout` — движок не держит
            // вторую копию (раньше `FocusLayout::compute` был мёртв, а эта
            // ветка повторяла его формулы).
            LayoutKind::Focus | LayoutKind::Speaker => vks_layout::FocusLayout::slot_rects(&canvas, total_slots as u32),
            _ => GridLayout::compute(&canvas, total_slots),
        };

        let mut layout = Layout::new(kind);
        layout.version = desc.version;

        if is_sparse_custom {
            for (idx, (&slot_id, &vpid)) in slot_to_pid.iter().enumerate() {
                let rect = rects.get(idx).copied().unwrap_or_else(|| canvas.as_rect());
                let is_focus = kind == LayoutKind::Focus && idx == 0;
                layout.slots.push(vks_layout::Slot::with_attributes(slot_id, Some(vpid), rect, 0, false, is_focus));
            }
        } else {
            for slot_id in 0..total_slots as u32 {
                let rect = rects.get(slot_id as usize).copied().unwrap_or_else(|| canvas.as_rect());
                let pid = slot_to_pid.get(&slot_id).copied();
                let is_focus = kind == LayoutKind::Focus && slot_id == 0;
                layout.slots.push(vks_layout::Slot::with_attributes(slot_id, pid, rect, 0, false, is_focus));
            }
            for (&slot_id, &vpid) in &slot_to_pid {
                if layout.slots.iter().all(|s| s.id.as_u32() != slot_id) {
                    layout.slots.push(vks_layout::Slot::new(slot_id, Some(vpid), canvas.as_rect()));
                }
            }
        }
        layout
    }

    /// Применяет descriptor к комнате: crossfade, current layout, версия.
    /// Одна точка для ensure_room/bridge/apply, чтобы не дублировать сброс
    /// elapsed/applied_at.
    fn adopt_layout(&mut self, layout: LayoutDescriptor) -> vks_layout::Layout {
        let new_layout = Self::layout_from_descriptor(&layout);
        self.bridge.apply(&new_layout);
        self.current_layout = new_layout.clone();
        self.layout = Some(layout);
        self.layout_applied_at_ms = None;
        self.elapsed_ms = 0;
        new_layout
    }

    /// Recomputes the composite + mixed audio + per-participant mix-minus + VAD from the current
    /// frames and participant audio (deterministic logical step of 33ms).
    fn tick(&mut self, frames: &BTreeMap<u32, Arc<Frame>>, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
        self.elapsed_ms += 33;
        self.process_media(frames, audio_inputs);
    }

    /// Advances the pipeline at a specific monotonic timestamp `now_ms`.
    /// Audio runs on a 20ms cadence, and video applies ~30 fps frame-dropping backpressure.
    fn tick_at(
        &mut self,
        now_ms: u64,
        frames: &BTreeMap<u32, Arc<Frame>>,
        audio_inputs: &[(ParticipantId, Vec<f32>)],
    ) -> bool {
        let (_ran, _events, _seating) = self.tick_audio_at(now_ms, audio_inputs);
        self.tick_video_at(now_ms, frames)
    }

    /// Advances the audio processing pass at a specific monotonic timestamp `now_ms` (20 ms cadence).
    fn tick_audio_at(
        &mut self,
        now_ms: u64,
        audio_inputs: &[(ParticipantId, Vec<f32>)],
    ) -> (bool, Vec<RoomEvent>, Option<SeatingMove>) {
        if let Some(last) = self.last_audio_ms {
            if now_ms < last + 18 {
                return (false, Vec::new(), None);
            }
        }
        self.last_audio_ms = Some(now_ms);
        let started = vks_common::metrics::mcu_tick_metrics_enabled().then(std::time::Instant::now);
        let (events, seating) = self.process_audio(audio_inputs);
        if let Some(started) = started {
            vks_common::metrics::observe_mcu_tick(vks_common::metrics::McuTickPhase::Audio, started.elapsed());
        }
        (true, events, seating)
    }

    /// Advances the video compositing pass at a specific monotonic timestamp `now_ms` (33 ms budget).
    fn tick_video_at(&mut self, now_ms: u64, frames: &BTreeMap<u32, Arc<Frame>>) -> bool {
        if let Some(last) = self.last_render_ms {
            if now_ms < last + 30 {
                self.stats.frames_dropped += 1;
                return false;
            }
        }
        self.last_render_ms = Some(now_ms);
        let applied_at = *self.layout_applied_at_ms.get_or_insert(now_ms);
        self.elapsed_ms = now_ms.saturating_sub(applied_at);
        let started = vks_common::metrics::mcu_tick_metrics_enabled().then(std::time::Instant::now);
        self.process_video(frames);
        if let Some(started) = started {
            vks_common::metrics::observe_mcu_tick(vks_common::metrics::McuTickPhase::Video, started.elapsed());
        }
        true
    }

    fn process_video(&mut self, frames: &BTreeMap<u32, Arc<Frame>>) {
        let t0 = std::time::Instant::now();
        let rects = self.slot_rects();
        self.composite = Arc::new(Compositor.compose(frames, &rects));
        // Кадры сохраняются для персональной сборки (самопросмотр): сам композит
        // уже собран, а личная дельта нужна отдельно.
        self.frames.clone_from(frames);
        self.stats.last_duration_us = t0.elapsed().as_micros() as u64;
        self.stats.frames_rendered += 1;
    }

    /// Пересадка главного окна по правилам Codian §2.2 (S12, ADR-0029 фаза 3).
    ///
    /// «Важный» участник важнее активного спикера; активный спикер важнее
    /// всех остальных. Раскладка перестраивается **по событию** смены спикера:
    /// если спикер уже в главном окне, `promote_to_main_window` возвращает
    /// `false` и версия не меняется.
    fn refresh_focus(&mut self) -> Option<(RoomEvent, SeatingMove)> {
        let desired = self.important.or(self.active_speaker)?;
        if !matches!(self.current_layout.kind, LayoutKind::Focus | LayoutKind::Speaker) {
            // Равная сетка и фиксированный макет PNM не имеют главного окна:
            // голосовая активация их не трогает (Codian §2.1: 52 макета
            // статичны по геометрии).
            return None;
        }
        let old_assignments: BTreeMap<ParticipantId, u32> = self
            .current_layout
            .slots
            .iter()
            .filter_map(|slot| {
                slot.participant_id
                    .map(|pid| (ParticipantId::from_uuid(uuid::Uuid::from_u128(pid.as_u128())), slot.id.as_u32()))
            })
            .collect();
        let layout_pid = vks_layout::ParticipantId::new(desired.as_uuid().as_u128());
        if !vks_layout::promote_to_main_window(&mut self.current_layout, layout_pid) {
            return None;
        }
        self.current_layout.bump_version();
        self.sync_descriptor_assignments();
        let event = RoomEvent::LayoutChanged { version: self.current_layout.version };
        let seating = SeatingMove { old_assignments, new_layout: self.current_layout.clone() };
        Some((event, seating))
    }

    /// Дескриптор обязан отражать фактическое заселение: иначе следующая
    /// `apply_layout` пересадит кадры по устаревшим назначениям (`rekey_frames`).
    fn sync_descriptor_assignments(&mut self) {
        let Some(descriptor) = self.layout.as_mut() else {
            return;
        };
        descriptor.assignments.clear();
        for slot in &self.current_layout.slots {
            if let Some(pid) = slot.participant_id {
                descriptor
                    .assignments
                    .insert(ParticipantId::from_uuid(uuid::Uuid::from_u128(pid.as_u128())), slot.id.as_u32());
            }
        }
        descriptor.version = self.current_layout.version;
    }

    fn process_audio(&mut self, audio_inputs: &[(ParticipantId, Vec<f32>)]) -> (Vec<RoomEvent>, Option<SeatingMove>) {
        let slices: Vec<&[f32]> = audio_inputs.iter().map(|(_, v)| v.as_slice()).collect();
        let target_len = self.mixed_audio.len().max(960);
        let (full_mix, minus_mixes) = AudioMixer.mix_and_mix_minus(&slices, target_len);
        self.mixed_audio = full_mix;

        // Mix-minus for each participant with audio
        self.participant_audio.clear();
        for ((pid, _), buf) in audio_inputs.iter().zip(minus_mixes.into_iter()) {
            self.participant_audio.insert(*pid, buf);
        }

        // For room subscribers and publishers who didn't publish audio in this tick (e.g. muted/listeners),
        // supply the room's mixed audio so they hear other participants.
        for sub in &self.subscribers {
            self.participant_audio.entry(*sub).or_insert_with(|| self.mixed_audio.clone());
        }
        for pub_id in self.publisher_tracks.keys() {
            self.participant_audio.entry(*pub_id).or_insert_with(|| self.mixed_audio.clone());
        }

        // Per-participant VAD and active speaker detection
        let mut highest_energy = 0.0f32;
        let mut current_speaker = None;
        let mut events = Vec::new();

        for (pid, buf) in audio_inputs {
            let vad = self.vads.entry(*pid).or_insert_with(|| Vad::new(crate::vad::VadConfig::default()));
            let mut frame = [0.0f32; 960];
            let mut sum_sq = 0.0f32;
            for (i, v) in buf.iter().take(960).enumerate() {
                if v.is_finite() {
                    frame[i] = *v;
                    sum_sq += v * v;
                }
            }
            vad.feed_frame(&frame);
            let is_speaking = vad.is_speaking();
            // Уровень для индикатора речи в клиенте: RMS 20 мс кадра,
            // 0..=1000 (протокол `SpeakingStateChanged.level`).
            let level = (sum_sq / 960.0).sqrt().clamp(0.0, 1.0).mul_add(1000.0, 0.0) as u32;
            // Первое наблюдение — это начальное состояние, а не изменение:
            // в список попадают только смены «говорит/молчит».
            if let Some(previous) = self.speaking_states.get(pid) {
                if *previous != is_speaking {
                    self.speaking_states.insert(*pid, is_speaking);
                    self.speaking_changes.push((*pid, is_speaking));
                    events.push(RoomEvent::Speaking { participant: *pid, speaking: is_speaking, level });
                }
            } else {
                self.speaking_states.insert(*pid, is_speaking);
            }

            if is_speaking && sum_sq > highest_energy {
                highest_energy = sum_sq;
                current_speaker = Some(*pid);
            }
        }

        // Active speaker with silence hysteresis (keep previous speaker if nobody is currently active)
        if let Some(spk) = current_speaker {
            self.active_speaker = Some(spk);
        }

        // Codian §2.2: главное окно следует за активным спикером (или за
        // «Важным» участником). Пересчёт — по событию смены спикера, а не
        // пересборка на каждом тике: если спикер уже в главном окне,
        // `refresh_focus` ничего не меняет.
        let seating = self.refresh_focus().map(|(event, seating)| {
            events.push(event);
            seating
        });
        (events, seating)
    }

    fn process_media(&mut self, frames: &BTreeMap<u32, Arc<Frame>>, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
        self.process_video(frames);
        let (_events, _seating) = self.process_audio(audio_inputs);
    }
}

/// Пересадка участников внутри раскладки: что было и что стало.
///
/// Нужна, чтобы перенести декодированные кадры вместе с участниками
/// ([`McuEngine::rekey_frames`]): кадр принадлежит участнику, а не номеру слота.
/// Без переноса после голосовой пересадки в окне остаётся видео прежнего
/// жильца — до следующего кадра этого участника, а если он выключил камеру,
/// то навсегда.
#[derive(Debug, Clone)]
struct SeatingMove {
    old_assignments: BTreeMap<ParticipantId, u32>,
    new_layout: vks_layout::Layout,
}

/// Событие комнаты для внешнего потребителя (S12, ADR-0029 фаза 3).
///
/// Движок не знает про сигналинг: он отдаёт события тому, кто их поставил
/// ([`McuEngine::set_event_sink`]), а сервер превращает их в
/// `SpeakingStateChanged` и `LayoutChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomEvent {
    /// Участник начал или закончил говорить; `level` — громкость кадра
    /// 0..=1000 (RMS), чтобы клиент мог нарисовать индикатор.
    Speaking { participant: ParticipantId, speaking: bool, level: u32 },
    /// Движок перестроил раскладку: главное окно переехало к новому спикеру
    /// или к «Важному» участнику (Codian §2.2).
    LayoutChanged { version: u32 },
}

/// Приёмник событий комнат. Сервер ставит его один раз на движок.
pub type EventSink = Arc<dyn Fn(RoomId, RoomEvent) + Send + Sync>;

/// Слот приёмника событий: `Arc<dyn Fn>` не реализует `Debug`, а движок
/// обязан оставаться `Debug` (его печатают структуры состояния и тесты).
#[derive(Default)]
struct SinkSlot(Mutex<Option<EventSink>>);

impl std::fmt::Debug for SinkSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SinkSlot").field("installed", &self.0.lock().map(|s| s.is_some()).unwrap_or(false)).finish()
    }
}

/// MCU engine (impl [`MediaEngine`]).
/// Room → (slot → frame): pluggable mock frame source.
type FrameSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<u32, Arc<Frame>>>>>;
/// Room → (participant → 20 ms PCM frame): pluggable mock audio source.
type AudioSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<ParticipantId, Vec<f32>>>>>;

#[derive(Debug, Default)]
pub struct McuEngine {
    rooms: Arc<Mutex<BTreeMap<RoomId, RoomState>>>,
    /// Pluggable frame source (mock by default).
    frames: FrameSource,
    /// Pluggable audio source (mock).
    audio: AudioSource,
    /// Раскладка, пришедшая до создания MCU-комнаты (сессия стартует раньше
    /// первого шлюзового вызова). Применяется при создании комнаты/бридже,
    /// не аллоцируя 2 МБ композита для пустой SFU-сессии.
    pending_layouts: Arc<Mutex<BTreeMap<RoomId, LayoutDescriptor>>>,
    /// Приёмник событий комнат (S12): ставится сервером один раз.
    event_sink: SinkSlot,
}

fn layout_has_video(tracks: &[MediaTrack]) -> bool {
    tracks
        .iter()
        .any(|track| matches!(track.kind, vks_core::track::TrackKind::Video | vks_core::track::TrackKind::Screen))
}

/// Свободный слот: только из geometry (T03), иначе legacy-скан u32.
fn free_layout_slot(layout: &LayoutDescriptor) -> Option<u32> {
    let used: std::collections::BTreeSet<u32> = layout.assignments.values().copied().collect();
    if layout.geometry.is_empty() {
        return (0..u32::MAX).find(|slot| !used.contains(slot));
    }
    layout.geometry.iter().map(|cell| cell.slot).filter(|slot| !used.contains(slot)).min()
}

fn seat_participant(layout: &mut LayoutDescriptor, pid: ParticipantId) -> bool {
    if layout.assignments.contains_key(&pid) {
        return false;
    }
    let Some(slot) = free_layout_slot(layout) else {
        tracing::warn!(%pid, "layout has no free geometry slot; participant stays unassigned");
        return false;
    };
    layout.assignments.insert(pid, slot);
    true
}

/// Убирает назначения вне присланной geometry: иначе `layout_from_descriptor`
/// нарисовал бы участника full-canvas поверх сетки. Дальше repair посадит его
/// в свободный geometry-слот, если он есть.
fn sanitize_assignments(layout: &mut LayoutDescriptor) {
    if layout.geometry.is_empty() {
        return;
    }
    let geometry: std::collections::BTreeSet<u32> = layout.geometry.iter().map(|cell| cell.slot).collect();
    layout.assignments.retain(|_, slot| geometry.contains(slot));
}

impl McuEngine {
    /// Creates an empty MCU engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Переносит кадры на новые номера слотов после смены раскладки.
    ///
    /// Кадр принадлежит участнику, а не позиции в сетке: без переноса после
    /// перестановки в окне остаётся видео прежнего участника (устаревший кадр).
    /// Переносятся только кадры, у которых есть новый слот.
    fn rekey_frames(
        &self,
        room: RoomId,
        old_assignments: &BTreeMap<ParticipantId, u32>,
        new_layout: &vks_layout::Layout,
    ) {
        // `vks_layout::ParticipantId` — тот же UUID, но в виде `u128`.
        let new_slots: BTreeMap<ParticipantId, u32> = new_layout
            .slots
            .iter()
            .filter_map(|slot| {
                slot.participant_id.map(|pid| {
                    let uuid = uuid::Uuid::from_u128(pid.as_u128());
                    (ParticipantId::from_uuid(uuid), slot.id.as_u32())
                })
            })
            .collect();
        if new_slots.is_empty() {
            return;
        }
        let mut guard = self.frames.lock().unwrap();
        let Some(frames) = guard.get_mut(&room) else { return };
        let mut moved: Vec<(u32, Arc<Frame>)> = Vec::new();
        for (pid, old_slot) in old_assignments {
            let Some(new_slot) = new_slots.get(pid) else { continue };
            if *new_slot == *old_slot {
                continue;
            }
            if let Some(frame) = frames.remove(old_slot) {
                moved.push((*new_slot, frame));
            }
        }
        for (slot, frame) in moved {
            frames.insert(slot, frame);
        }
    }

    /// Injects a (mock) frame for a slot of a room (test helper).
    pub fn set_frame(&self, room: RoomId, slot: u32, frame: Frame) {
        self.frames.lock().unwrap().entry(room).or_default().insert(slot, Arc::new(frame));
    }

    /// Injects a (mock) audio frame for a participant (test helper).
    pub fn set_audio(&self, room: RoomId, pid: ParticipantId, samples: Vec<f32>) {
        self.audio.lock().unwrap().entry(room).or_default().insert(pid, samples);
    }

    /// Injects a PCM audio slice for a participant reusing existing buffer capacity (zero-allocation hot path).
    pub fn set_audio_slice(&self, room: RoomId, pid: ParticipantId, samples: &[f32]) {
        let mut audio_guard = self.audio.lock().unwrap();
        let map = audio_guard.entry(room).or_default();
        let entry = map.entry(pid).or_insert_with(|| Vec::with_capacity(samples.len()));
        entry.clear();
        entry.extend_from_slice(samples);
    }

    /// Advances the room's pipeline (30 fps tick) and returns the composite
    /// frame. Deterministic for a given input sequence.
    pub fn tick(&self, room: RoomId) -> Option<Arc<Frame>> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room)?;
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let audio_map = self.audio.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let audio_inputs: Vec<(ParticipantId, Vec<f32>)> = state
            .publisher_tracks
            .keys()
            .filter_map(|pid| audio_map.get(pid).cloned().map(|samples| (*pid, samples)))
            .collect();
        state.tick(&frames, &audio_inputs);
        Some(state.composite.clone())
    }

    /// Advances the room's pipeline at a specific monotonic timestamp `now_ms`.
    /// Returns Some(Frame) if rendered, or None if dropped by backpressure.
    pub fn tick_at(&self, room: RoomId, now_ms: u64) -> Option<Arc<Frame>> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room)?;
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let audio_map = self.audio.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let audio_inputs: Vec<(ParticipantId, Vec<f32>)> = state
            .publisher_tracks
            .keys()
            .filter_map(|pid| audio_map.get(pid).cloned().map(|samples| (*pid, samples)))
            .collect();
        if state.tick_at(now_ms, &frames, &audio_inputs) {
            Some(state.composite.clone())
        } else {
            None
        }
    }

    /// Advances only the audio pipeline at a specific monotonic timestamp `now_ms` (20 ms cadence).
    /// Returns true if an audio tick was executed, or false if rejected by the 18 ms guard.
    pub fn tick_audio_at(&self, room: RoomId, now_ms: u64) -> bool {
        let (ran, events, seating) = {
            let mut g = self.rooms.lock().unwrap();
            let state = match g.get_mut(&room) {
                Some(s) => s,
                None => return false,
            };
            let audio_map = self.audio.lock().unwrap().get(&room).cloned().unwrap_or_default();
            let audio_inputs: Vec<(ParticipantId, Vec<f32>)> = state
                .publisher_tracks
                .keys()
                .filter_map(|pid| audio_map.get(pid).cloned().map(|samples| (*pid, samples)))
                .collect();
            state.tick_audio_at(now_ms, &audio_inputs)
        };
        // Кадр принадлежит участнику: после голосовой пересадки переносим
        // пиксели в новые слоты, иначе в главном окне остаётся видео прежнего
        // жильца (и навсегда, если тот выключил камеру).
        if let Some(seating) = seating {
            self.rekey_frames(room, &seating.old_assignments, &seating.new_layout);
        }
        self.emit(room, events);
        ran
    }

    /// Ставит приёмник событий комнат (S12: `SpeakingStateChanged` и
    /// `LayoutChanged`). Сервер зовёт это один раз; движок о сигналинге не знает.
    pub fn set_event_sink(&self, sink: EventSink) {
        *self.event_sink.0.lock().unwrap() = Some(sink);
    }

    /// Отдаёт события приёмнику. Вызывается **после** снятия блокировки комнат:
    /// приёмник может брать другие блокировки (ростер, шина комнаты).
    fn emit(&self, room: RoomId, events: Vec<RoomEvent>) {
        if events.is_empty() {
            return;
        }
        let sink = self.event_sink.0.lock().unwrap().clone();
        if let Some(sink) = sink {
            for event in events {
                sink(room, event);
            }
        }
    }

    /// «Важный» участник комнаты (Codian §2.2): принудительно в главном окне,
    /// важнее активного спикера. `None` возвращает режим голосовой активации.
    ///
    /// Возвращает `true`, если раскладка перестроилась.
    pub fn set_important_participant(&self, room: RoomId, participant: Option<ParticipantId>) -> bool {
        let changed = {
            let mut g = self.rooms.lock().unwrap();
            let Some(state) = g.get_mut(&room) else {
                return false;
            };
            state.important = participant;
            state.refresh_focus()
        };
        let Some((event, seating)) = changed else {
            return false;
        };
        self.rekey_frames(room, &seating.old_assignments, &seating.new_layout);
        self.emit(room, vec![event]);
        true
    }

    /// «Важный» участник комнаты (`None` — не назначен).
    pub fn important_participant(&self, room: RoomId) -> Option<ParticipantId> {
        self.rooms.lock().unwrap().get(&room).and_then(|state| state.important)
    }

    /// Advances only the video pipeline at a specific monotonic timestamp `now_ms` (33 ms budget).
    /// Returns Some(Frame) if rendered, or None if dropped by frame-dropping backpressure.
    pub fn tick_video_at(&self, room: RoomId, now_ms: u64) -> Option<Arc<Frame>> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room)?;
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        if state.tick_video_at(now_ms, &frames) {
            Some(state.composite.clone())
        } else {
            None
        }
    }

    /// Кадровый тик + кадр для конкретного зрителя.
    ///
    /// То, что нужно egress'у: конвейер продвигается один раз, а кадр
    /// выбирается для конкретного получателя (ADR-0029, фаза 0: общий кадр).
    pub fn tick_video_for_viewer(&self, room: RoomId, now_ms: u64, viewer: ParticipantId) -> Option<Arc<Frame>> {
        match self.tick_video_at(room, now_ms) {
            Some(frame) => Some(frame),
            None => self.frame_for_viewer(room, viewer),
        }
    }

    /// Кадр для конкретного зрителя.
    ///
    /// Фаза 2 ADR-0029 («Continuous Presence»): по умолчанию самопросмотр
    /// выключен, и зритель получает **тот же** `Arc<Frame>`, что и все
    /// остальные (нулевые копии; байты не меняются). Когда самопросмотр
    /// включён, персональный кадр собирается из общего: ячейка зрителя в
    /// общей сетке освобождается, а его камера рисуется в ячейку низшего
    /// приоритета (последнюю по z-порядку) — ровно то правило, что описано
    /// в спецификации Codian §2 («собственный видеопоток смотрящего не
    /// учитывается в сетке, самопросмотр — низший приоритет»).
    pub fn frame_for_viewer(&self, room: RoomId, viewer: ParticipantId) -> Option<Arc<Frame>> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room)?;
        let base = state.composite.clone();
        if !state.self_view {
            return Some(base);
        }
        let rects = state.slot_rects();
        // Ячейка зрителя в общей сетке.
        // Идентификаторы участников в раскладке и в ядре — разные типы с одним
        // UUID, поэтому сравниваем по нему.
        let viewer_uuid = viewer.as_uuid().as_u128();
        let own_slot = state
            .current_layout
            .slots
            .iter()
            .find(|slot| slot.participant_id.map(|pid| pid.as_u128()) == Some(viewer_uuid))
            .map(|s| s.id.as_u32());
        let Some(own_slot) = own_slot else { return Some(base) };
        // Ячейка самопросмотра — низший приоритет: последняя по (z, id).
        let Some(self_slot) = state
            .current_layout
            .slots
            .iter()
            .max_by_key(|slot| (slot.z_index, slot.id.as_u32()))
            .map(|slot| slot.id.as_u32())
        else {
            return Some(base);
        };
        if self_slot == own_slot {
            // Зритель уже стоит в ячейке низшего приоритета — дельта пуста.
            return Some(base);
        }
        let Some(own_rect) = rects.get(&own_slot) else { return Some(base) };
        let Some(self_rect) = rects.get(&self_slot) else { return Some(base) };
        let mut canvas = Frame::new(base.width, base.height, base.data.clone());
        Compositor::clear(&mut canvas, own_rect);
        if let Some(camera) = state.frames.get(&own_slot) {
            Compositor::blit(&mut canvas, camera, self_rect);
        }
        Some(Arc::new(canvas))
    }

    /// Включает самопросмотр в комнате (ADR-0029, фаза 2).
    ///
    /// По умолчанию выключен: общий кадр не меняется, поведение комнаты
    /// остаётся прежним до явного включения.
    pub fn set_self_view(&self, room: RoomId, enabled: bool) {
        let mut g = self.rooms.lock().unwrap();
        if let Some(state) = g.get_mut(&room) {
            state.self_view = enabled;
        }
    }

    /// Returns the rendering and backpressure stats of a room (M3.6).
    pub fn stats(&self, room: RoomId) -> Option<McuRenderStats> {
        let g = self.rooms.lock().unwrap();
        g.get(&room).map(|s| s.stats)
    }

    /// Applied layout descriptor (test/inspector), including staged pending
    /// layouts for rooms that do not exist yet.
    pub fn layout_descriptor(&self, room: RoomId) -> Option<LayoutDescriptor> {
        if let Some(layout) = self.rooms.lock().unwrap().get(&room).and_then(|state| state.layout.clone()) {
            return Some(layout);
        }
        self.pending_layouts.lock().unwrap().get(&room).cloned()
    }

    /// Эффективная раскладка комнаты — ровно та, что рисует композитор:
    /// с вычисленной авто-сеткой, с клампом прямоугольников в холст и с
    /// участниками, разложенными по слотам.
    ///
    /// Источник истины для рассылки раскладки клиентам: дескриптор может
    /// прийти без геометрии (режим auto), и тогда «провод» и «композит»
    /// разошлись бы.
    pub fn current_layout(&self, room: RoomId) -> Option<vks_layout::Layout> {
        if let Some(state) = self.rooms.lock().unwrap().get(&room) {
            return Some(state.current_layout.clone());
        }
        // Комната ещё не создана: раскладка лежит в pending и будет применена
        // при первом бридже. Клиент всё равно должен получить её снимок.
        let pending = self.pending_layouts.lock().unwrap().get(&room).cloned()?;
        Some(RoomState::layout_from_descriptor(&pending))
    }

    /// The current crossfaded slot rects of a room (test/inspector).
    pub fn slot_rects(&self, room: RoomId) -> Option<SlotRects> {
        self.rooms.lock().unwrap().get(&room).map(|s| s.slot_rects())
    }

    /// Returns the personal N-1 mix-minus audio for a participant in the room.
    pub fn participant_audio(&self, room: RoomId, pid: ParticipantId) -> Option<Vec<f32>> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room)?;
        state.participant_audio.get(&pid).cloned().or_else(|| {
            if state.subscribers.contains(&pid) || state.publisher_tracks.contains_key(&pid) {
                Some(state.mixed_audio.clone())
            } else {
                None
            }
        })
    }

    /// Copies the personal N-1 mix-minus audio into a mutable slice without heap allocation.
    pub fn copy_participant_audio(&self, room: RoomId, pid: ParticipantId, out: &mut [f32]) -> bool {
        let g = self.rooms.lock().unwrap();
        let state = match g.get(&room) {
            Some(s) => s,
            None => return false,
        };
        if let Some(src) = state.participant_audio.get(&pid) {
            let n = src.len().min(out.len());
            out[..n].copy_from_slice(&src[..n]);
            true
        } else if (state.subscribers.contains(&pid) || state.publisher_tracks.contains_key(&pid))
            && !state.mixed_audio.is_empty()
        {
            let n = state.mixed_audio.len().min(out.len());
            out[..n].copy_from_slice(&state.mixed_audio[..n]);
            true
        } else {
            false
        }
    }

    /// Returns the global room audio mix (for recording / monitoring).
    pub fn mixed_audio(&self, room: RoomId) -> Option<Vec<f32>> {
        let g = self.rooms.lock().unwrap();
        g.get(&room).map(|s| s.mixed_audio.clone())
    }

    /// Returns the active speaker determined by per-participant VAD.
    /// Забирает накопленные изменения состояния «говорит» (S12).
    ///
    /// Потребитель (сервер) дренирует список после audio-тика и обновляет
    /// ростер + рассылает `SpeakingStateChanged`: без этого состояние
    /// «говорит» не покидало бы движок.
    pub fn take_speaking_changes(&self, room: RoomId) -> Vec<(ParticipantId, bool)> {
        let mut g = self.rooms.lock().unwrap();
        g.get_mut(&room).map(|state| std::mem::take(&mut state.speaking_changes)).unwrap_or_default()
    }

    pub fn active_speaker(&self, room: RoomId) -> Option<ParticipantId> {
        self.rooms.lock().unwrap().get(&room).and_then(|s| s.active_speaker)
    }

    /// Returns whether the given participant is currently speaking.
    pub fn is_speaking(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.rooms.lock().unwrap().get(&room).and_then(|s| s.speaking_states.get(&pid).copied()).unwrap_or(false)
    }

    /// Ensures a room exists in the MCU engine.
    pub fn ensure_room(&self, room: RoomId) {
        let mut pending = self.pending_layouts.lock().unwrap().remove(&room);
        if let Some(layout) = pending.as_mut() {
            sanitize_assignments(layout);
        }
        let mut g = self.rooms.lock().unwrap();
        let state = g.entry(room).or_insert_with(|| RoomState::new(RoomMode::Mcu, RoomCaps::default()));
        // Pending применяется только к пустой комнате: более новая раскладка
        // не должна откатываться stale-значением.
        if state.layout.is_none() {
            if let Some(layout) = pending {
                state.adopt_layout(layout);
            }
        }
    }

    /// Synchronously bridges a participant into the MCU room as both subscriber and publisher.
    pub fn bridge_participant(&self, room: RoomId, pid: ParticipantId, tracks: Vec<MediaTrack>) {
        // Pending забираем до блокировки rooms, чтобы не было инверсии порядка
        // с apply_layout (rooms → pending).
        let pending = self.pending_layouts.lock().unwrap().remove(&room);
        let mut g = self.rooms.lock().unwrap();
        let state = g.entry(room).or_insert_with(|| RoomState::new(RoomMode::Mcu, RoomCaps::default()));
        state.subscribers.insert(pid);
        // Pending применяем и к audio-only bridge: SIP/H.323 может прийти без
        // видео (или видео деградировало), но выбранная до дозвона раскладка
        // не должна теряться. Видео-участник затем сядет в её geometry-слот.
        if state.layout.is_none() {
            if let Some(layout) = pending {
                state.adopt_layout(layout);
            }
        }
        Self::register_publisher(state, pid, tracks);
    }

    /// Registers a video publisher and seats it in a free layout slot **without**
    /// making it a subscriber of the composite.
    ///
    /// Нужен транспорту WebRTC: браузер получает медиа через собственный
    /// fanout (str0m), а не через композит MCU, — но он обязан занимать слот
    /// раскладки, иначе `set_participant_frame` молча выбросит его кадры.
    pub fn seat_publisher(&self, room: RoomId, pid: ParticipantId, tracks: Vec<MediaTrack>) {
        let pending = self.pending_layouts.lock().unwrap().remove(&room);
        let mut g = self.rooms.lock().unwrap();
        let state = g.entry(room).or_insert_with(|| RoomState::new(RoomMode::Mcu, RoomCaps::default()));
        if state.layout.is_none() {
            if let Some(layout) = pending {
                state.adopt_layout(layout);
            }
        }
        Self::register_publisher(state, pid, tracks);
    }

    /// Регистрирует паблишера и сажает его в свободный слот раскладки.
    fn register_publisher(state: &mut RoomState, pid: ParticipantId, tracks: Vec<MediaTrack>) {
        if tracks.is_empty() {
            return;
        }
        let has_video = layout_has_video(&tracks);
        state.publisher_tracks.insert(pid, tracks);
        if !state.join_order.contains(&pid) {
            state.join_order.push(pid);
        }
        if !has_video {
            return;
        }
        let descriptor = state.layout.get_or_insert_with(|| LayoutDescriptor {
            layout_id: "grid".into(),
            version: 0,
            assignments: BTreeMap::new(),
            geometry: Vec::new(),
        });
        if seat_participant(descriptor, pid) {
            descriptor.version = descriptor.version.saturating_add(1);
            let layout = RoomState::layout_from_descriptor(descriptor);
            state.bridge.apply(&layout);
            state.current_layout = layout;
            state.layout_applied_at_ms = None;
            state.elapsed_ms = 0;
        }
    }

    /// Writes a decoded frame only into the slot belonging to this publisher.
    pub fn set_participant_frame(&self, room: RoomId, pid: ParticipantId, frame: Frame) -> bool {
        let rooms = self.rooms.lock().unwrap();
        let Some(state) = rooms.get(&room) else {
            return false;
        };
        let Some(slot) = state.layout.as_ref().and_then(|layout| layout.assignments.get(&pid)) else {
            return false;
        };
        self.frames.lock().unwrap().entry(room).or_default().insert(*slot, Arc::new(frame));
        true
    }

    /// Removes the participant's decoded pixels immediately when reception is muted.
    pub fn clear_participant_frame(&self, room: RoomId, pid: ParticipantId) {
        let mut rooms = self.rooms.lock().unwrap();
        let Some(state) = rooms.get_mut(&room) else {
            return;
        };
        if let Some(slot) = state.layout.as_ref().and_then(|layout| layout.assignments.get(&pid)) {
            self.frames.lock().unwrap().entry(room).or_default().remove(slot);
            let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
            state.process_video(&frames);
        }
    }

    /// Current composed pixels, shared by subscribers when a render is throttled.
    pub fn composite_frame(&self, room: RoomId) -> Option<Arc<Frame>> {
        self.rooms.lock().unwrap().get(&room).map(|state| state.composite.clone())
    }

    /// Synchronously removes a bridged participant from the MCU room, purging layout and audio.
    pub fn unbridge_participant(&self, room: RoomId, pid: ParticipantId) {
        let mut g = self.rooms.lock().unwrap();
        if let Some(state) = g.get_mut(&room) {
            state.subscribers.remove(&pid);
            state.publisher_tracks.remove(&pid);
            state.join_order.retain(|known| *known != pid);
            state.vads.remove(&pid);
            state.speaking_states.remove(&pid);
            if state.active_speaker == Some(pid) {
                state.active_speaker = None;
            }
            state.participant_audio.remove(&pid);
            self.audio.lock().unwrap().entry(room).or_default().remove(&pid);

            let removed_slot = state.layout.as_mut().and_then(|desc| desc.assignments.remove(&pid));
            if let Some(slot_id) = removed_slot {
                self.frames.lock().unwrap().entry(room).or_default().remove(&slot_id);
            }

            let vpid = vks_layout::ParticipantId::new(pid.as_uuid().as_u128());
            for slot in &mut state.current_layout.slots {
                if slot.participant_id == Some(vpid) {
                    slot.participant_id = None;
                    self.frames.lock().unwrap().entry(room).or_default().remove(&slot.id.as_u32());
                }
            }

            if let Some(desc) = &state.layout {
                let new_layout = RoomState::layout_from_descriptor(desc);
                state.bridge.apply(&new_layout);
                state.current_layout = new_layout;
            }

            let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
            state.process_video(&frames);
        }
    }

    /// Returns whether the participant is registered in the MCU room.
    pub fn has_participant(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.rooms
            .lock()
            .unwrap()
            .get(&room)
            .map(|s| s.subscribers.contains(&pid) || s.publisher_tracks.contains_key(&pid))
            .unwrap_or(false)
    }
}

#[async_trait]
impl MediaEngine for McuEngine {
    fn mode(&self) -> RoomMode {
        RoomMode::Mcu
    }

    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        if g.contains_key(&cfg.room_id) {
            return Err(MediaEngineError::RoomAlreadyExists(cfg.room_id));
        }
        let state = RoomState::new(RoomMode::Mcu, cfg.caps);
        g.insert(cfg.room_id, state);
        Ok(RoomMediaHandle { room: cfg.room_id, mode: RoomMode::Mcu })
    }

    async fn destroy_room(&self, room: RoomId) -> Result<(), MediaEngineError> {
        let removed = {
            let mut g = self.rooms.lock().unwrap();
            g.remove(&room).is_some()
        };
        // Не держим rooms при взятии pending: единый порядок локов
        // (rooms → pending) во всех путях.
        self.pending_layouts.lock().unwrap().remove(&room);
        if !removed {
            return Err(MediaEngineError::RoomNotFound(room));
        }
        self.frames.lock().unwrap().remove(&room);
        self.audio.lock().unwrap().remove(&room);
        Ok(())
    }

    async fn switch_room_mode(&self, room: RoomId, _new_mode: RoomMode) -> Result<RoomMode, MediaEngineError> {
        // The MCU engine is mode-fixed: it always serves RoomMode::Mcu.
        // An in-place SFU↔MCU switch is coordinated by the core (ADR-0026);
        // here we report the mode this engine serves.
        let g = self.rooms.lock().unwrap();
        if g.contains_key(&room) {
            Ok(RoomMode::Mcu)
        } else {
            Err(MediaEngineError::RoomNotFound(room))
        }
    }

    async fn add_publisher(
        &self,
        room: RoomId,
        publisher: ParticipantId,
        tracks: Vec<MediaTrack>,
    ) -> Result<PublisherHandle, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.publisher_tracks.insert(publisher, tracks);
        if !state.join_order.contains(&publisher) {
            state.join_order.push(publisher);
        }
        Ok(PublisherHandle { room, publisher })
    }

    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.publisher_tracks.remove(&publisher);
        state.join_order.retain(|known| *known != publisher);
        state.vads.remove(&publisher);
        state.speaking_states.remove(&publisher);
        if state.active_speaker == Some(publisher) {
            state.active_speaker = None;
        }
        state.participant_audio.remove(&publisher);
        self.audio.lock().unwrap().entry(room).or_default().remove(&publisher);

        // M3.2: Eliminate ghost frames and clean up layout bindings
        let removed_slot = state.layout.as_mut().and_then(|desc| desc.assignments.remove(&publisher));
        if let Some(slot_id) = removed_slot {
            self.frames.lock().unwrap().entry(room).or_default().remove(&slot_id);
        }

        let vpid = vks_layout::ParticipantId::new(publisher.as_uuid().as_u128());
        for slot in &mut state.current_layout.slots {
            if slot.participant_id == Some(vpid) {
                slot.participant_id = None;
                self.frames.lock().unwrap().entry(room).or_default().remove(&slot.id.as_u32());
            }
        }

        if let Some(desc) = &state.layout {
            let new_layout = RoomState::layout_from_descriptor(desc);
            state.bridge.apply(&new_layout);
            state.current_layout = new_layout;
        }

        // Re-compose composite frame immediately so departed publisher's video is purged
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        state.process_video(&frames);
        Ok(())
    }

    async fn add_subscriber(
        &self,
        room: RoomId,
        subscriber: ParticipantId,
        _req: SubscribeRequest,
    ) -> Result<SubscriberHandle, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.subscribers.insert(subscriber);
        Ok(SubscriberHandle { room, subscriber })
    }

    async fn remove_subscriber(&self, room: RoomId, subscriber: ParticipantId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.subscribers.remove(&subscriber);
        state.participant_audio.remove(&subscriber);
        Ok(())
    }

    async fn apply_layout(&self, room: RoomId, mut layout: LayoutDescriptor) -> Result<(), MediaEngineError> {
        sanitize_assignments(&mut layout);
        let mut g = self.rooms.lock().unwrap();
        let Some(state) = g.get_mut(&room) else {
            drop(g);
            // SFU-сессия без шлюза не должна аллоцировать MCU-композит.
            self.pending_layouts.lock().unwrap().insert(room, layout);
            return Ok(());
        };
        // Комната уже есть: pending устарел, иначе он мог бы «оживить» старую
        // раскладку на следующем bridge. Порядок локов rooms → pending.
        self.pending_layouts.lock().unwrap().remove(&room);
        // Уже забридженные видео-паблишеры не должны потерять ячейку: раскладка
        // может прийти вообще без привязок («Очистить все видеоокна» → все окна
        // AUTO, auto-пресет, возврат к «Общей» после удаления ручной). Иначе
        // `set_participant_frame` перестал бы писать кадры и живой SIP-терминал
        // увидел бы замерший последний снимок вместо новой сетки.
        // Порядок посадки — порядок прихода (S12), а не сортировка по UUID:
        // иначе после смены раскладки участники менялись бы местами.
        let mut bridged: Vec<ParticipantId> = state
            .join_order
            .iter()
            .copied()
            .filter(|pid| state.publisher_tracks.get(pid).is_some_and(|tracks| layout_has_video(tracks)))
            .collect();
        for (pid, tracks) in &state.publisher_tracks {
            if layout_has_video(tracks) && !bridged.contains(pid) {
                bridged.push(*pid);
            }
        }
        let version_floor = state.layout.as_ref().map_or(0, |old| old.version.saturating_add(1));
        for pid in bridged {
            if !layout.assignments.contains_key(&pid) {
                seat_participant(&mut layout, pid);
            }
        }
        // Версия монотонна: даже auto/default без смены назначений должна
        // перестроить композитор. Repair больше не бампает отдельно, поэтому
        // двойного инкремента нет.
        layout.version = layout.version.max(version_floor);
        let old_assignments = state.layout.as_ref().map(|previous| previous.assignments.clone());
        let new_layout = state.adopt_layout(layout);
        if let Some(old_assignments) = old_assignments {
            self.rekey_frames(room, &old_assignments, &new_layout);
        }
        Ok(())
    }

    async fn set_room_capabilities(&self, room: RoomId, caps: RoomCaps) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.caps = caps;
        Ok(())
    }

    async fn list_publishers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.publisher_tracks.keys().copied().collect())
    }

    async fn list_subscribers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.subscribers.iter().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_core::track::{Track, TrackKind};

    fn room_id() -> RoomId {
        RoomId::from_uuid("50000000-0000-0000-0000-000000000001".parse().unwrap())
    }
    fn p(n: u64) -> ParticipantId {
        ParticipantId::from_uuid(format!("60000000-0000-0000-0000-{n:012}").parse().unwrap())
    }
    fn video_track(pid: ParticipantId) -> Track {
        Track::new(pid, TrackKind::Video)
    }
    fn cfg(rid: RoomId) -> RoomMediaCfg {
        RoomMediaCfg { room_id: rid, mode: RoomMode::Mcu, caps: RoomCaps::default() }
    }
    fn sub_req() -> SubscribeRequest {
        SubscribeRequest { track_ids: Vec::new(), audio_only: false }
    }

    #[tokio::test]
    async fn mode_is_mcu() {
        assert_eq!(McuEngine::new().mode(), RoomMode::Mcu);
    }

    #[tokio::test]
    async fn create_and_destroy_room() {
        let e = McuEngine::new();
        let rid = room_id();
        let handle = e.create_room(cfg(rid)).await.unwrap();
        assert_eq!(handle.mode, RoomMode::Mcu);
        e.destroy_room(rid).await.unwrap();
        assert!(matches!(e.destroy_room(rid).await.unwrap_err(), MediaEngineError::RoomNotFound(_)));
    }

    /// ADR-0029, фаза 0: кадр для зрителя — тот же `Arc`, что и общий
    /// композит (нулевые копии), байты не меняются.
    #[tokio::test]
    async fn frame_for_viewer_shares_the_composite_without_copies() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        for _ in 0..12 {
            e.tick(rid);
        }
        let composite = e.composite_frame(rid).expect("composite");
        let first = e.frame_for_viewer(rid, p(100)).expect("кадр для зрителя");
        let second = e.frame_for_viewer(rid, p(101)).expect("кадр для второго зрителя");
        assert!(std::sync::Arc::ptr_eq(&first, &second), "зрители делят один кадр");
        assert!(std::sync::Arc::ptr_eq(&first, &composite), "кадр зрителя — общий композит");
        assert_eq!(first.data, composite.data, "байты не изменились");
    }

    /// ADR-0029, фаза 2: самопросмотр выключен по умолчанию — поведение
    /// комнаты не меняется, зритель получает общий `Arc<Frame>`.
    #[tokio::test]
    async fn self_view_is_off_by_default() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        for _ in 0..12 {
            e.tick(rid);
        }
        let composite = e.composite_frame(rid).expect("composite");
        let personal = e.frame_for_viewer(rid, a).expect("кадр для зрителя");
        assert!(std::sync::Arc::ptr_eq(&personal, &composite), "без самопросмотра кадр общий");
    }

    /// ADR-0029, фаза 2 («Continuous Presence»): при включённом самопросмотре
    /// зритель видит свою камеру в ячейке низшего приоритета, а его ячейка в
    /// общей сетке освобождена (у другого зрителя она остаётся).
    #[tokio::test]
    async fn self_view_moves_the_viewer_to_the_lowest_priority_cell() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        use vks_core::ports::media_engine::LayoutSlot;

        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        e.set_frame(rid, 1, Frame::filled(320, 180, 200));
        // Две ячейки 960×540: слот 0 — крупный, слот 1 — низший приоритет.
        let geometry = (0..2)
            .map(|slot| LayoutSlot { slot, x: slot * 960, y: 0, width: 960, height: 540, z_order: slot })
            .collect();
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        assignments.insert(b, 1);
        let layout = LayoutDescriptor { layout_id: "custom".into(), version: 1, assignments, geometry };
        e.apply_layout(rid, layout).await.unwrap();
        for _ in 0..12 {
            e.tick(rid);
        }
        let rects = e.slot_rects(rid).expect("слоты комнаты");
        let own = *rects.get(&0).expect("слот 0");
        let lowest = *rects.get(&1).expect("слот 1");
        e.set_self_view(rid, true);

        let personal = e.frame_for_viewer(rid, a).expect("персональный кадр");
        let other = e.frame_for_viewer(rid, b).expect("персональный кадр второго");
        assert!(!std::sync::Arc::ptr_eq(&personal, &other), "у зрителей разные кадры");
        // Ячейка зрителя в общей сетке зачёркнута, его камера — в последней ячейке.
        let cw = CANVAS_SIZE.0 as usize;
        let pixel = |frame: &Frame, x: u32, y: u32| -> u8 {
            let offset = y as usize * cw + x as usize;
            frame.data.get(offset).copied().unwrap_or_default()
        };
        let (ox, oy) = (own.x + own.w / 4, own.y + own.h / 4);
        assert_eq!(pixel(&personal, ox, oy), 0, "grayscale canvas: освобождённая ячейка залита 0");
        let (lx, ly) = (lowest.x + lowest.w / 4, lowest.y + lowest.h / 4);
        assert_eq!(pixel(&personal, lx, ly), 10, "камера зрителя в ячейке низшего приоритета");
        // У второго зрителя его камера осталась в общей сетке.
        let (bx, by) = (own.x + own.w / 4, own.y + own.h / 4);
        assert_eq!(pixel(&other, bx, by), 10, "у другого зрителя камера на месте");
    }

    /// ADR-0029, фаза 0: egress-запрос кадра для зрителя продвигает конвейер
    /// и возвращает композит (как раньше `tick_video_at`).
    #[tokio::test]
    async fn tick_video_for_viewer_advances_the_pipeline() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        let frame = e.tick_video_for_viewer(rid, 100, p(100)).expect("кадр");
        assert_eq!((frame.width, frame.height), (CANVAS_SIZE.0, CANVAS_SIZE.1));
        // Второй запрос в том же кадровом окне дропается по backpressure,
        // но кадр зрителя всё равно доступен (последний композит).
        let again = e.tick_video_for_viewer(rid, 101, p(100)).expect("кадр зрителя");
        assert_eq!(again.data, frame.data);
    }

    /// S12: изменение состояния «говорит» выходит из движка наружу.
    #[tokio::test]
    async fn speaking_changes_leave_the_engine() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.set_audio(rid, a, vec![0.9f32; 960]);
        // VAD с дебаунсом 500 мс: состояние «говорит» фиксируется после
        // устойчивого сигнала, а не с первого кадра.
        for step in 0..30 {
            e.tick_audio_at(rid, 100 + step * 20);
        }
        let changes = e.take_speaking_changes(rid);
        assert_eq!(changes, vec![(a, true)], "громкий участник начал говорить");
        assert!(e.take_speaking_changes(rid).is_empty(), "список забирается один раз");
        assert!(e.is_speaking(rid, a));
    }

    /// Копит события комнаты: тест подставляет его вместо серверной шины.
    fn collect_events(e: &McuEngine) -> Arc<Mutex<Vec<(RoomId, RoomEvent)>>> {
        let collected = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&collected);
        e.set_event_sink(Arc::new(move |room, event| {
            sink.lock().unwrap().push((room, event));
        }));
        collected
    }

    /// Комната с применённой раскладкой (движок принимает её отдельным вызовом).
    async fn apply_focus_layout(e: &McuEngine, rid: RoomId, layout_id: &str, assignments: &[(ParticipantId, u32)]) {
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: layout_id.into(),
                version: 1,
                assignments: assignments.iter().copied().collect(),
                geometry: Vec::new(),
            },
        )
        .await
        .unwrap();
    }

    fn slot_of(e: &McuEngine, rid: RoomId, pid: ParticipantId) -> Option<u32> {
        let layout = e.current_layout(rid)?;
        let wanted = pid.as_uuid().as_u128();
        layout.slots.iter().find(|slot| slot.participant_id.map(|p| p.as_u128()) == Some(wanted)).map(|s| s.id.as_u32())
    }

    fn loud(e: &McuEngine, rid: RoomId, pid: ParticipantId, steps: u64) {
        e.set_audio(rid, pid, vec![0.9f32; 960]);
        for step in 0..steps {
            e.tick_audio_at(rid, 100 + step * 20);
        }
    }

    /// Codian §2.2: главное окно следует за активным спикером (S12, фаза 3).
    #[tokio::test]
    async fn voice_activation_moves_the_main_window_to_the_active_speaker() {
        let e = McuEngine::new();
        let rid = room_id();
        let a = p(1);
        let b = p(2);
        e.create_room(cfg(rid)).await.unwrap();
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        apply_focus_layout(&e, rid, "focus", &[(a, 0), (b, 1)]).await;
        let events = collect_events(&e);

        assert_eq!(slot_of(&e, rid, a), Some(0), "сначала главное окно у A");
        assert_eq!(slot_of(&e, rid, b), Some(1));

        // B говорит громко: главное окно (слот 0) переезжает к B, A получает
        // освободившийся слот B (никто не пропадает из раскладки).
        loud(&e, rid, b, 30);
        assert_eq!(slot_of(&e, rid, b), Some(0), "активный спикер — в главном окне");
        assert_eq!(slot_of(&e, rid, a), Some(1), "прежний жилец не пропал");
        let version = e.current_layout(rid).expect("раскладка").version;

        let collected = events.lock().unwrap().clone();
        assert!(
            collected.iter().any(|(room, event)| *room == rid
                && matches!(event, RoomEvent::Speaking { participant, speaking, .. } if *participant == b && *speaking)),
            "смена речи уходит наружу: {collected:?}"
        );
        assert!(
            collected.iter().any(|(room, event)| *room == rid
                && matches!(event, RoomEvent::LayoutChanged { version: v } if *v == version)),
            "пересадка главного окна уходит наружу: {collected:?}"
        );
        // Дескриптор отражает фактическое заселение: повторный apply не откатит.
        let descriptor = e.layout_descriptor(rid).expect("дескриптор");
        assert_eq!(descriptor.assignments.get(&b), Some(&0));
        assert_eq!(descriptor.assignments.get(&a), Some(&1));
    }

    /// Голосовая пересадка переносит и пиксели: кадр принадлежит участнику,
    /// а не номеру слота. Без переноса в главном окне остался бы прежний жилец.
    #[tokio::test]
    async fn voice_activation_moves_the_frames_with_the_participants() {
        let e = McuEngine::new();
        let rid = room_id();
        let a = p(1);
        let b = p(2);
        e.create_room(cfg(rid)).await.unwrap();
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        apply_focus_layout(&e, rid, "focus", &[(a, 0), (b, 1)]).await;
        let _ = collect_events(&e);

        // Слот 0 (крупный) — A (яркость 10), слот 1 — B (яркость 20).
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        e.set_frame(rid, 1, Frame::filled(320, 180, 20));
        for _ in 0..12 {
            e.tick(rid);
        }

        // B говорит громко: главное окно переезжает к B.
        loud(&e, rid, b, 30);
        assert_eq!(slot_of(&e, rid, b), Some(0), "главное окно — у спикера");

        let out = e.tick(rid).unwrap();
        let cw = CANVAS_SIZE.0 as usize;
        // Кадр 320×180 вписывается в слоты с letterbox (1440×810 и 480×270 по
        // центру), поэтому семплируем по центру окна, а не по его краю.
        let main = out.data[400 * cw + 960];
        let strip = out.data[900 * cw + 960];
        assert_eq!(main, 20, "в главном окне пиксели спикера B, а не прежнего жильца A");
        assert_eq!(strip, 10, "в полосе — пиксели A: кадры переехали вместе с участниками");
    }

    /// «Важный» участник важнее активного спикера (Codian §2.2).
    #[tokio::test]
    async fn important_participant_outranks_the_active_speaker() {
        let e = McuEngine::new();
        let rid = room_id();
        let a = p(1);
        let b = p(2);
        e.create_room(cfg(rid)).await.unwrap();
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        apply_focus_layout(&e, rid, "focus", &[(a, 0), (b, 1)]).await;

        // B говорит громче — главное окно у B.
        loud(&e, rid, b, 30);
        assert_eq!(slot_of(&e, rid, b), Some(0));

        // Назначаем «Важным» A: он вытесняет спикера из главного окна.
        assert!(e.set_important_participant(rid, Some(a)), "пересадка состоялась");
        assert_eq!(slot_of(&e, rid, a), Some(0), "«Важный» — в главном окне");
        assert_eq!(slot_of(&e, rid, b), Some(1));
        assert_eq!(e.important_participant(rid), Some(a));

        // Прежний спикер продолжает говорить — «Важный» всё равно главнее.
        loud(&e, rid, b, 10);
        assert_eq!(slot_of(&e, rid, a), Some(0), "спикер не вытесняет «Важного»");

        // Снятие статуса возвращает голосовую активацию.
        assert!(e.set_important_participant(rid, None));
        assert_eq!(slot_of(&e, rid, b), Some(0), "голосовая активация вернулась");
        assert!(e.important_participant(rid).is_none());
        assert!(!e.set_important_participant(RoomId::from_uuid(uuid::Uuid::from_u128(4242)), Some(a)));
    }

    /// Равная сетка и фиксированный макет PNM не имеют главного окна:
    /// голосовая активация их не трогает (Codian §2.1: 52 макета статичны).
    #[tokio::test]
    async fn voice_activation_leaves_grid_and_custom_layouts_alone() {
        let e = McuEngine::new();
        let rid = room_id();
        let a = p(1);
        let b = p(2);
        e.create_room(cfg(rid)).await.unwrap();
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        let before = e.current_layout(rid).expect("раскладка").clone();

        loud(&e, rid, b, 30);
        let after = e.current_layout(rid).expect("раскладка");
        assert_eq!(after.kind, LayoutKind::Grid);
        assert_eq!(after.slots.len(), before.slots.len());
        assert_eq!(after.version, before.version, "сетка не перестраивается голосом");
    }

    /// Голосовая активация без подписки ничего не ломает: приёмника нет.
    #[tokio::test]
    async fn voice_activation_works_without_an_event_sink() {
        let e = McuEngine::new();
        let rid = room_id();
        let a = p(1);
        e.create_room(cfg(rid)).await.unwrap();
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        apply_focus_layout(&e, rid, "speaker", &[(a, 0)]).await;
        loud(&e, rid, a, 30);
        assert_eq!(e.current_layout(rid).expect("раскладка").kind, LayoutKind::Speaker);
    }

    /// S12: авто-сетка и пересадка участников идут по порядку прихода, а не
    /// по сортировке UUID.
    #[tokio::test]
    async fn auto_layout_follows_the_join_order_not_the_uuid_sort() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        // `p(9)` приходит раньше `p(1)`, но сортировка по UUID дала бы p(1) первым.
        let first = p(9);
        let second = p(1);
        e.add_publisher(rid, first, vec![video_track(first)]).await.unwrap();
        e.add_publisher(rid, second, vec![video_track(second)]).await.unwrap();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "auto".into(),
                version: 1,
                assignments: std::collections::BTreeMap::new(),
                geometry: Vec::new(),
            },
        )
        .await
        .unwrap();

        let layout = e.current_layout(rid).expect("раскладка");
        let vpid = |pid: ParticipantId| Some(vks_layout::ParticipantId::new(pid.as_uuid().as_u128()));
        assert_eq!(layout.slots[0].participant_id, vpid(first), "слот 0 — первый пришедший");
        assert_eq!(layout.slots[1].participant_id, vpid(second), "слот 1 — второй пришедший");
    }

    #[tokio::test]
    async fn full_pipeline_composite() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        // 2×2 grid layout: slot 0 → a, slot 1 → b.
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        assignments.insert(b, 1);
        let layout = LayoutDescriptor { layout_id: "L".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();
        e.add_subscriber(rid, p(100), sub_req()).await.unwrap();

        // Drive the pipeline with mock frames.
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        e.set_frame(rid, 1, Frame::filled(320, 180, 20));
        e.set_audio(rid, a, vec![0.5f32; 960]);
        e.set_audio(rid, b, vec![0.5f32; 960]);

        // Let the crossfade finish (300 ms ≈ 10 ticks).
        for _ in 0..12 {
            e.tick(rid);
        }
        let out = e.tick(rid).unwrap();
        assert_eq!((out.width, out.height), (CANVAS_SIZE.0, CANVAS_SIZE.1));
        // Slot 0 occupies col 0..960 (full 1080 tall); the 320×180 frame is
        // letterboxed to 960×540, centered → image rows 270..810. Sample the
        // image center (row 400): slot 0 = frame a (10), slot 1 = frame b (20).
        let row = 400usize;
        assert_eq!(out.data[row * CANVAS_SIZE.0 as usize + 100], 10, "slot 0 = frame a");
        assert_eq!(out.data[row * CANVAS_SIZE.0 as usize + 960 + 100], 20, "slot 1 = frame b");
    }

    #[tokio::test]
    async fn list_publishers_and_subscribers() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_subscriber(rid, p(10), sub_req()).await.unwrap();
        let pubs = e.list_publishers(rid).await.unwrap();
        assert!(pubs.contains(&a));
        let subs = e.list_subscribers(rid).await.unwrap();
        assert!(subs.contains(&p(10)));
    }

    #[tokio::test]
    async fn unknown_room_errors() {
        let e = McuEngine::new();
        let missing = RoomId::from_uuid("70000000-0000-0000-0000-000000000001".parse().unwrap());
        assert!(matches!(e.destroy_room(missing).await.unwrap_err(), MediaEngineError::RoomNotFound(_)));
        assert!(matches!(e.list_publishers(missing).await.unwrap_err(), MediaEngineError::RoomNotFound(_)));
        // T03: раскладка до создания комнаты — pending, а не ошибка: сессия
        // стартует раньше первого шлюзового вызова.
        e.apply_layout(missing, LayoutDescriptor::default()).await.expect("pending layout is accepted");
        assert!(e.layout_descriptor(missing).is_some());
        assert!(matches!(e.destroy_room(missing).await.unwrap_err(), MediaEngineError::RoomNotFound(_)));
        assert!(e.layout_descriptor(missing).is_none(), "destroy_room чистит pending");
    }

    #[tokio::test]
    async fn switch_mode_reports_mcu() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        assert_eq!(e.switch_room_mode(rid, RoomMode::Mcu).await.unwrap(), RoomMode::Mcu);
    }

    #[tokio::test]
    async fn set_capabilities() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let caps = RoomCaps { video_codecs: vec!["h264".to_string()], ..Default::default() };
        e.set_room_capabilities(rid, caps).await.unwrap();
        // No error; state updated internally.
    }

    #[tokio::test]
    async fn mix_minus_and_speaker_detection() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();

        // A speaks with audio (square wave crossing zero), B is silent
        let speech: Vec<f32> = (0..960).map(|i| if i % 16 < 8 { 0.7f32 } else { -0.7f32 }).collect();
        e.set_audio(rid, a, speech);
        e.set_audio(rid, b, vec![0.0f32; 960]);

        // Tick 30 times (30 * 20ms = 600ms > 500ms debounce)
        for _ in 0..30 {
            e.tick(rid);
        }

        // Mix-minus check:
        // A hears B (which is 0.0, no self echo!)
        let a_audio = e.participant_audio(rid, a).expect("A audio");
        assert!((a_audio[0] - 0.0).abs() < 1e-4, "A must not hear their own audio");

        // B hears A (0.7)
        let b_audio = e.participant_audio(rid, b).expect("B audio");
        assert!((b_audio[0] - 0.7).abs() < 1e-4, "B must hear A's audio");

        // VAD active speaker check
        assert_eq!(e.active_speaker(rid), Some(a), "A should be the active speaker");

        // Cleanup check on publisher remove
        e.remove_publisher(rid, a).await.unwrap();
        assert!(e.participant_audio(rid, a).is_none(), "Audio cleaned up on remove");
        assert!(!e.is_speaking(rid, a), "VAD state cleaned up on remove");
    }

    #[tokio::test]
    async fn custom_slot_and_focus_layout() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let presenter = p(10);
        let audience = p(20);
        e.add_publisher(rid, presenter, vec![video_track(presenter)]).await.unwrap();
        e.add_publisher(rid, audience, vec![video_track(audience)]).await.unwrap();

        let mut assignments = std::collections::BTreeMap::new();
        // Custom slot assignment: presenter to slot 0, audience to slot 1
        assignments.insert(presenter, 0);
        assignments.insert(audience, 1);
        let layout = LayoutDescriptor { layout_id: "focus".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();

        let rects = e.slot_rects(rid).expect("slot rects");
        assert_eq!(rects.len(), 2);
        // Slot 0 (focus) takes 75% height = 810 px
        let r0 = rects.get(&0).expect("slot 0");
        assert_eq!(r0.h, 810);
        assert_eq!(r0.y, 0);
        // Slot 1 (strip) takes 25% height = 270 px
        let r1 = rects.get(&1).expect("slot 1");
        assert_eq!(r1.h, 270);
        assert_eq!(r1.y, 810);
    }

    #[tokio::test]
    async fn listener_subscriber_receives_mixed_audio() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let speaker = p(1);
        let listener = p(2);
        e.add_publisher(rid, speaker, vec![video_track(speaker)]).await.unwrap();
        e.add_subscriber(rid, listener, sub_req()).await.unwrap();

        // Speaker produces audio
        e.set_audio(rid, speaker, vec![0.5f32; 960]);
        e.tick(rid);

        // Speaker gets mix-minus (silence for single speaker)
        let speaker_audio = e.participant_audio(rid, speaker).expect("speaker audio");
        assert_eq!(speaker_audio, vec![0.0f32; 960]);

        // Listener (non-publisher) gets the full mixed room audio (speaker's voice)
        let listener_audio = e.participant_audio(rid, listener).expect("listener audio");
        assert_eq!(listener_audio.len(), 960);
        assert!((listener_audio[0] - 0.5f32).abs() < 1e-4);
    }

    #[tokio::test]
    async fn independent_audio_and_video_cadence() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let speaker = p(1);
        let listener = p(2);
        e.add_publisher(rid, speaker, vec![video_track(speaker)]).await.unwrap();
        e.add_subscriber(rid, listener, sub_req()).await.unwrap();

        // 1. Audio tick at 0 ms: accepted
        e.set_audio(rid, speaker, vec![0.7f32; 960]);
        assert!(e.tick_audio_at(rid, 0));
        assert!(e.participant_audio(rid, listener).is_some());

        // 2. Audio tick at 10 ms: throttled (within 20ms cadence)
        assert!(!e.tick_audio_at(rid, 10));

        // 3. Audio tick at 20 ms: accepted
        assert!(e.tick_audio_at(rid, 20));

        // 4. Video tick at 0 ms: accepted
        assert!(e.tick_video_at(rid, 0).is_some());

        // 5. Video tick at 20 ms: dropped by video backpressure (requires ~30 ms)
        assert!(e.tick_video_at(rid, 20).is_none());

        // 6. Video tick at 33 ms: accepted
        assert!(e.tick_video_at(rid, 33).is_some());

        // 7. tick_at at 40ms: audio runs at 40ms (20ms since last 20ms audio tick) even though video drops (40 < 33+30)
        let stats_before = e.stats(rid).unwrap().frames_dropped;
        let rendered = e.tick_at(rid, 40);
        assert!(rendered.is_none(), "video frame dropped due to 30fps budget");
        let stats_after = e.stats(rid).unwrap().frames_dropped;
        assert_eq!(stats_after, stats_before + 1);
        // But listener still has valid fresh audio!
        assert!(e.participant_audio(rid, listener).is_some());
    }

    #[tokio::test]
    async fn ghost_frame_cleared_on_publisher_remove() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();

        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        assignments.insert(b, 1);
        let layout = LayoutDescriptor { layout_id: "grid".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();

        // Feed frames for slot 0 (a) and slot 1 (b)
        e.set_frame(rid, 0, Frame::filled(320, 180, 10));
        e.set_frame(rid, 1, Frame::filled(320, 180, 20));

        // Advance layout crossfade
        for _ in 0..12 {
            e.tick(rid);
        }

        let out = e.tick(rid).unwrap();
        let row = 400usize;
        assert_eq!(out.data[row * CANVAS_SIZE.0 as usize + 100], 10, "slot 0 has frame A (10)");
        assert_eq!(out.data[row * CANVAS_SIZE.0 as usize + 960 + 100], 20, "slot 1 has frame B (20)");

        // Now publisher A leaves the room
        e.remove_publisher(rid, a).await.unwrap();

        // The composite frame must IMMEDIATELY have publisher A's frame purged (no ghost frame!)
        let out_after_remove = e.tick(rid).unwrap();
        assert_ne!(out_after_remove.data[row * CANVAS_SIZE.0 as usize + 100], 10, "slot 0 ghost frame must be gone!");
        assert_eq!(out_after_remove.data[row * CANVAS_SIZE.0 as usize + 100], 0, "slot 0 is cleared to background");

        // Publisher B's frame is still present
        assert_eq!(
            out_after_remove.data[row * CANVAS_SIZE.0 as usize + 960 + 100],
            20,
            "slot 1 frame B remains active"
        );
    }

    #[tokio::test]
    async fn arbitrary_slot_ids_7_and_42_sparse_layout_no_holes() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();

        // Custom arbitrary slot IDs: 7 and 42
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 7);
        assignments.insert(b, 42);
        let layout = LayoutDescriptor { layout_id: "grid".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();

        // Under M0.4 / M3.2, sparse slot IDs must create exactly 2 slots without 40+ holes
        let rects = e.slot_rects(rid).expect("slot rects");
        assert_eq!(rects.len(), 2, "must have exactly 2 slots for 2 participants, no holes");
        assert!(rects.contains_key(&7), "slot 7 exists");
        assert!(rects.contains_key(&42), "slot 42 exists");

        // Both slots occupy full half-screen proportions (e.g. 960 width each)
        let r7 = rects.get(&7).unwrap();
        let r42 = rects.get(&42).unwrap();
        assert_eq!(r7.w, 960);
        assert_eq!(r7.h, 1080);
        assert_eq!(r42.w, 960);
        assert_eq!(r42.h, 1080);
    }

    /// Кадр принадлежит участнику, а не номеру слота: после перестановки
    /// участников в раскладке в окне должно быть видео нового владельца.
    #[tokio::test]
    async fn frames_follow_participant_after_slot_swap() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(11);
        let b = p(12);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();

        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        assignments.insert(b, 1);
        e.apply_layout(
            rid,
            LayoutDescriptor { layout_id: "grid".into(), version: 1, assignments, geometry: Vec::new() },
        )
        .await
        .unwrap();
        assert!(e.set_participant_frame(rid, a, Frame::filled(320, 180, 11)));
        assert!(e.set_participant_frame(rid, b, Frame::filled(320, 180, 22)));
        e.tick_at(rid, 100).unwrap();

        // Меняем участников местами: теперь слот 0 принадлежит B, слот 1 — A.
        let mut swapped = std::collections::BTreeMap::new();
        swapped.insert(a, 1);
        swapped.insert(b, 0);
        e.apply_layout(
            rid,
            LayoutDescriptor { layout_id: "grid".into(), version: 2, assignments: swapped, geometry: Vec::new() },
        )
        .await
        .unwrap();

        let rects = e.slot_rects(rid).expect("slot rects");
        let out = e.tick_at(rid, 200).expect("composite");
        // Слот 0 (левый) теперь B — там должно быть его значение 22, а не 11.
        let left = rects.get(&0).expect("slot 0");
        // Кадр вписывается в слот с сохранением пропорций (letterbox), поэтому
        // берём центр слота, а не его левый верхний угол.
        let cx = left.x + left.w / 2;
        let cy = left.y + left.h / 2;
        let value = out.data[cy as usize * CANVAS_SIZE.0 as usize + cx as usize];
        assert_eq!(value, 22, "после перестановки в слоте 0 должно быть видео участника B");
    }

    /// P0: выбранная раскладка должна доводить абонента до ЕГО ячейки.
    /// `apply_layout` (pid→slot 3) + `bridge_participant` не должны
    /// переназначать слот в «первый свободный» (slot 0).
    #[tokio::test]
    async fn chosen_layout_places_publisher_into_its_own_slot() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(3);
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 3);
        let layout = LayoutDescriptor { layout_id: "grid".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();

        e.bridge_participant(rid, a, vec![video_track(a)]);

        let rects = e.slot_rects(rid).expect("slot rects");
        assert_eq!(rects.len(), 1, "chosen layout must not add an auto-assigned slot 0");
        assert!(rects.contains_key(&3), "publisher keeps the requested slot 3");

        // Colour (77 in the Y plane) must appear exactly inside slot 3's rect.
        let frame = Frame::new(320, 180, vec![77u8; 320 * 180 * 3 / 2]);
        assert!(e.set_participant_frame(rid, a, frame), "assigned publisher must be able to write a frame");
        let out = e.tick(rid).expect("composite frame");
        let rect = rects.get(&3).unwrap();
        let x = (rect.x + rect.w / 2) as usize;
        let y = (rect.y + rect.h / 2) as usize;
        assert_eq!(out.data[y * CANVAS_SIZE.0 as usize + x], 77, "slot 3 must show the publisher's colour");
    }

    /// Поздний участник при готовой раскладке занимает минимальный свободный
    /// u32-слот, а «сидящий» сохраняет свой.
    #[tokio::test]
    async fn late_joiner_takes_minimal_free_slot_of_chosen_layout() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let seated = p(1);
        let late = p(2);
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(seated, 3);
        let layout = LayoutDescriptor { layout_id: "grid".into(), version: 1, assignments, geometry: Vec::new() };
        e.apply_layout(rid, layout).await.unwrap();

        e.bridge_participant(rid, late, vec![video_track(late)]);

        // Assignment появляется сразу (slot 0 свободен), а геометрия нового
        // слота — после 300-мс crossfade: `slot_rects` внутри окна отдаёт
        // rects предыдущей раскладки.
        assert!(
            e.set_participant_frame(rid, late, Frame::filled(320, 180, 5)),
            "late joiner must own a slot as soon as it is bridged"
        );
        for _ in 0..11 {
            e.tick(rid);
        }

        let rects = e.slot_rects(rid).expect("slot rects");
        assert!(rects.contains_key(&3), "seated participant keeps slot 3");
        assert!(rects.contains_key(&0), "late joiner takes the minimal free slot 0");
    }

    /// Точная геометрия PNM (`calculate`/`preset_selected_b_4_(2)`) должна
    /// перекрывать авто-сетку: композитор рисует кадр ровно в присланном rect.
    #[tokio::test]
    async fn exact_geometry_overrides_auto_grid() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(7);
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        let layout = LayoutDescriptor {
            layout_id: "preset_selected_rb_4".into(),
            version: 1,
            assignments,
            geometry: vec![LayoutSlot { slot: 0, x: 960, y: 540, width: 960, height: 540, z_order: 0 }],
        };
        e.apply_layout(rid, layout).await.unwrap();
        e.bridge_participant(rid, a, vec![video_track(a)]);
        for _ in 0..11 {
            e.tick(rid);
        }

        let rects = e.slot_rects(rid).expect("slot rects");
        let cell = rects.get(&0).expect("slot 0");
        assert_eq!((cell.x, cell.y, cell.w, cell.h), (960, 540, 960, 540), "exact rect, not 1x1 sqrt grid");

        assert!(e.set_participant_frame(rid, a, Frame::filled(320, 180, 9)));
        let out = e.tick(rid).expect("composite");
        let index = ((cell.y + cell.h / 2) * CANVAS_SIZE.0 + (cell.x + cell.w / 2)) as usize;
        assert_eq!(out.data[index], 9, "publisher colour is rendered inside its exact cell");
    }

    /// «Очистить все видеоокна» / auto-раскладка приходят вообще без привязок.
    /// Уже забридженный паблишер обязан сохранить ячейку, иначе
    /// `set_participant_frame` начнёт возвращать false и живой SIP-терминал
    /// увидит замерший последний снимок вместо новой сетки.
    #[tokio::test]
    async fn layout_without_assignments_keeps_bridged_publisher_visible() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(5);
        e.bridge_participant(rid, a, vec![video_track(a)]);
        assert!(e.set_participant_frame(rid, a, Frame::filled(320, 180, 55)), "bridged publisher starts visible");

        // 2×2 без привязок: все окна свободны (после «Очистить все видеоокна»).
        let geometry = (0..4)
            .map(|slot| LayoutSlot {
                slot,
                x: (slot % 2) * 960,
                y: (slot / 2) * 540,
                width: 960,
                height: 540,
                z_order: slot,
            })
            .collect();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_equal_4".into(),
                version: 2,
                assignments: std::collections::BTreeMap::new(),
                geometry,
            },
        )
        .await
        .unwrap();

        assert!(
            e.set_participant_frame(rid, a, Frame::filled(320, 180, 99)),
            "bridged publisher must keep a slot after an AUTO-only layout"
        );
        for _ in 0..11 {
            e.tick(rid);
        }
        let rects = e.slot_rects(rid).expect("slot rects");
        assert_eq!(rects.len(), 4, "геометрия 2×2 сохранена, а не подменена авто-сеткой");
        let descriptor = e.layout_descriptor(rid).expect("descriptor");
        assert!(descriptor.version >= 2, "auto/repair монотонно увеличивает версию");
        assert_eq!(descriptor.assignments.len(), 1, "забридженный паблишер назначен в geometry");
        let composite = e.tick(rid).expect("composite frame");
        assert_eq!(composite.data[270 * CANVAS_SIZE.0 as usize + 480], 99, "новый кадр виден в верхней левой ячейке");
    }

    /// T03: pending-раскладка не создаёт комнату, но применяется на первом
    /// bridge; лишний паблишер не получает full-canvas вне geometry.
    #[tokio::test]
    async fn pending_layout_applies_on_first_bridge_without_extra_full_canvas() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        let geometry = (0..4)
            .map(|slot| LayoutSlot {
                slot,
                x: (slot % 2) * 960,
                y: (slot / 2) * 540,
                width: 960,
                height: 540,
                z_order: slot,
            })
            .collect();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_equal_4".into(),
                version: 1,
                assignments: Default::default(),
                geometry,
            },
        )
        .await
        .unwrap();
        assert!(e.slot_rects(rid).is_none(), "pending не аллоцирует комнату");
        assert!(e.layout_descriptor(rid).is_some(), "pending сохранён");

        let pids: Vec<_> = (1..=5).map(p).collect();
        for pid in &pids {
            e.bridge_participant(rid, *pid, vec![video_track(*pid)]);
        }
        for _ in 0..11 {
            e.tick(rid);
        }
        let rects = e.slot_rects(rid).expect("room created on first bridge");
        assert_eq!(rects.len(), 4, "ровно 4 geometry-ячейки, пятый не получает full-canvas");
        let layout = e.layout_descriptor(rid).expect("layout");
        assert_eq!(layout.assignments.len(), 4, "пятый паблишер остаётся без слота");
        assert!(layout.assignments.values().all(|slot| *slot < 4));
    }

    /// T03: stale pending не должен перезаписывать уже применённую раскладку
    /// через публичный `ensure_room`.
    #[tokio::test]
    async fn ensure_room_does_not_overwrite_existing_layout_with_stale_pending() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "current".into(),
                version: 1,
                assignments: BTreeMap::new(),
                geometry: Vec::new(),
            },
        )
        .await
        .unwrap();
        e.pending_layouts.lock().unwrap().insert(
            rid,
            LayoutDescriptor {
                layout_id: "stale".into(),
                version: 99,
                assignments: BTreeMap::new(),
                geometry: Vec::new(),
            },
        );
        e.ensure_room(rid);
        assert_eq!(e.layout_descriptor(rid).expect("layout").layout_id, "current");
        assert!(e.pending_layouts.lock().unwrap().is_empty(), "stale pending удалён");
    }

    /// T03: audio-only первый bridge не должен съедать pending-раскладку.
    #[tokio::test]
    async fn pending_layout_survives_audio_only_first_bridge() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        let geometry = (0..4)
            .map(|slot| LayoutSlot {
                slot,
                x: (slot % 2) * 960,
                y: (slot / 2) * 540,
                width: 960,
                height: 540,
                z_order: 0,
            })
            .collect();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_selected_b_4".into(),
                version: 1,
                assignments: Default::default(),
                geometry,
            },
        )
        .await
        .unwrap();

        let audio_only = p(21);
        e.bridge_participant(rid, audio_only, vec![Track::new(audio_only, vks_core::track::TrackKind::Audio)]);
        let layout = e.layout_descriptor(rid).expect("pending layout must survive audio-only bridge");
        assert_eq!(layout.layout_id, "preset_selected_b_4", "выбранная раскладка сохранена");
        assert_eq!(layout.geometry.len(), 4, "geometry не подменена пустой grid");

        let video = p(22);
        e.bridge_participant(rid, video, vec![video_track(video)]);
        let layout = e.layout_descriptor(rid).expect("layout");
        assert_eq!(layout.layout_id, "preset_selected_b_4");
        assert!(layout.assignments.contains_key(&video), "видео-участник сел в geometry-слот");
    }

    /// T03: при готовой geometry новый участник занимает только свободный
    /// geometry-слот, а не произвольный u32.
    #[tokio::test]
    async fn late_joiner_uses_only_free_geometry_slot() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let first = p(10);
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(first, 0);
        let geometry = vec![
            LayoutSlot { slot: 0, x: 0, y: 0, width: 960, height: 540, z_order: 0 },
            LayoutSlot { slot: 1, x: 960, y: 0, width: 960, height: 540, z_order: 1 },
        ];
        e.apply_layout(rid, LayoutDescriptor { layout_id: "preset_equal_2".into(), version: 1, assignments, geometry })
            .await
            .unwrap();
        let second = p(11);
        e.bridge_participant(rid, second, vec![video_track(second)]);
        let layout = e.layout_descriptor(rid).expect("layout");
        assert_eq!(layout.assignments[&second], 1, "свободный geometry-слот 1");
    }
}

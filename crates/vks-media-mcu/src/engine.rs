//! MCU engine — impl [`MediaEngine`] (MCU).
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
    LayoutDescriptor, LayoutSlot, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
use vks_core::{ParticipantId, RoomId, RoomMode};
use vks_layout::GridLayout;
use vks_layout::{Canvas, Layout, LayoutKind};

use crate::audio_mixer::AudioMixer;
use crate::compositor::{Compositor, CANVAS_SIZE};
use crate::farm::{EncoderFarm, EncoderSpec, FarmError, GroupStat, GroupViewer};
use crate::frame::Frame;
use crate::layout_bridge::{LayoutBridge, PixelRect, SlotRects};
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
    caps: RoomCaps,
    /// Publisher → their tracks (kept for listing / future decode).
    publisher_tracks: BTreeMap<ParticipantId, Vec<MediaTrack>>,
    /// Порядок прихода паблишеров (S12): сетка и заполнение слотов идут по
    /// порядку прихода, а не по сортировке UUID.
    join_order: Vec<ParticipantId>,
    /// Недавность речи (Codian §2.2): участники в порядке убывания недавности,
    /// свежий спикер — в голове. Мелкие ячейки заполняются по этому порядку.
    recent_speakers: Vec<ParticipantId>,
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
    /// Размер холста комнаты (`O1`): по умолчанию 1920×1080, но
    /// `ensure_composite_size` ставит его равным выходному профилю первой
    /// сессии, чтобы egress-`resize_i420` стал identity-`memcpy`.
    canvas: (u32, u32),
    /// Last composed frame, per subscriber (mock: the same composite for
    /// every subscriber). Размер всегда равен [`RoomState::canvas`].
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
    /// Participant display name labels for compositor placeholder badges.
    participant_labels: BTreeMap<ParticipantId, String>,
    /// Participant label visibility (default true).
    participant_label_visibility: BTreeMap<ParticipantId, bool>,
    /// Pre-allocated scratch frame buffer for zero-alloc compositing.
    scratch_frame: Option<Frame>,
}

impl RoomState {
    fn new(caps: RoomCaps) -> Self {
        Self {
            caps,
            publisher_tracks: BTreeMap::new(),
            join_order: Vec::new(),
            recent_speakers: Vec::new(),
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
            canvas: CANVAS_SIZE,
            composite: Arc::new({
                let pixels = CANVAS_SIZE.0 as usize * CANVAS_SIZE.1 as usize;
                let mut data = vec![128u8; pixels * 3 / 2];
                Compositor::fill_studio_black(&mut data, pixels);
                Frame::new(CANVAS_SIZE.0, CANVAS_SIZE.1, data)
            }),
            scratch_frame: None,
            mixed_audio: Vec::new(),
            participant_audio: BTreeMap::new(),
            vads: BTreeMap::new(),
            speaking_states: BTreeMap::new(),
            speaking_changes: Vec::new(),
            active_speaker: None,
            important: None,
            participant_labels: BTreeMap::new(),
            participant_label_visibility: BTreeMap::new(),
        }
    }

    /// The current (crossfaded) slot rects for the current layout.
    fn slot_rects(&self) -> SlotRects {
        self.bridge.current(self.elapsed_ms)
    }

    /// Slot id -> participant label mapping for compositor name badges.
    fn slot_labels(&self) -> BTreeMap<u32, String> {
        let mut labels = BTreeMap::new();
        if let Some(ref layout) = self.layout {
            for (pid, slot) in &layout.assignments {
                if self.participant_label_visibility.get(pid).copied().unwrap_or(true) {
                    if let Some(label) = self.participant_labels.get(pid) {
                        labels.insert(*slot, label.clone());
                    }
                }
            }
        }
        labels
    }

    /// Видео-участники в порядке прихода (S12) — тот же порядок, что в
    /// `apply_layout`, поэтому сетка стабильна между расширениями.
    fn video_participants_in_join_order(&self) -> Vec<ParticipantId> {
        let mut bridged: Vec<ParticipantId> = self
            .join_order
            .iter()
            .copied()
            .filter(|pid| self.publisher_tracks.get(pid).is_some_and(|tracks| layout_has_video(tracks)))
            .collect();
        for (pid, tracks) in &self.publisher_tracks {
            if layout_has_video(tracks) && !bridged.contains(pid) {
                bridged.push(*pid);
            }
        }
        bridged
    }

    /// Сажает видео-участника в слот; при нехватке окон авто-расширяет
    /// раскладку равной сеткой (функциональный блокер 20+).
    ///
    /// `true` — раскладку (geometry/назначения/версию) нужно применить заново.
    fn seat_video_or_expand(&mut self, pid: ParticipantId) -> bool {
        let descriptor = self.layout.get_or_insert_with(|| LayoutDescriptor {
            layout_id: "grid".into(),
            version: 0,
            assignments: BTreeMap::new(),
            geometry: Vec::new(),
            mode: "auto".into(),
            ..Default::default()
        });
        if descriptor.assignments.contains_key(&pid) {
            return false;
        }
        if seat_participant(descriptor, pid) {
            descriptor.version = descriptor.version.saturating_add(1);
            return true;
        }
        let mut bridged = self.video_participants_in_join_order();
        if !bridged.contains(&pid) {
            bridged.push(pid);
        }
        let descriptor = self.layout.as_mut().expect("layout created above");
        regrid_for_video(descriptor, &bridged)
    }

    /// Rects раскладки (пространство 1920×1080) → пространство холста комнаты.
    ///
    /// `O1`: масштабируем обе границы «half-up» и берём разность, чтобы тайлы
    /// стыковались без щелей и не выходили за холст (независимое округление
    /// `x` и `w` давало бы дырки в 1 px). Для дефолтного холста — без работы.
    fn scaled_slot_rects(&self) -> SlotRects {
        let rects = self.slot_rects();
        let (cw, ch) = self.canvas;
        let is_even_canvas = cw.is_multiple_of(2) && ch.is_multiple_of(2);
        if (cw, ch) == CANVAS_SIZE {
            if !is_even_canvas {
                return rects;
            }
            return rects
                .into_iter()
                .map(|(key, rect)| {
                    let x0 = (rect.x & !1).min(cw.saturating_sub(2));
                    let y0 = (rect.y & !1).min(ch.saturating_sub(2));
                    let x1 = ((rect.x.saturating_add(rect.w) + 1) & !1).min(cw);
                    let y1 = ((rect.y.saturating_add(rect.h) + 1) & !1).min(ch);
                    let w = x1.saturating_sub(x0).max(2);
                    let h = y1.saturating_sub(y0).max(2);
                    (key, PixelRect::new(x0, y0, w, h).with_z(rect.z))
                })
                .collect();
        }
        let scale = |value: u32, source: u32, target: u32| -> u32 {
            ((u64::from(value) * u64::from(target) + u64::from(source) / 2) / u64::from(source)) as u32
        };
        rects
            .into_iter()
            .map(|(key, rect)| {
                let raw_x0 = scale(rect.x, CANVAS_SIZE.0, cw);
                let raw_y0 = scale(rect.y, CANVAS_SIZE.1, ch);
                let raw_x1 = scale(rect.x.saturating_add(rect.w), CANVAS_SIZE.0, cw).min(cw);
                let raw_y1 = scale(rect.y.saturating_add(rect.h), CANVAS_SIZE.1, ch).min(ch);
                if is_even_canvas {
                    let x0 = (raw_x0 & !1).min(cw.saturating_sub(2));
                    let y0 = (raw_y0 & !1).min(ch.saturating_sub(2));
                    let x1 = ((raw_x1 + 1) & !1).min(cw);
                    let y1 = ((raw_y1 + 1) & !1).min(ch);
                    let w = x1.saturating_sub(x0).clamp(2, cw.saturating_sub(x0));
                    let h = y1.saturating_sub(y0).clamp(2, ch.saturating_sub(y0));
                    (key, PixelRect::new(x0, y0, w, h).with_z(rect.z))
                } else {
                    let w = raw_x1.saturating_sub(raw_x0).clamp(1, cw);
                    let h = raw_y1.saturating_sub(raw_y0).clamp(1, ch);
                    (key, PixelRect::new(raw_x0.min(cw.saturating_sub(1)), raw_y0.min(ch.saturating_sub(1)), w, h).with_z(rect.z))
                }
            })
            .collect()
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

            // Детектируем базис координат: если вся геометрия вписывается в 1280x720
            // и при этом достигает краев 720p (максимальный x+w > 960 или y+h > 540),
            // значит геометрия пришла в базисе 720p (Codian / Admin UI calculate).
            let max_w = cells.iter().map(|c| c.x.saturating_add(c.width)).max().unwrap_or(0);
            let max_h = cells.iter().map(|c| c.y.saturating_add(c.height)).max().unwrap_or(0);
            let is_720p_basis = max_w <= 1280 && max_h <= 720 && (max_w > 960 || max_h > 540);

            let scale_coord = |val: u32, src: u32, dst: u32| -> u32 {
                ((u64::from(val) * u64::from(dst) + u64::from(src) / 2) / u64::from(src)) as u32
            };

            for cell in &cells {
                let (cx, cy, cw, ch) = if is_720p_basis {
                    let x0 = scale_coord(cell.x, 1280, CANVAS_SIZE.0);
                    let y0 = scale_coord(cell.y, 720, CANVAS_SIZE.1);
                    let x1 = scale_coord(cell.x.saturating_add(cell.width), 1280, CANVAS_SIZE.0).min(CANVAS_SIZE.0);
                    let y1 = scale_coord(cell.y.saturating_add(cell.height), 720, CANVAS_SIZE.1).min(CANVAS_SIZE.1);
                    (x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1))
                } else {
                    (cell.x, cell.y, cell.width, cell.height)
                };

                let x = cx.min(canvas.width.saturating_sub(1));
                let y = cy.min(canvas.height.saturating_sub(1));
                let width = cw.max(1).min(canvas.width.saturating_sub(x));
                let height = ch.max(1).min(canvas.height.saturating_sub(y));
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
    #[allow(dead_code)]
    fn tick(&mut self, frames: &BTreeMap<u32, Arc<Frame>>, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
        self.elapsed_ms += 33;
        self.process_media(frames, audio_inputs);
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

    fn process_video(&mut self, frames: &BTreeMap<u32, Arc<Frame>>) {
        let t0 = std::time::Instant::now();
        let rects = self.scaled_slot_rects();
        let labels = self.slot_labels();
        let (width, height) = self.canvas;

        if let Some(exclusive) = Arc::get_mut(&mut self.composite) {
            if exclusive.width == width && exclusive.height == height {
                Compositor.compose_sized_with_labels_into(exclusive, frames, &rects, &labels);
                self.frames.clone_from(frames);
                self.stats.last_duration_us = t0.elapsed().as_micros() as u64;
                self.stats.frames_rendered += 1;
                return;
            }
        }

        let mut target_frame = self
            .scratch_frame
            .take()
            .unwrap_or_else(|| Frame::new(width, height, Vec::new()));
        target_frame.width = width;
        target_frame.height = height;
        Compositor.compose_sized_with_labels_into(&mut target_frame, frames, &rects, &labels);

        self.composite = Arc::new(target_frame);
        self.frames.clone_from(frames);
        self.stats.last_duration_us = t0.elapsed().as_micros() as u64;
        self.stats.frames_rendered += 1;
    }

    /// Пересадка по Codian §2.2 (S12, ADR-0029 фаза 3): главное окно —
    /// «Важному» участнику или активному спикеру, мелкие ячейки — по убыванию
    /// недавности речи.
    ///
    /// Жильцы только меняются местами: никто не пропадает из раскладки. Равная
    /// сетка и статичный макет PNM не трогаются (Codian §2.1: 52 макета
    /// фиксированы по геометрии). Пересчёт идёт по событию смены спикера, а не
    /// на каждом тике: если размещение уже верное, функция возвращает `None`.
    fn refresh_seating(&mut self) -> Option<(RoomEvent, SeatingMove)> {
        if !matches!(self.current_layout.kind, LayoutKind::Focus | LayoutKind::Speaker) {
            return None;
        }
        let main = vks_layout::main_window(&self.current_layout)?;
        // Показываем только тех, кто уже в раскладке: остальные — без видео.
        let shown: BTreeSet<ParticipantId> = self
            .current_layout
            .slots
            .iter()
            .filter_map(|slot| slot.participant_id)
            .map(|pid| ParticipantId::from_uuid(uuid::Uuid::from_u128(pid.as_u128())))
            .collect();
        // Порядок: «важный»/спикер, затем недавность речи, затем порядок прихода.
        let mut order: Vec<ParticipantId> = Vec::new();
        let focus = self.important.or(self.active_speaker);
        for pid in focus.iter().chain(self.recent_speakers.iter()).chain(self.join_order.iter()) {
            if shown.contains(pid) && !order.contains(pid) {
                order.push(*pid);
            }
        }
        // Главное окно первым, остальные — по возрастанию номера слота.
        let mut slots: Vec<vks_layout::SlotId> = self.current_layout.slots.iter().map(|slot| slot.id).collect();
        slots.sort_by_key(|slot| (*slot != main, slot.as_u32()));
        // `skip(1)`: первый участник уже занял главное окно.
        let mut rest = order.iter().skip(1);
        let desired: BTreeMap<vks_layout::SlotId, Option<ParticipantId>> = slots
            .iter()
            .map(|slot| {
                let want = if *slot == main { order.first().copied() } else { rest.next().copied() };
                (*slot, want)
            })
            .collect();
        let current = |slot: &vks_layout::Slot| {
            slot.participant_id.map(|pid| ParticipantId::from_uuid(uuid::Uuid::from_u128(pid.as_u128())))
        };
        if self.current_layout.slots.iter().all(|slot| desired.get(&slot.id).copied().flatten() == current(slot)) {
            return None;
        }
        let old_assignments: BTreeMap<ParticipantId, u32> = self
            .current_layout
            .slots
            .iter()
            .filter_map(|slot| current(slot).map(|pid| (pid, slot.id.as_u32())))
            .collect();
        for slot in &mut self.current_layout.slots {
            slot.participant_id = desired
                .get(&slot.id)
                .copied()
                .flatten()
                .map(|pid| vks_layout::ParticipantId::new(pid.as_uuid().as_u128()));
        }
        self.current_layout.bump_version();
        self.sync_descriptor_assignments();
        let event = RoomEvent::LayoutChanged { version: self.current_layout.version };
        let seating = SeatingMove { old_assignments, new_layout: self.current_layout.clone() };
        Some((event, seating))
    }

    /// Отмечает участника как недавнего спикера (MRU, ограниченный 16 записями).
    fn note_recent_speaker(&mut self, pid: ParticipantId) {
        self.recent_speakers.retain(|known| *known != pid);
        self.recent_speakers.insert(0, pid);
        self.recent_speakers.truncate(16);
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
                    if is_speaking {
                        // Codian §2.2: мелкие ячейки — по убыванию недавности речи.
                        self.note_recent_speaker(*pid);
                    }
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

        // Codian §2.2: главное окно — активному спикеру или «Важному»
        // участнику, мелкие ячейки — по недавности речи. Пересчёт — по событию
        // смены спикера, а не пересборка на каждом тике: если размещение уже
        // верное, `refresh_seating` ничего не меняет.
        let seating = self.refresh_seating().map(|(event, seating)| {
            events.push(event);
            seating
        });
        (events, seating)
    }

    #[allow(dead_code)]
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

/// `P1`: потолок коэффициента битрейта группы — 25 % к базовому.
pub const GROUP_BITRATE_CEILING_PCT: u32 = 125;

/// `P1`: битрейт группы по числу видимых окон композита.
/// Табличное квантование (корзины) числа окон для стабилизации битрейта энкодера
/// и предотвращения перезапусков FFmpeg при каждом входе/выходе участника.
#[must_use]
pub fn bucket_visible_windows(visible: usize) -> usize {
    match visible {
        0 | 1 => 1,
        2 => 2,
        3..=6 => 6,
        7..=12 => 12,
        13..=25 => 20,
        _ => 25,
    }
}

/// Вычисляет целевой групповой битрейт на основе базового и квантованного числа окон.
///
/// Правило (утверждено Lead'ом):
/// `bitrate = clamp(base × (1 + 0.02 × (bucketed − 1)), base, 1.25 × base)`,
/// `visible = 0` → `base`. Пустые/одно окно не поднимают битрейт: общий
/// энкодер на 20 окон требует больше полосы (BENCHMARK §10: +8.6 %), но не
/// больше 25 % сверх базового — иначе группа «съедает» полосу зрителей.
#[must_use]
pub fn group_bitrate_kbps(base_kbps: u32, visible_windows: usize) -> u32 {
    let bucketed = bucket_visible_windows(visible_windows);
    if bucketed <= 1 {
        return base_kbps;
    }
    let extra_pct = 2u64 * (bucketed as u64 - 1);
    let scaled = (u64::from(base_kbps) * (100 + extra_pct) / 100) as u32;
    let ceiling = u64::from(base_kbps) * u64::from(GROUP_BITRATE_CEILING_PCT) / 100;
    let ceiling = u32::try_from(ceiling).unwrap_or(u32::MAX);
    scaled.clamp(base_kbps, ceiling)
}

/// MCU engine (impl [`MediaEngine`]).
/// Room → (slot → frame): pluggable mock frame source.
type FrameSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<u32, Arc<Frame>>>>>;
/// Room → (participant → 20 ms PCM frame): pluggable mock audio source.
type AudioSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<ParticipantId, Vec<f32>>>>>;

/// Canonical lock order for [`McuEngine`] (T12; fixes the P0 deadlock risk in
/// `docs/audit/2026-10-29/pitfalls/T12.md` §P0-3):
///
/// ```text
/// bridge_tick_rooms → pending_layouts → rooms → frames → audio
/// ```
///
/// `bridge_tick_rooms` is only ever taken alone (never while another engine lock
/// is held) and `audio` is always the innermost lock, so every method takes
/// locks **left to right**. Two inversions existed before
/// T12 and are fixed here:
/// - `apply_layout` took `rooms → pending_layouts`, while
///   `ensure_room`/`bridge_participant`/`seat_publisher` take
///   `pending_layouts → rooms`;
/// - `remove_publisher` took `rooms → audio → frames`, while `tick`/`tick_at`
///   take `rooms → frames → audio`.
///
/// The room event sink is invoked only after all engine locks are released
/// (see the private `emit` sink dispatch): a sink may take roster/room-hub locks that are not
/// part of this order.
#[derive(Debug, Default)]
pub struct McuEngine {
    rooms: Arc<std::sync::RwLock<BTreeMap<RoomId, Arc<Mutex<RoomState>>>>>,
    /// `P1`: авторитетный канал числа видимых окон на комнату (один на комнату,
    /// пишет только движок через [`McuEngine::refresh_visible_windows`], сессии
    /// лишь читают). Устраняет thrash от per-session писателей.
    visible: Mutex<BTreeMap<RoomId, Arc<VisibleWindows>>>,
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
    /// `O7`: ферма энкодеров — один энкодер на группу (комната + профиль + холст).
    /// Живёт рядом с раскладкой: это свойство медиаядра, а не HTTP-слоя.
    farm: EncoderFarm,
    /// Accepted audio ticks (T12): lets the mixed-room bridge prove that the
    /// gateway RTP loop and the bridge clock do not double-tick the same room.
    audio_ticks: std::sync::atomic::AtomicU64,
    /// Rooms whose 20 ms audio clock is owned by the server's hybrid bridge
    /// (T12). While a room is listed, [`McuEngine::tick_audio_at`] is a no-op
    /// for every other caller (gateway RTP loop, AAC egress), so no path can
    /// advance a room's mix twice per 20 ms; the bridge calls
    /// [`McuEngine::tick_audio_at_bridge`]. Leaf lock: never taken while another
    /// engine lock is held.
    bridge_tick_rooms: Mutex<std::collections::BTreeSet<RoomId>>,
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
        // Не финальная ошибка: вызывающий обязан расширить раскладку
        // (`regrid_for_video`) — инвариант «видео-участник без слота не бывает».
        tracing::debug!(%pid, "no free geometry slot; auto-expanding the layout");
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

/// Перестраивает раскладку в равную сетку на всех видео-участников.
///
/// Функциональный блокер 20+: с сервера приходят раскладки на меньшее число
/// окон (пресет `preset_default_13`; либо `geometry` из одного окна, когда
/// `assignments` пуст и `slot_count = max(assignments)+1 = 1`). Лишние
/// участники оставались без слота — их кадры молча отбрасывались
/// (`set_participant_frame` → `false`), и 7–8 из 20 не попадали в композит.
///
/// Раскладка перестраивается целиком: `geometry` — из `GridLayout::compute`,
/// назначения — по порядку прихода (S12). Сохранять rect'ы первых N окон
/// нельзя: они от пресета другого размера, и новые окна перекрывали бы их.
/// `preset` очищается, чтобы прежний пресет не «возвращал» малое число окон.
fn regrid_for_video(layout: &mut LayoutDescriptor, bridged: &[ParticipantId]) -> bool {
    if bridged.is_empty() {
        return false;
    }
    let previous_windows = layout.geometry.len();
    let canvas = Canvas::new(CANVAS_SIZE.0, CANVAS_SIZE.1);
    let rects = GridLayout::compute(&canvas, bridged.len());
    if rects.len() < bridged.len() {
        return false;
    }
    layout.geometry = rects
        .iter()
        .enumerate()
        .map(|(index, rect)| LayoutSlot {
            slot: index as u32,
            x: rect.x,
            y: rect.y,
            width: rect.w,
            height: rect.h,
            z_order: 0,
        })
        .collect();
    layout.assignments = bridged.iter().enumerate().map(|(index, pid)| (*pid, index as u32)).collect();
    layout.preset = String::new();
    layout.mode = "auto".into();
    layout.version = layout.version.saturating_add(1);
    tracing::info!(
        participants = bridged.len(),
        previous_windows,
        "layout auto-expanded to an equal grid: geometry windows < video participants"
    );
    true
}

/// Атомарный снимок раскладки: эффективная геометрия + справочные метаданные
/// применённого дескриптора (T10a). Один лок на комнату — версия геометрии и
/// метаданные всегда из одного применения.
#[derive(Debug, Clone)]
pub struct LayoutSnapshot {
    /// То, что реально рисует композитор (`LayoutChanged.slots`).
    pub layout: vks_layout::Layout,
    /// Id пресета из админ-состояния; "" = нет/неизвестно.
    pub preset: String,
    /// Режим редактора (`auto`/`dynamic`/`manual`); "" = неизвестно.
    pub mode: String,
}

/// `P1`: авторитетное значение числа видимых окон + дебаунс публикации.
///
/// Сессии не пишут значение сами (иначе 10 render-тасков перетирают `watch`
/// переходными числами); пишет только движок, и только когда новое число
/// держится стабильно дольше [`VISIBLE_WINDOWS_STABLE`].
#[derive(Debug)]
struct VisibleWindows {
    tx: tokio::sync::watch::Sender<usize>,
    inner: Mutex<VisibleWindowsInner>,
}

#[derive(Debug)]
struct VisibleWindowsInner {
    current: usize,
    pending: usize,
    since: std::time::Instant,
}

/// `P1`: сколько число видимых окон должно продержаться, прежде чем сменить
/// битрейт группы (гистерезис против флаппинга при входе/выходе участников).
pub const VISIBLE_WINDOWS_STABLE: std::time::Duration = std::time::Duration::from_millis(500);

impl VisibleWindows {
    fn new(initial: usize) -> Self {
        let (tx, _) = tokio::sync::watch::channel(initial);
        Self {
            tx,
            inner: Mutex::new(VisibleWindowsInner {
                current: initial,
                pending: initial,
                since: std::time::Instant::now(),
            }),
        }
    }
}

impl McuEngine {
    /// Creates an empty MCU engine.
    pub fn new() -> Self {
        Self::default()
    }

    fn room_arc(&self, room: RoomId) -> Option<Arc<Mutex<RoomState>>> {
        self.rooms.read().unwrap().get(&room).cloned()
    }

    fn with_room<R>(&self, room: RoomId, f: impl FnOnce(&RoomState) -> R) -> Option<R> {
        let arc = self.room_arc(room)?;
        let g = arc.lock().unwrap();
        Some(f(&g))
    }

    fn with_room_mut<R>(&self, room: RoomId, f: impl FnOnce(&mut RoomState) -> R) -> Option<R> {
        let arc = self.room_arc(room)?;
        let mut g = arc.lock().unwrap();
        Some(f(&mut g))
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
        let arc = self.room_arc(room)?;
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let audio_map = self.audio.lock().unwrap().get(&room).cloned().unwrap_or_default();
        let snapshot = {
            let mut state = arc.lock().unwrap();
            let audio_inputs: Vec<(ParticipantId, Vec<f32>)> = state
                .publisher_tracks
                .keys()
                .filter_map(|pid| audio_map.get(pid).cloned().map(|samples| (*pid, samples)))
                .collect();
            state.elapsed_ms += 33;
            state.process_audio(&audio_inputs);
            let rects = state.scaled_slot_rects();
            let labels = state.slot_labels();
            let (width, height) = state.canvas;
            (width, height, rects, labels)
        };
        let (width, height, rects, labels) = snapshot;
        let t0 = std::time::Instant::now();
        let composite = Arc::new(Compositor.compose_sized_with_labels(width, height, &frames, &rects, &labels));
        let duration_us = t0.elapsed().as_micros() as u64;
        {
            let mut state = arc.lock().unwrap();
            state.composite = composite.clone();
            state.frames = frames;
            state.stats.last_duration_us = duration_us;
            state.stats.frames_rendered += 1;
        }
        Some(composite)
    }

    /// Advances the room's pipeline at a specific monotonic timestamp `now_ms`.
    /// Returns Some(Frame) if rendered, or None if dropped by backpressure.
    pub fn tick_at(&self, room: RoomId, now_ms: u64) -> Option<Arc<Frame>> {
        self.tick_audio_at_impl(room, now_ms);
        self.tick_video_at(room, now_ms)
    }

    /// Advances only the audio pipeline at a specific monotonic timestamp `now_ms` (20 ms cadence).
    ///
    /// Returns true if an audio tick was executed, or false if rejected by the
    /// 18 ms guard **or if the room's tick is owned by the hybrid bridge**
    /// ([`McuEngine::set_bridge_tick_owner`]): one clock per room.
    pub fn tick_audio_at(&self, room: RoomId, now_ms: u64) -> bool {
        if self.bridge_owns_tick(room) {
            return false;
        }
        self.tick_audio_at_impl(room, now_ms)
    }

    /// Bridge-privileged 20 ms tick (T12): ignores the tick-ownership guard.
    ///
    /// Only the mixed-room bridge task calls this while it owns the room clock.
    pub fn tick_audio_at_bridge(&self, room: RoomId, now_ms: u64) -> bool {
        self.tick_audio_at_impl(room, now_ms)
    }

    fn tick_audio_at_impl(&self, room: RoomId, now_ms: u64) -> bool {
        let (ran, events, seating) = {
            let arc = match self.room_arc(room) {
                Some(s) => s,
                None => return false,
            };
            let mut state = arc.lock().unwrap();
            let audio_map = self.audio.lock().unwrap().get(&room).cloned().unwrap_or_default();
            let audio_inputs: Vec<(ParticipantId, Vec<f32>)> = state
                .publisher_tracks
                .keys()
                .filter_map(|pid| audio_map.get(pid).cloned().map(|samples| (*pid, samples)))
                .collect();
            state.tick_audio_at(now_ms, &audio_inputs)
        };
        if ran {
            self.audio_ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // Кадр принадлежит участнику: после голосовой пересадки переносим
        // пиксели в новые слоты, иначе в главном окне остаётся видео прежнего
        // жильца (и навсегда, если тот выключил камеру).
        if let Some(seating) = seating {
            self.rekey_frames(room, &seating.old_assignments, &seating.new_layout);
        }
        self.emit(room, events);
        ran
    }

    /// Marks the room's 20 ms audio clock as owned by the hybrid bridge.
    ///
    /// While owned, [`McuEngine::tick_audio_at`] returns `false` without advancing
    /// the mix (gateway RTP loop and AAC egress become non-tickers), so the mix
    /// cannot be double-advanced when a browser and a gateway share a room.
    pub fn set_bridge_tick_owner(&self, room: RoomId, owned: bool) {
        let mut g = self.bridge_tick_rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if owned {
            g.insert(room);
        } else {
            g.remove(&room);
        }
    }

    /// Whether the hybrid bridge currently owns this room's audio tick.
    pub fn bridge_owns_tick(&self, room: RoomId) -> bool {
        self.bridge_tick_rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&room)
    }

    /// Accepted audio-tick count since start (T12 single-clock proof).
    ///
    /// The mixed-room bridge owns the 20 ms tick while it is active; the
    /// gateway RTP loop must then skip `tick_audio_at`. A test can assert that
    /// the room is ticked ~50/s instead of ~100/s.
    pub fn audio_ticks_total(&self) -> u64 {
        self.audio_ticks.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Mix only the listed participants' current 20 ms PCM slices.
    ///
    /// Used by the mixed-room audio bridge for the **gateway-only** egress mix:
    /// browsers keep receiving each other through the SFU fanout, so their PCM
    /// must not be part of the stream the bridge sends back to them (that would
    /// double-deliver every browser voice and would echo the listener to
    /// itself). Gain/soft-clip are the same [`AudioMixer`] as the room mix.
    ///
    /// Takes only the audio lock: it must stay safe to call on the bridge clock
    /// while the room is being ticked.
    pub fn mix_participants_audio(&self, room: RoomId, participants: &[ParticipantId]) -> Vec<f32> {
        let audio_guard = self.audio.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(room_audio) = audio_guard.get(&room) else {
            return vec![0.0; 960];
        };
        let slices: Vec<&[f32]> =
            participants.iter().filter_map(|pid| room_audio.get(pid).map(Vec::as_slice)).collect();
        if slices.is_empty() {
            return vec![0.0; 960];
        }
        AudioMixer.mix(&slices, 960)
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
            let arc = match self.room_arc(room) {
                Some(a) => a,
                None => return false,
            };
            let mut state = arc.lock().unwrap();
            state.important = participant;
            state.refresh_seating()
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
        self.with_room(room, |state| state.important).flatten()
    }

    /// Advances only the video pipeline at a specific monotonic timestamp `now_ms` (33 ms budget).
    /// Returns Some(Frame) if rendered, or None if dropped by frame-dropping backpressure.
    pub fn tick_video_at(&self, room: RoomId, now_ms: u64) -> Option<Arc<Frame>> {
        let arc = self.room_arc(room)?;
        let snapshot = {
            let mut state = arc.lock().unwrap();
            if let Some(last) = state.last_render_ms {
                if now_ms < last + 30 {
                    state.stats.frames_dropped += 1;
                    return None;
                }
            }
            state.last_render_ms = Some(now_ms);
            let applied_at = *state.layout_applied_at_ms.get_or_insert(now_ms);
            state.elapsed_ms = now_ms.saturating_sub(applied_at);
            let rects = state.scaled_slot_rects();
            let labels = state.slot_labels();
            let (width, height) = state.canvas;
            let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
            (width, height, rects, frames, labels)
        };
        let (width, height, rects, frames, labels) = snapshot;
        let started = vks_common::metrics::mcu_tick_metrics_enabled().then(std::time::Instant::now);
        let t0 = std::time::Instant::now();
        let composite = Arc::new(Compositor.compose_sized_with_labels(width, height, &frames, &rects, &labels));
        let duration_us = t0.elapsed().as_micros() as u64;
        if let Some(started) = started {
            vks_common::metrics::observe_mcu_tick(vks_common::metrics::McuTickPhase::Video, started.elapsed());
        }
        {
            let mut state = arc.lock().unwrap();
            state.composite = composite.clone();
            state.frames = frames;
            state.stats.last_duration_us = duration_us;
            state.stats.frames_rendered += 1;
        }
        Some(composite)
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
        let arc = self.room_arc(room)?;
        let snapshot = {
            let state = arc.lock().unwrap();
            let base = state.composite.clone();
            if !state.self_view {
                return Some(base);
            }
            let rects = state.scaled_slot_rects();
            let viewer_uuid = viewer.as_uuid().as_u128();
            let own_slot = state
                .current_layout
                .slots
                .iter()
                .find(|slot| slot.participant_id.map(|pid| pid.as_u128()) == Some(viewer_uuid))
                .map(|s| s.id.as_u32());
            let Some(own_slot) = own_slot else { return Some(base) };
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
                return Some(base);
            }
            let Some(own_rect) = rects.get(&own_slot).copied() else { return Some(base) };
            let Some(self_rect) = rects.get(&self_slot).copied() else { return Some(base) };
            let camera = state.frames.get(&own_slot).cloned();
            (base, own_rect, self_rect, camera)
        };
        let (base, own_rect, self_rect, camera) = snapshot;
        let mut canvas = Frame::new(base.width, base.height, base.data.clone());
        Compositor::clear(&mut canvas, &own_rect);
        if let Some(camera) = camera {
            Compositor::blit(&mut canvas, &camera, &self_rect);
        }
        Some(Arc::new(canvas))
    }

    /// Размер холста комнаты (`O1`), если комната существует.
    pub fn composite_size(&self, room: RoomId) -> Option<(u32, u32)> {
        self.with_room(room, |state| state.canvas)
    }

    /// `O1`: выставляет холст комнаты явно (тест/админка).
    ///
    /// `false`, если комнаты нет или размер нулевой. Композит заменяется
    /// чёрным кадром нужного размера, чтобы egress не получил кадр с чужими
    /// размерами до ближайшего тика (resize всё равно бы справился, но так
    /// проще рассуждать).
    pub fn set_composite_size(&self, room: RoomId, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        let changed = self.with_room_mut(room, |state| {
            let changed = state.canvas != (width, height);
            Self::resize_canvas(state, width, height);
            changed
        });
        let Some(changed) = changed else { return false };
        if changed {
            self.farm.invalidate_room(room);
        }
        true
    }

    /// `O1`: делает холст комнаты равным выходному профилю, если комната ещё
    /// не рендерила ни кадра.
    ///
    /// Первая сессия задаёт холст (типовой случай: все участники комнаты
    /// договариваются об одном профиле), дальше размер не меняется — иначе
    /// уже отдаваемые кадры меняли бы геометрию на ходу. Зритель с другим
    /// профилем по-прежнему получает обычный `resize_i420`.
    pub fn ensure_composite_size(&self, room: RoomId, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }
        let res = self.with_room_mut(room, |state| {
            if state.stats.frames_rendered > 0 {
                return None;
            }
            let changed = state.canvas != (width, height);
            Self::resize_canvas(state, width, height);
            Some(changed)
        });
        match res {
            Some(Some(changed)) => {
                if changed {
                    self.farm.invalidate_room(room);
                }
                true
            }
            _ => false,
        }
    }

    /// Заменяет холст комнаты и её текущий композит чёрным кадром размера
    /// `width×height` (YUV420, если размеры чётные).
    fn resize_canvas(state: &mut RoomState, width: u32, height: u32) {
        if state.canvas == (width, height) {
            return;
        }
        state.canvas = (width, height);
        let pixels = width as usize * height as usize;
        let data = if width.is_multiple_of(2) && height.is_multiple_of(2) {
            let mut data = vec![128u8; pixels * 3 / 2];
            Compositor::fill_studio_black(&mut data, pixels);
            data
        } else {
            vec![0u8; pixels]
        };
        state.composite = Arc::new(Frame::new(width, height, data));
    }

    /// `O7`: ферма энкодеров (диагностика/метрики/тесты).
    pub fn video_farm(&self) -> &EncoderFarm {
        &self.farm
    }

    /// Включён ли самопросмотр в комнате: ферма и self-view несовместимы
    /// (разным зрителям нужен разный кадр), такой зритель идёт старым путём.
    pub fn self_view(&self, room: RoomId) -> bool {
        self.with_room(room, |state| state.self_view).unwrap_or(false)
    }

    /// `O7`: подписка зрителя на групповой энкодер.
    ///
    /// Ключ = комната + выходной профиль + версия холста; вход группы — холст
    /// комнаты (egress-resize, если профиль отличается, делает сам FFmpeg).
    pub async fn acquire_video_viewer(&self, room: RoomId, spec: &EncoderSpec) -> Result<GroupViewer, FarmError> {
        let canvas = self.composite_size(room).unwrap_or((spec.width, spec.height));
        let key = spec.key(room, self.farm.canvas_version(room));
        self.farm.acquire(key, canvas).await
    }

    /// `O7`: публикует общий кадр комнаты всем её группам (дедуп по кадру).
    pub fn publish_video_frame(&self, room: RoomId, frame: &Arc<Frame>) -> usize {
        self.farm.publish(room, frame)
    }

    /// `O7`: PLI/FIR любого зрителя → запрос keyframe группе (v1: учитывается,
    /// IDR приходит по keyint ≤1 с; форс-IDR — P4b).
    pub fn request_video_keyframe(&self, room: RoomId) {
        self.farm.request_keyframe(room);
    }

    /// `O7`: диагностика групп (refcount/fps/keyframes/restarts).
    pub fn video_farm_stats(&self) -> Vec<GroupStat> {
        let mut stats = self.farm.snapshot();
        // `P1`: число видимых окон — свойство комнаты, не группы; ферма о
        // раскладке ничего не знает, поэтому обогащаем снимок здесь.
        for stat in &mut stats {
            stat.visible_windows = self.visible_windows(stat.key.room);
        }
        stats
    }

    /// Включает самопросмотр в комнате (ADR-0029, фаза 2).
    ///
    /// По умолчанию выключен: общий кадр не меняется, поведение комнаты
    /// остаётся прежним до явного включения.
    pub fn set_self_view(&self, room: RoomId, enabled: bool) {
        self.with_room_mut(room, |state| {
            state.self_view = enabled;
        });
    }

    /// Returns the rendering and backpressure stats of a room (M3.6).
    pub fn stats(&self, room: RoomId) -> Option<McuRenderStats> {
        self.with_room(room, |s| s.stats)
    }

    /// Applied layout descriptor (test/inspector), including staged pending
    /// layouts for rooms that do not exist yet.
    pub fn layout_descriptor(&self, room: RoomId) -> Option<LayoutDescriptor> {
        if let Some(layout) = self.with_room(room, |state| state.layout.clone()).flatten() {
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
        // Один источник истины — `layout_snapshot` (включая pending-комнаты).
        self.layout_snapshot(room).map(|snapshot| snapshot.layout)
    }

    /// Снимок раскладки для провода: эффективная геометрия и метаданные
    /// одного и того же применения (T10a). В отличие от пары
    /// `current_layout` + `layout_descriptor`, оба значения читаются под одним
    /// локом и не могут разъехаться по версии.
    pub fn layout_snapshot(&self, room: RoomId) -> Option<LayoutSnapshot> {
        if let Some(snapshot) = self.with_room(room, |state| {
            let (preset, mode) = state
                .layout
                .as_ref()
                .map(|descriptor| (descriptor.preset.clone(), descriptor.mode.clone()))
                .unwrap_or_default();
            LayoutSnapshot { layout: state.current_layout.clone(), preset, mode }
        }) {
            return Some(snapshot);
        }
        let pending = self.pending_layouts.lock().unwrap().get(&room).cloned()?;
        Some(LayoutSnapshot {
            layout: RoomState::layout_from_descriptor(&pending),
            preset: pending.preset,
            mode: pending.mode,
        })
    }

    /// The current crossfaded slot rects of a room (test/inspector).
    pub fn slot_rects(&self, room: RoomId) -> Option<SlotRects> {
        self.with_room(room, |s| s.slot_rects())
    }

    /// `P1`: число **занятых** ячеек композита — окно, в которое посажен
    /// участник с видео (есть назначение слота в текущей геометрии).
    ///
    /// Основание: правило битрейта группы ([`group_bitrate_kbps`] — решение
    /// Lead'а по BENCHMARK §10: общий энкодер на 20 окон даёт +8.6 % битрейта).
    /// Считаем именно занятость ячеек, а не наличие декодированного кадра:
    /// ячейка с чёрным/ещё не декодированным кадром — всё равно окно
    /// композита (и окно энкодера), а пустая ячейка (участник без видео) —
    /// нет. Пустые слоты в геометрию не попадают, поэтому двойная проверка
    /// `assignments ∩ geometry` отсекает назначения вне сетки.
    /// `0` — видеопубликаторов в комнате нет (или комнаты нет).
    #[must_use]
    pub fn visible_windows(&self, room: RoomId) -> usize {
        self.with_room(room, |state| {
            state.current_layout.slots.iter().filter(|slot| slot.participant_id.is_some()).count()
        }).unwrap_or(0)
    }

    /// `P1`: подписка на авторитетное число видимых окон комнаты.
    ///
    /// Возвращает текущее значение (для spec в прологе моста); изменения
    /// публикует только [`Self::refresh_visible_windows`] — один источник на
    /// комнату, поэтому сессии не перетирают друг другу канал.
    #[must_use]
    pub fn visible_windows_receiver(&self, room: RoomId) -> tokio::sync::watch::Receiver<usize> {
        let current = self.visible_windows(room);
        let mut map = self.visible.lock().unwrap();
        let entry = map.entry(room).or_insert_with(|| Arc::new(VisibleWindows::new(current)));
        entry.tx.subscribe()
    }

    /// `P1`: пересчитывает окна комнаты и публикует смену, если новое значение
    /// стабильно дольше [`VISIBLE_WINDOWS_STABLE`] (гистерезис).
    ///
    /// Дёргается из render-тасков сессий (это и есть тик дебаунса); значение
    /// считается по общему состоянию движка, поэтому все сессии видят одно
    /// и то же число и не могут «перетереть» канал разными значениями.
    /// `Some(n)` — канал обновлён, `None` — ещё рано/без изменений.
    pub fn refresh_visible_windows(&self, room: RoomId) -> Option<usize> {
        let visible = self.visible_windows(room);
        let state = {
            let mut map = self.visible.lock().unwrap();
            map.entry(room).or_insert_with(|| Arc::new(VisibleWindows::new(visible))).clone()
        };
        let mut inner = state.inner.lock().unwrap();
        if visible == inner.current {
            inner.pending = visible;
            return None;
        }
        if inner.pending != visible {
            inner.pending = visible;
            inner.since = std::time::Instant::now();
            tracing::debug!(%room, visible, current = inner.current, "visible windows changed; waiting for stability");
            return None;
        }
        if inner.since.elapsed() < VISIBLE_WINDOWS_STABLE {
            return None;
        }
        inner.current = visible;
        drop(inner);
        let _ = state.tx.send(visible);
        tracing::info!(%room, visible, "visible windows stable; encoder groups may re-key");
        Some(visible)
    }

    /// Returns the personal N-1 mix-minus audio for a participant in the room.
    pub fn participant_audio(&self, room: RoomId, pid: ParticipantId) -> Option<Vec<f32>> {
        self.with_room(room, |state| {
            state.participant_audio.get(&pid).cloned().or_else(|| {
                if state.subscribers.contains(&pid) || state.publisher_tracks.contains_key(&pid) {
                    Some(state.mixed_audio.clone())
                } else {
                    None
                }
            })
        })
        .flatten()
    }

    /// Copies the personal N-1 mix-minus audio into a mutable slice without heap allocation.
    pub fn copy_participant_audio(&self, room: RoomId, pid: ParticipantId, out: &mut [f32]) -> bool {
        self.with_room(room, |state| {
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
        })
        .unwrap_or(false)
    }

    /// Returns the global room audio mix (for recording / monitoring).
    pub fn mixed_audio(&self, room: RoomId) -> Option<Vec<f32>> {
        self.with_room(room, |s| s.mixed_audio.clone())
    }

    /// Returns the active speaker determined by per-participant VAD.
    /// Забирает накопленные изменения состояния «говорит» (S12).
    ///
    /// Потребитель (сервер) дренирует список после audio-тика и обновляет
    /// ростер + рассылает `SpeakingStateChanged`: без этого состояние
    /// «говорит» не покидало бы движок.
    pub fn take_speaking_changes(&self, room: RoomId) -> Vec<(ParticipantId, bool)> {
        self.with_room_mut(room, |state| std::mem::take(&mut state.speaking_changes)).unwrap_or_default()
    }

    pub fn active_speaker(&self, room: RoomId) -> Option<ParticipantId> {
        self.with_room(room, |s| s.active_speaker).flatten()
    }

    /// Returns whether the given participant is currently speaking.
    pub fn is_speaking(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.with_room(room, |s| s.speaking_states.get(&pid).copied()).flatten().unwrap_or(false)
    }

    /// Ensures a room exists in the MCU engine.
    pub fn ensure_room(&self, room: RoomId) {
        let mut pending = self.pending_layouts.lock().unwrap().remove(&room);
        if let Some(layout) = pending.as_mut() {
            sanitize_assignments(layout);
        }
        let arc = {
            let mut g = self.rooms.write().unwrap();
            g.entry(room).or_insert_with(|| Arc::new(Mutex::new(RoomState::new(RoomCaps::default())))).clone()
        };
        let mut state = arc.lock().unwrap();
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
        let arc = {
            let mut g = self.rooms.write().unwrap();
            g.entry(room).or_insert_with(|| Arc::new(Mutex::new(RoomState::new(RoomCaps::default())))).clone()
        };
        let mut state = arc.lock().unwrap();
        state.subscribers.insert(pid);
        // Pending применяем и к audio-only bridge: SIP/H.323 может прийти без
        // видео (или видео деградировало), но выбранная до дозвона раскладка
        // не должна теряться. Видео-участник затем сядет в её geometry-слот.
        if state.layout.is_none() {
            if let Some(layout) = pending {
                state.adopt_layout(layout);
            }
        }
        Self::register_publisher(&mut state, pid, tracks);
    }

    /// Registers a video publisher and seats it in a free layout slot **without**
    /// making it a subscriber of the composite.
    ///
    /// Нужен транспорту WebRTC: браузер получает медиа через собственный
    /// fanout (str0m), а не через композит MCU, — но он обязан занимать слот
    /// раскладки, иначе `set_participant_frame` молча выбросит его кадры.
    pub fn seat_publisher(&self, room: RoomId, pid: ParticipantId, tracks: Vec<MediaTrack>) {
        let pending = self.pending_layouts.lock().unwrap().remove(&room);
        let arc = {
            let mut g = self.rooms.write().unwrap();
            g.entry(room).or_insert_with(|| Arc::new(Mutex::new(RoomState::new(RoomCaps::default())))).clone()
        };
        let mut state = arc.lock().unwrap();
        if state.layout.is_none() {
            if let Some(layout) = pending {
                state.adopt_layout(layout);
            }
        }
        Self::register_publisher(&mut state, pid, tracks);
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
        if state.seat_video_or_expand(pid) {
            let descriptor = state.layout.as_ref().expect("layout set by seat_video_or_expand");
            let layout = RoomState::layout_from_descriptor(descriptor);
            state.bridge.apply(&layout);
            state.current_layout = layout;
            state.layout_applied_at_ms = None;
            state.elapsed_ms = 0;
        }
    }

    /// Writes a decoded frame only into the slot belonging to this publisher.
    pub fn set_participant_frame(&self, room: RoomId, pid: ParticipantId, frame: Frame) -> bool {
        let slot = self
            .with_room(room, |state| state.layout.as_ref().and_then(|layout| layout.assignments.get(&pid).copied()))
            .flatten();
        let Some(slot) = slot else {
            return false;
        };
        self.frames.lock().unwrap().entry(room).or_default().insert(slot, Arc::new(frame));
        true
    }

    /// Removes the participant's decoded pixels immediately when reception is muted.
    pub fn clear_participant_frame(&self, room: RoomId, pid: ParticipantId) {
        let slot = self
            .with_room(room, |state| state.layout.as_ref().and_then(|layout| layout.assignments.get(&pid).copied()))
            .flatten();
        if let Some(slot) = slot {
            self.frames.lock().unwrap().entry(room).or_default().remove(&slot);
            let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
            self.with_room_mut(room, |state| state.process_video(&frames));
        }
    }

    /// Current composed pixels, shared by subscribers when a render is throttled.
    pub fn composite_frame(&self, room: RoomId) -> Option<Arc<Frame>> {
        self.with_room(room, |state| state.composite.clone())
    }

    /// Sets a participant's display name label for the room's compositor layout.
    pub fn set_participant_label(&self, room: RoomId, pid: ParticipantId, label: String) {
        self.with_room_mut(room, |state| {
            state.participant_labels.insert(pid, label);
        });
    }

    /// Sets visibility of a participant's display name label in the room's compositor layout.
    pub fn set_participant_label_visibility(&self, room: RoomId, pid: ParticipantId, visible: bool) {
        self.with_room_mut(room, |state| {
            state.participant_label_visibility.insert(pid, visible);
        });
    }

    /// Synchronously removes a bridged participant from the MCU room, purging layout and audio.
    pub fn unbridge_participant(&self, room: RoomId, pid: ParticipantId) {
        let arc = match self.room_arc(room) {
            Some(a) => a,
            None => return,
        };
        let mut state = arc.lock().unwrap();
        state.subscribers.remove(&pid);
        state.publisher_tracks.remove(&pid);
        state.join_order.retain(|known| *known != pid);
        state.vads.remove(&pid);
        state.speaking_states.remove(&pid);
        state.participant_labels.remove(&pid);
        state.participant_label_visibility.remove(&pid);
        if state.active_speaker == Some(pid) {
            state.active_speaker = None;
        }
        state.participant_audio.remove(&pid);

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
        // T12 lock order: frames before audio (same inversion as
        // `remove_publisher`).
        self.audio.lock().unwrap().entry(room).or_default().remove(&pid);
    }

    /// Returns whether the participant is registered in the MCU room.
    pub fn has_participant(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.with_room(room, |s| s.subscribers.contains(&pid) || s.publisher_tracks.contains_key(&pid))
            .unwrap_or(false)
    }
}

#[async_trait]
impl MediaEngine for McuEngine {
    fn mode(&self) -> RoomMode {
        RoomMode::Mcu
    }

    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError> {
        let mut pending = self.pending_layouts.lock().unwrap();
        let mut g = self.rooms.write().unwrap();
        if g.contains_key(&cfg.room_id) {
            return Err(MediaEngineError::RoomAlreadyExists(cfg.room_id));
        }
        let mut state = RoomState::new(cfg.caps);
        // Adopt a layout saved by apply_layout before the first viewer arrives.
        // Lock order: pending_layouts -> rooms, matching apply_layout.
        if let Some(layout) = pending.remove(&cfg.room_id) {
            state.adopt_layout(layout);
        }
        g.insert(cfg.room_id, Arc::new(Mutex::new(state)));
        Ok(RoomMediaHandle { room: cfg.room_id, mode: RoomMode::Mcu })
    }

    async fn destroy_room(&self, room: RoomId) -> Result<(), MediaEngineError> {
        // Retain the canonical `pending_layouts -> rooms` temporal order even
        // though the critical sections are disjoint; the pending layout is only
        // an eventual layout transition and never depends on room state.
        let mut pending = self.pending_layouts.lock().unwrap();
        pending.remove(&room);
        drop(pending);
        let removed = {
            let mut g = self.rooms.write().unwrap();
            g.remove(&room).is_some()
        };
        if !removed {
            return Err(MediaEngineError::RoomNotFound(room));
        }
        self.frames.lock().unwrap().remove(&room);
        self.audio.lock().unwrap().remove(&room);
        self.visible.lock().unwrap().remove(&room);
        self.bridge_tick_rooms.lock().unwrap().remove(&room);
        Ok(())
    }

    async fn switch_room_mode(&self, room: RoomId, _new_mode: RoomMode) -> Result<RoomMode, MediaEngineError> {
        // The MCU engine is mode-fixed: it always serves RoomMode::Mcu.
        // An in-place SFU↔MCU switch is coordinated by the core (ADR-0026);
        // here we report the mode this engine serves.
        let exists = self.rooms.read().unwrap().contains_key(&room);
        if exists {
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
        self.with_room_mut(room, |state| {
            state.publisher_tracks.insert(publisher, tracks);
            if !state.join_order.contains(&publisher) {
                state.join_order.push(publisher);
            }
        })
        .ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(PublisherHandle { room, publisher })
    }

    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError> {
        let arc = self.room_arc(room).ok_or(MediaEngineError::RoomNotFound(room))?;
        let mut state = arc.lock().unwrap();
        state.publisher_tracks.remove(&publisher);
        state.join_order.retain(|known| *known != publisher);
        state.vads.remove(&publisher);
        state.speaking_states.remove(&publisher);
        if state.active_speaker == Some(publisher) {
            state.active_speaker = None;
        }
        state.participant_audio.remove(&publisher);

        // T12 lock order: rooms → frames → audio. `frames` is taken before
        // `audio` (this used to be rooms → audio → frames, the exact inverse of
        // `tick`/`tick_at`); the audio map is cleared last.
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
        self.audio.lock().unwrap().entry(room).or_default().remove(&publisher);
        Ok(())
    }

    async fn add_subscriber(
        &self,
        room: RoomId,
        subscriber: ParticipantId,
        _req: SubscribeRequest,
    ) -> Result<SubscriberHandle, MediaEngineError> {
        self.with_room_mut(room, |state| {
            state.subscribers.insert(subscriber);
        })
        .ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(SubscriberHandle { room, subscriber })
    }

    async fn remove_subscriber(&self, room: RoomId, subscriber: ParticipantId) -> Result<(), MediaEngineError> {
        self.with_room_mut(room, |state| {
            state.subscribers.remove(&subscriber);
            state.participant_audio.remove(&subscriber);
        })
        .ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(())
    }

    async fn apply_layout(&self, room: RoomId, mut layout: LayoutDescriptor) -> Result<(), MediaEngineError> {
        sanitize_assignments(&mut layout);
        // T12 lock order: pending_layouts → rooms. This used to take `rooms`
        // first and then `pending_layouts`, the inverse of
        // `ensure_room`/`bridge_participant`/`seat_publisher` → a concurrent
        // gateway landing and layout apply could deadlock (P0-3).
        let mut pending = self.pending_layouts.lock().unwrap();
        let arc = self.room_arc(room);
        let Some(arc) = arc else {
            // SFU-сессия без шлюза не должна аллоцировать MCU-композит.
            pending.insert(room, layout);
            return Ok(());
        };
        // Комната уже есть: pending устарел, иначе он мог бы «оживить» старую
        // раскладку на следующем bridge.
        pending.remove(&room);
        drop(pending);
        let mut state = arc.lock().unwrap();
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
        let mut unseated = false;
        for &pid in &bridged {
            if !layout.assignments.contains_key(&pid) && !seat_participant(&mut layout, pid) {
                unseated = true;
            }
        }
        if unseated {
            // Окон в присланной geometry меньше, чем видео-участников:
            // расширяем раскладку равной сеткой, иначе часть людей не попадает
            // в композит (функциональный блокер 20+, `preset_default_13`).
            regrid_for_video(&mut layout, &bridged);
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
        self.with_room_mut(room, |state| {
            state.caps = caps;
        })
        .ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(())
    }

    async fn list_publishers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        self.with_room(room, |state| state.publisher_tracks.keys().copied().collect())
            .ok_or(MediaEngineError::RoomNotFound(room))
    }

    async fn list_subscribers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        self.with_room(room, |state| state.subscribers.iter().copied().collect())
            .ok_or(MediaEngineError::RoomNotFound(room))
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
    /// YUV420-кадр с равномерной luma/chroma (проверка O1-холста).
    fn yuv_frame(width: u32, height: u32, value: u8) -> Frame {
        Frame::new(width, height, vec![value; (width * height * 3 / 2) as usize])
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

    /// `O1`: холст комнаты = выходной профиль, rects раскладки масштабируются
    /// под него без щелей, а размер фиксируется после первого рендера.
    #[tokio::test]
    async fn composite_canvas_follows_output_profile() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.add_publisher(rid, a, vec![video_track(a)]).await.unwrap();
        e.add_publisher(rid, b, vec![video_track(b)]).await.unwrap();
        // 2 тайла 960×540 в пространстве раскладки 1920×1080.
        let mut assignments = std::collections::BTreeMap::new();
        assignments.insert(a, 0);
        assignments.insert(b, 1);
        let layout = LayoutDescriptor {
            layout_id: "L".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
        e.apply_layout(rid, layout).await.unwrap();
        e.set_frame(rid, 0, yuv_frame(320, 180, 10));
        e.set_frame(rid, 1, yuv_frame(320, 180, 20));

        assert_eq!(e.composite_size(rid), Some(CANVAS_SIZE), "до первой сессии — дефолтный холст");
        assert!(e.ensure_composite_size(rid, 1280, 720), "первая сессия задаёт холст");
        assert_eq!(e.composite_size(rid), Some((1280, 720)));

        e.tick(rid);
        let out = e.composite_frame(rid).expect("composite");
        assert_eq!((out.width, out.height), (1280, 720));
        assert_eq!(out.data.len(), 1280 * 720 * 3 / 2, "YUV-холст профиля");
        // Холст 1280×720: два тайла по 640×720 (2 колонки), кадр 16:9
        // вписывается в 640×360 и центрируется по вертикали → контент y∈[180,540).
        // Стык колонок ровно на x=640, без щели.
        let row = 300 * 1280;
        let (left, right) = (out.data[row + 100], out.data[row + 640 + 100]);
        assert!(
            (left == 10 && right == 20) || (left == 20 && right == 10),
            "оба тайла отрисованы: left={left}, right={right}"
        );
        assert_eq!(out.data[row + 639], left, "левый тайл доходит до стыка");
        assert_eq!(out.data[row + 640], right, "правый тайл начинается на стыке");
        assert_eq!(out.data[10 * 1280 + 100], 16, "letterbox-полоса не закрашена");

        // После первого рендера холст фиксируется: другая сессия с иным
        // профилем получает обычный resize, геометрия на ходу не меняется.
        assert!(!e.ensure_composite_size(rid, 1920, 1080));
        assert_eq!(e.composite_size(rid), Some((1280, 720)));

        // Явный сеттер работает и после рендера, нулевой размер отвергается.
        assert!(e.set_composite_size(rid, 640, 360));
        assert_eq!(e.composite_size(rid), Some((640, 360)));
        assert!(!e.set_composite_size(rid, 0, 0));
        assert!(!e.set_composite_size(RoomId::new(), 640, 360), "нет комнаты — false");
    }

    /// Функциональный блокер 20+: в раскладке из админки 13 окон
    /// (`preset_default_13`), а видео-участников 20 — все обязаны получить слот.
    /// До правки 7–8 человек молча оставались без окна и не попадали в композит.
    #[tokio::test]
    async fn twenty_participants_fit_a_thirteen_window_preset() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let participants: Vec<ParticipantId> = (1..=20).map(p).collect();
        for &pid in &participants {
            e.add_publisher(rid, pid, vec![video_track(pid)]).await.unwrap();
        }
        let canvas = Canvas::new(CANVAS_SIZE.0, CANVAS_SIZE.1);
        let preset_rects = GridLayout::compute(&canvas, 13);
        let geometry: Vec<LayoutSlot> = preset_rects
            .iter()
            .enumerate()
            .map(|(index, rect)| LayoutSlot {
                slot: index as u32,
                x: rect.x,
                y: rect.y,
                width: rect.w,
                height: rect.h,
                z_order: 0,
            })
            .collect();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_default_13".into(),
                version: 1,
                assignments: BTreeMap::new(),
                geometry,
                preset: "preset_default_13".into(),
                mode: "manual".into(),
            },
        )
        .await
        .unwrap();

        let snapshot = e.layout_snapshot(rid).expect("snapshot");
        assert_eq!(snapshot.layout.slots.len(), 20, "сетка расширена под всех участников");
        // `vks_layout::ParticipantId` и `vks_core::ParticipantId` — разные типы
        // с общим UUID, поэтому сверяем по uuid.
        let mut assigned = BTreeSet::new();
        for slot in &snapshot.layout.slots {
            if let Some(pid) = slot.participant_id {
                assert!(assigned.insert(pid.as_u128()), "участник назначен дважды: {pid:?}");
            }
        }
        assert_eq!(assigned.len(), 20, "ни один видео-участник не остался без окна");
        for &pid in &participants {
            assert!(assigned.contains(&pid.as_uuid().as_u128()), "нет слота для {pid:?}");
        }
        for i in 0..snapshot.layout.slots.len() {
            for j in (i + 1)..snapshot.layout.slots.len() {
                assert!(
                    !snapshot.layout.slots[i].rect.intersects(&snapshot.layout.slots[j].rect),
                    "окна {i} и {j} перекрываются"
                );
            }
        }
        assert!(snapshot.preset.is_empty(), "пресет сброшен: геометрия больше не пресетная");

        // Каждый кадр принимается и виден в своей ячейке (значение = 20 + индекс).
        let value_of = |index: usize| 20 + index as u8;
        for (index, &pid) in participants.iter().enumerate() {
            assert!(
                e.set_participant_frame(rid, pid, yuv_frame(320, 180, value_of(index))),
                "кадр участника {pid:?} должен приниматься (слот есть)"
            );
        }
        for _ in 0..12 {
            e.tick(rid);
        }
        let composite = e.composite_frame(rid).expect("composite");
        let cw = CANVAS_SIZE.0 as usize;
        let center_of = |rect: &vks_layout::Rect| {
            (rect.y as usize + rect.h as usize / 2) * cw + rect.x as usize + rect.w as usize / 2
        };
        for slot in &snapshot.layout.slots {
            let pid = slot.participant_id.expect("все слоты заняты");
            let index =
                participants.iter().position(|p| p.as_uuid().as_u128() == pid.as_u128()).expect("участник из списка");
            assert_eq!(
                composite.data[center_of(&slot.rect)],
                value_of(index),
                "в ячейке {pid:?} должно быть его видео"
            );
        }

        // Self-view ON: своя ячейка освобождается, камера уходит в ячейку
        // низшего приоритета (максимальный id = 19).
        let viewer = participants[0];
        e.set_self_view(rid, true);
        e.tick(rid);
        let own_slot = snapshot
            .layout
            .slots
            .iter()
            .find(|slot| slot.participant_id.is_some_and(|pid| pid.as_u128() == viewer.as_uuid().as_u128()))
            .expect("own slot");
        let self_slot = snapshot.layout.slots.iter().max_by_key(|slot| slot.id.as_u32()).expect("self slot");
        let personal = e.frame_for_viewer(rid, viewer).expect("кадр зрителя");
        assert_ne!(personal.data, composite.data, "персональный кадр отличается от общего");
        assert_eq!(personal.data[center_of(&own_slot.rect)], 16, "своя ячейка очищена");
        assert_eq!(personal.data[center_of(&self_slot.rect)], value_of(0), "своя камера в ячейке низшего приоритета");
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
        let layout =
            LayoutDescriptor { layout_id: "custom".into(), version: 1, assignments, geometry, ..Default::default() };
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
                ..Default::default()
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

    /// Codian §2.2: мелкие ячейки заполняются по убыванию недавности речи.
    #[tokio::test]
    async fn thumbnails_are_ordered_by_speech_recency() {
        let e = McuEngine::new();
        let rid = room_id();
        let (a, b, c) = (p(1), p(2), p(3));
        e.create_room(cfg(rid)).await.unwrap();
        for pid in [a, b, c] {
            e.add_publisher(rid, pid, vec![video_track(pid)]).await.unwrap();
        }
        apply_focus_layout(&e, rid, "focus", &[(a, 0), (b, 1), (c, 2)]).await;
        let _ = collect_events(&e);

        // A говорит 0.6 с — главное окно уходит к нему.
        e.set_audio(rid, a, vec![0.9f32; 960]);
        for step in 0..30 {
            e.tick_audio_at(rid, 100 + step * 20);
        }
        assert_eq!(slot_of(&e, rid, a), Some(0), "говорящий — в главном окне");

        // A замолкает, говорит B: главное окно у B, а недавний спикер A —
        // первым в полосе, не говоривший C — последним.
        e.set_audio(rid, a, vec![0.0f32; 960]);
        e.set_audio(rid, b, vec![0.9f32; 960]);
        for step in 30..90 {
            e.tick_audio_at(rid, 100 + step * 20);
        }
        assert_eq!(slot_of(&e, rid, b), Some(0), "главное окно следует за спикером");
        assert_eq!(slot_of(&e, rid, a), Some(1), "недавний спикер — первая мелкая ячейка");
        assert_eq!(slot_of(&e, rid, c), Some(2), "не говоривший — последняя ячейка");
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
                ..Default::default()
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
        let layout = LayoutDescriptor {
            layout_id: "L".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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
        let layout = LayoutDescriptor {
            layout_id: "focus".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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
        let layout = LayoutDescriptor {
            layout_id: "grid".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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
        let layout = LayoutDescriptor {
            layout_id: "grid".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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

    /// `P1`: правило битрейта группы — кламп [base, 1.25×base], visible=0 → base.
    #[test]
    fn group_bitrate_scales_by_visible_windows_and_clamps() {
        assert_eq!(group_bitrate_kbps(2048, 0), 2048, "пустая комната — базовый битрейт");
        assert_eq!(group_bitrate_kbps(2048, 1), 2048, "одно окно — базовый битрейт");
        assert_eq!(group_bitrate_kbps(2048, 2), 2088, "2 окна: floor(base × 1.02)");
        assert_eq!(group_bitrate_kbps(2048, 20), 2560, "20 окон: 1.25 × base (кламп)");
        assert_eq!(group_bitrate_kbps(2048, 100), 2560, "кламп не превышается");
        assert_eq!(group_bitrate_kbps(1000, 20), 1250);
        for visible in 0..64 {
            let bitrate = group_bitrate_kbps(2048, visible);
            assert!(bitrate >= 2048, "битрейт не ниже базового (visible={visible})");
            assert!(bitrate <= 2048 * GROUP_BITRATE_CEILING_PCT / 100, "не выше 1.25×base");
        }
    }

    /// `P1`: видимые окна = занятые ячейки композита (участник с видео посажен
    /// в слот текущей геометрии). Наличие кадра не требуется: «чёрное» или ещё
    /// не декодированное окно — всё равно окно композита; пустые и аудио-только
    /// ячейки не считаются. Это production-семантика: не зависит от мок-источника
    /// кадров (`set_participant_frame` пишет кадры, но раскладка их не ждёт).
    #[tokio::test]
    async fn visible_windows_counts_occupied_composite_cells() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        assert_eq!(e.visible_windows(rid), 0, "пустая комната — 0 окон");
        let a = p(1);
        let b = p(2);
        let audio_only = p(3);
        // Production-путь: `bridge_participant` — ровно то, что зовёт
        // `bridge_gateway_participant_media` для SIP/H.323-участника.
        e.bridge_participant(rid, a, vec![video_track(a)]);
        assert_eq!(e.visible_windows(rid), 1, "видео-участник занимает окно");
        e.bridge_participant(rid, b, vec![video_track(b)]);
        assert_eq!(e.visible_windows(rid), 2, "два видео-участника — два окна");
        e.bridge_participant(rid, audio_only, vec![Track::new(audio_only, TrackKind::Audio)]);
        assert_eq!(e.visible_windows(rid), 2, "аудио-только участник окна не занимает");
        e.unbridge_participant(rid, b);
        assert_eq!(e.visible_windows(rid), 1, "ушёл участник — окно освободилось");
        assert_eq!(e.visible_windows(RoomId::new()), 0, "нет комнаты — 0 окон");
    }

    /// `P1`: канал видимых окон дебаунсится 500 мс и публикуется один раз,
    /// а не на каждый тик рендера (иначе десятки групп от churn зрителей).
    #[tokio::test]
    async fn visible_windows_channel_debounces_and_publishes_once() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.bridge_participant(rid, a, vec![video_track(a)]);
        let rx = e.visible_windows_receiver(rid);
        assert_eq!(*rx.borrow(), 1, "текущее значение — 1 окно");
        e.bridge_participant(rid, b, vec![video_track(b)]);
        assert_eq!(e.refresh_visible_windows(rid), None, "сразу после смены — ждём стабильности");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert_eq!(e.refresh_visible_windows(rid), Some(2), "стабильно ≥500 мс — публикуем");
        assert_eq!(*rx.borrow(), 2);
        assert_eq!(e.refresh_visible_windows(rid), None, "повторный тик не публикует");
    }

    /// `P1`: флаппинг вверх-вниз не публикуется: значение вернулось к текущему —
    /// канал молчит (гистерезис не даёт пере-ключений на каждый вход/выход).
    #[tokio::test]
    async fn visible_windows_channel_ignores_flapping() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let a = p(1);
        let b = p(2);
        e.bridge_participant(rid, a, vec![video_track(a)]);
        let rx = e.visible_windows_receiver(rid);
        e.bridge_participant(rid, b, vec![video_track(b)]);
        assert_eq!(e.refresh_visible_windows(rid), None);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        e.unbridge_participant(rid, b);
        assert_eq!(e.refresh_visible_windows(rid), None, "вернулись к текущему — сброс pending");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert_eq!(e.refresh_visible_windows(rid), None, "флаппинг не публикуется");
        assert_eq!(*rx.borrow(), 1, "канал остался на стабильном значении");
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
            LayoutDescriptor {
                layout_id: "grid".into(),
                version: 1,
                assignments,
                geometry: Vec::new(),
                ..Default::default()
            },
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
            LayoutDescriptor {
                layout_id: "grid".into(),
                version: 2,
                assignments: swapped,
                geometry: Vec::new(),
                ..Default::default()
            },
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
        let layout = LayoutDescriptor {
            layout_id: "grid".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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
        let layout = LayoutDescriptor {
            layout_id: "grid".into(),
            version: 1,
            assignments,
            geometry: Vec::new(),
            ..Default::default()
        };
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
            ..Default::default()
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
                ..Default::default()
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

    /// T10a: авто-сетка, созданная самим движком (без apply из админки),
    /// помечается `mode:"auto"`, preset пуст — иначе клиент не отличил бы её
    /// от legacy-кадра.
    #[tokio::test]
    async fn register_publisher_marks_auto_mode() {
        let e = McuEngine::new();
        let rid = room_id();
        let pid = p(1);
        e.bridge_participant(rid, pid, vec![video_track(pid)]);
        let snapshot = e.layout_snapshot(rid).expect("snapshot after publisher");
        assert_eq!(snapshot.mode, "auto");
        assert_eq!(snapshot.preset, "");
    }

    /// T10a: снимок провода несёт справочные метаданные применённого
    /// дескриптора вместе с эффективной геометрией.
    #[tokio::test]
    async fn layout_snapshot_carries_preset_and_mode() {
        let e = McuEngine::new();
        let rid = room_id();
        e.ensure_room(rid);
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "codian-auto".into(),
                version: 1,
                assignments: BTreeMap::new(),
                geometry: Vec::new(),
                preset: "codian_07".into(),
                mode: "manual".into(),
            },
        )
        .await
        .expect("layout applied");
        let snapshot = e.layout_snapshot(rid).expect("snapshot");
        assert_eq!(snapshot.preset, "codian_07");
        assert_eq!(snapshot.mode, "manual");
        assert_eq!(snapshot.layout.version, 1, "геометрия и метаданные из одного применения");
    }

    /// T10a: pending-раскладка до создания комнаты тоже отдаёт метаданные —
    /// иначе первый зритель получил бы снимок без режима.
    #[tokio::test]
    async fn layout_snapshot_for_pending_room_preserves_metadata() {
        let e = McuEngine::new();
        let rid = room_id();
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "pending".into(),
                version: 4,
                assignments: BTreeMap::new(),
                geometry: Vec::new(),
                preset: "vks_grid_9".into(),
                mode: "manual".into(),
            },
        )
        .await
        .expect("pending layout staged");
        let snapshot = e.layout_snapshot(rid).expect("pending snapshot");
        assert_eq!(snapshot.preset, "vks_grid_9");
        assert_eq!(snapshot.mode, "manual");
        assert_eq!(snapshot.layout.version, 4);
    }

    /// T12: pending-раскладка, пришедшая до создания комнаты, подхватывается
    /// `create_room` (а не только первым мостом): первый зритель не видит
    /// пустую сетку, версия/геометрия сохранены, pending очищен.
    #[tokio::test]
    async fn create_room_adopts_pending_layout() {
        use vks_core::ports::media_engine::LayoutSlot;

        let e = McuEngine::new();
        let rid = room_id();
        let geometry = vec![LayoutSlot { slot: 0, x: 0, y: 0, width: 1280, height: 720, z_order: 0 }];
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_equal_1".into(),
                version: 7,
                assignments: Default::default(),
                geometry,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(e.slot_rects(rid).is_none(), "комнаты ещё нет");
        assert!(e.layout_descriptor(rid).is_some(), "pending сохранён");

        e.create_room(cfg(rid)).await.unwrap();
        let adopted = e.layout_descriptor(rid).expect("pending применён в create_room");
        assert_eq!(adopted.layout_id, "preset_equal_1", "id раскладки сохранён");
        assert_eq!(adopted.version, 7, "версия сохранена");
        assert_eq!(adopted.geometry.len(), 1, "геометрия сохранена");
        assert_eq!(e.slot_rects(rid).map(|rects| rects.len()), Some(1), "окно доступно сразу");

        e.destroy_room(rid).await.unwrap();
        assert!(e.layout_descriptor(rid).is_none(), "pending не воскресает после destroy");
    }

    /// T03: pending-раскладка не создаёт комнату, но применяется на первом
    /// bridge. Поведение изменено (блокер 20+): если geometry-ячеек меньше, чем
    /// видео-участников, раскладка авто-расширяется равной сеткой, а не
    /// оставляет лишних без слота (раньше пятый паблишер получал full-canvas
    /// или вообще терял кадры).
    #[tokio::test]
    async fn pending_layout_applies_on_first_bridge_and_expands_for_extra_publishers() {
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
                ..Default::default()
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
        assert_eq!(rects.len(), 5, "geometry расширена сеткой под всех видео-участников");
        let layout = e.layout_descriptor(rid).expect("layout");
        assert_eq!(layout.assignments.len(), 5, "каждый видео-участник получил слот");
        assert_eq!(layout.assignments.values().copied().collect::<BTreeSet<_>>().len(), 5, "номера слотов уникальны");
        assert!(layout.preset.is_empty(), "после авто-расширения геометрия не пресетная");
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
                ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
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
        e.apply_layout(
            rid,
            LayoutDescriptor {
                layout_id: "preset_equal_2".into(),
                version: 1,
                assignments,
                geometry,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let second = p(11);
        e.bridge_participant(rid, second, vec![video_track(second)]);
        let layout = e.layout_descriptor(rid).expect("layout");
        assert_eq!(layout.assignments[&second], 1, "свободный geometry-слот 1");
    }

    /// T12/P0-3: canonical `pending_layouts → rooms → frames → audio` order.
    ///
    /// Before the fix `seat_publisher`/`apply_layout` took the two first locks
    /// in opposite orders, so a gateway landing concurrent with a layout apply
    /// could deadlock. The stress test runs both sides with a hard timeout: a
    /// deadlock fails the test instead of hanging the suite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lock_order_stress_landing_vs_layout_and_remove() {
        use std::sync::Arc;

        for iteration in 0..200u64 {
            let e = Arc::new(McuEngine::new());
            let rid = room_id();
            e.create_room(cfg(rid)).await.unwrap();
            let pid_layout = p(1000 + iteration * 2);
            let pid_bridge = p(1001 + iteration * 2);

            let start = Arc::new(std::sync::Barrier::new(3));
            let eng_a = e.clone();
            let eng_b = e.clone();
            let eng_c = e.clone();
            let b1 = start.clone();
            let b2 = start.clone();
            let b3 = start.clone();
            let a = std::thread::spawn(move || {
                b1.wait();
                eng_a.seat_publisher(rid, pid_layout, vec![video_track(pid_layout)]);
            });
            let b = std::thread::spawn(move || {
                b2.wait();
                eng_b.bridge_participant(rid, pid_bridge, vec![Track::new(pid_bridge, TrackKind::Audio)]);
            });
            let c = std::thread::spawn(move || {
                b3.wait();
                let handle = eng_c.clone();
                let task = handle.apply_layout(
                    rid,
                    LayoutDescriptor {
                        layout_id: format!("stress_{iteration}"),
                        version: 1,
                        assignments: Default::default(),
                        geometry: Vec::new(),
                        ..Default::default()
                    },
                );
                // A current-thread runtime would be required for `block_on`;
                // instead poll the future to completion on a fresh runtime.
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime")
                    .block_on(task)
                    .expect("apply_layout");
                eng_c.unbridge_participant(rid, pid_layout);
            });

            let joined = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                a.join().expect("thread a");
                b.join().expect("thread b");
                c.join().expect("thread c");
            })
            .await;
            assert!(joined.is_ok(), "iteration {iteration}: lock-order deadlock (2 s timeout)");
        }
    }

    /// T12: the bridge egress mix contains only the listed participants (the
    /// gateways), never the browser that receives it — that is what keeps the
    /// SFU browser↔browser fanout from being double-delivered.
    #[tokio::test]
    async fn mix_participants_audio_is_scoped_to_the_listed_sources() {
        let e = McuEngine::new();
        let rid = room_id();
        let browser = p(30);
        let gateway = p(31);
        let second_gateway = p(32);
        e.create_room(cfg(rid)).await.unwrap();
        for pid in [browser, gateway, second_gateway] {
            e.bridge_participant(rid, pid, vec![Track::new(pid, TrackKind::Audio)]);
        }
        e.set_audio(rid, browser, vec![0.8; 960]);
        e.set_audio(rid, gateway, vec![0.4; 960]);
        e.set_audio(rid, second_gateway, vec![0.2; 960]);

        let gateway_only = e.mix_participants_audio(rid, &[gateway, second_gateway]);
        assert_eq!(gateway_only.len(), 960);
        // gain 1/√2 over (0.4 + 0.2): the browser's 0.8 must not appear.
        let expected = (0.4 + 0.2) / 2.0f32.sqrt();
        assert!(
            (gateway_only[0] - expected).abs() < 1e-5,
            "gateway mix must exclude the browser: got {} expected {expected}",
            gateway_only[0]
        );
        assert!(gateway_only[0] < 0.6, "browser echo leaked into the bridge mix");
        // No listed source → silence, never the full room mix.
        assert!(e.mix_participants_audio(rid, &[]).iter().all(|s| *s == 0.0));
        // Unknown room → silence, not a panic.
        assert!(e.mix_participants_audio(RoomId::new(), &[gateway]).iter().all(|s| *s == 0.0));
    }

    /// T12: accepted audio ticks are counted so the mixed-room e2e can prove
    /// the bridge clock replaced (not duplicated) the gateway tick.
    #[tokio::test]
    async fn audio_tick_counter_advances_once_per_accepted_tick() {
        let e = McuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let start = e.audio_ticks_total();
        assert!(e.tick_audio_at(rid, 100), "first tick");
        assert!(!e.tick_audio_at(rid, 110), "the 18 ms guard rejects the duplicate");
        assert!(e.tick_audio_at(rid, 120), "next 20 ms tick");
        assert_eq!(e.audio_ticks_total(), start + 2, "only accepted ticks are counted");
    }

    /// T12: while the bridge owns a room's tick, every other caller's
    /// `tick_audio_at` is a no-op; only the bridge-privileged entry point
    /// advances the mix. This is what makes "one ticker per room" a property of
    /// the engine, not a convention every call site must remember.
    #[tokio::test]
    async fn bridge_tick_ownership_blocks_other_tickers_and_is_released() {
        let e = McuEngine::new();
        let rid = room_id();
        let other = RoomId::new();
        e.create_room(cfg(rid)).await.unwrap();
        e.create_room(cfg(other)).await.unwrap();
        assert!(!e.bridge_owns_tick(rid));
        let start = e.audio_ticks_total();

        e.set_bridge_tick_owner(rid, true);
        assert!(e.bridge_owns_tick(rid), "ownership is visible to the gateway loop");
        assert!(!e.tick_audio_at(rid, 100), "a gateway/AAC tick must be rejected while the bridge owns the room");
        assert!(e.tick_audio_at(other, 100), "other rooms are unaffected");
        assert!(e.tick_audio_at_bridge(rid, 100), "the bridge itself still ticks");
        assert!(!e.tick_audio_at_bridge(rid, 110), "the bridge obeys the 18 ms guard too");
        assert!(e.tick_audio_at_bridge(rid, 120));
        assert_eq!(e.audio_ticks_total(), start + 3, "2 bridge ticks + 1 other room");

        e.set_bridge_tick_owner(rid, false);
        assert!(!e.bridge_owns_tick(rid));
        assert!(e.tick_audio_at(rid, 200), "after release the gateway loop ticks again");
        assert!(!e.tick_audio_at(rid, 205), "and the 18 ms guard still applies");
        // Idempotent release.
        e.set_bridge_tick_owner(rid, false);
        assert!(!e.bridge_owns_tick(rid));
    }

    #[tokio::test]
    async fn layout_720p_basis_does_not_shrink_to_853x480_on_720p_canvas() {
        let e = McuEngine::new();
        let rid = room_id();
        let p = p(1);
        e.create_room(cfg(rid)).await.unwrap();
        e.bridge_participant(rid, p, vec![vks_core::track::Track::new(p, vks_core::track::TrackKind::Video)]);
        // Resize canvas to 720p (Codian MCU / SIP resolution)
        e.ensure_composite_size(rid, 1280, 720);

        let mut assignments = BTreeMap::new();
        assignments.insert(p, 0);
        let desc = LayoutDescriptor {
            version: 1,
            layout_id: "custom".into(),
            assignments,
            geometry: vec![vks_core::ports::media_engine::LayoutSlot {
                slot: 0,
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
                z_order: 0,
            }],
            ..Default::default()
        };
        e.apply_layout(rid, desc).await.unwrap();
        for _ in 0..12 {
            e.tick(rid);
        }

        // Check scaled rects
        let rects = e.with_room(rid, |s| s.scaled_slot_rects()).unwrap();
        let slot0 = rects.get(&0).expect("slot 0 must exist");
        assert_eq!(
            (slot0.x, slot0.y, slot0.w, slot0.h),
            (0, 0, 1280, 720),
            "720p geometry on 720p canvas must be 1:1 (1280x720), not shrunk to 853x480"
        );
    }
}

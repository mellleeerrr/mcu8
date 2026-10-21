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
    composite: Frame,
    /// Audio mix of the current frame (full mix for recording/monitoring).
    mixed_audio: Vec<f32>,
    /// Per-participant N-1 mix-minus audio (without self-echo).
    participant_audio: BTreeMap<ParticipantId, Vec<f32>>,
    /// Per-participant VAD instances (independent debounce and energy tracking).
    vads: BTreeMap<ParticipantId, Vad>,
    /// Last committed speaking state per participant.
    speaking_states: BTreeMap<ParticipantId, bool>,
    /// Current active/dominant speaker with silence hysteresis.
    active_speaker: Option<ParticipantId>,
}

impl RoomState {
    fn new(mode: RoomMode, caps: RoomCaps) -> Self {
        Self {
            mode,
            caps,
            publisher_tracks: BTreeMap::new(),
            subscribers: BTreeSet::new(),
            layout: None,
            current_layout: Layout::new(LayoutKind::Grid),
            bridge: LayoutBridge::new(),
            elapsed_ms: 0,
            layout_applied_at_ms: None,
            last_render_ms: None,
            last_audio_ms: None,
            stats: McuRenderStats::default(),
            composite: Frame::new(
                CANVAS_SIZE.0,
                CANVAS_SIZE.1,
                vec![0u8; CANVAS_SIZE.0 as usize * CANVAS_SIZE.1 as usize],
            ),
            mixed_audio: Vec::new(),
            participant_audio: BTreeMap::new(),
            vads: BTreeMap::new(),
            speaking_states: BTreeMap::new(),
            active_speaker: None,
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
            for (index, cell) in cells.iter().enumerate() {
                let x = cell.x.min(canvas.width.saturating_sub(1));
                let y = cell.y.min(canvas.height.saturating_sub(1));
                let width = cell.width.max(1).min(canvas.width.saturating_sub(x));
                let height = cell.height.max(1).min(canvas.height.saturating_sub(y));
                let pid = slot_to_pid.get(&cell.slot).copied();
                layout.slots.push(vks_layout::Slot::with_attributes(
                    cell.slot,
                    pid,
                    vks_layout::Rect::new(x, y, width, height),
                    cell.z_order as i32 + index as i32,
                    false,
                    false,
                ));
            }
            // Участник, которого нет в geometry (добавлен позже), получает
            // свободную ячейку в правом нижнем углу, а не пропадает.
            for (&slot_id, &vpid) in &slot_to_pid {
                if !cells.iter().any(|cell| cell.slot == slot_id) {
                    layout.slots.push(vks_layout::Slot::new(slot_id, Some(vpid), canvas.as_rect()));
                }
            }
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
            LayoutKind::Focus | LayoutKind::Speaker if total_slots > 1 => {
                let large_h = canvas.height * 75 / 100;
                let strip_h = canvas.height - large_h;
                let m = (total_slots - 1) as u32;

                let mut r = Vec::with_capacity(total_slots);
                // Slot 0: large presenter/focus tile
                r.push(vks_layout::Rect::new(0, 0, canvas.width, large_h));
                // Slots 1..total_slots: bottom thumbnail strip
                for k in 0..m {
                    let x = canvas.width * k / m;
                    let w = canvas.width / m;
                    r.push(vks_layout::Rect::new(x, large_h, w, strip_h));
                }
                r
            }
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

    /// Recomputes the composite + mixed audio + per-participant mix-minus + VAD from the current
    /// frames and participant audio (deterministic logical step of 33ms).
    fn tick(&mut self, frames: &BTreeMap<u32, Frame>, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
        self.elapsed_ms += 33;
        self.process_media(frames, audio_inputs);
    }

    /// Advances the pipeline at a specific monotonic timestamp `now_ms`.
    /// Audio runs on a 20ms cadence, and video applies ~30 fps frame-dropping backpressure.
    fn tick_at(
        &mut self,
        now_ms: u64,
        frames: &BTreeMap<u32, Frame>,
        audio_inputs: &[(ParticipantId, Vec<f32>)],
    ) -> bool {
        self.tick_audio_at(now_ms, audio_inputs);
        self.tick_video_at(now_ms, frames)
    }

    /// Advances the audio processing pass at a specific monotonic timestamp `now_ms` (20 ms cadence).
    fn tick_audio_at(&mut self, now_ms: u64, audio_inputs: &[(ParticipantId, Vec<f32>)]) -> bool {
        if let Some(last) = self.last_audio_ms {
            if now_ms < last + 18 {
                return false;
            }
        }
        self.last_audio_ms = Some(now_ms);
        self.process_audio(audio_inputs);
        true
    }

    /// Advances the video compositing pass at a specific monotonic timestamp `now_ms` (33 ms budget).
    fn tick_video_at(&mut self, now_ms: u64, frames: &BTreeMap<u32, Frame>) -> bool {
        if let Some(last) = self.last_render_ms {
            if now_ms < last + 30 {
                self.stats.frames_dropped += 1;
                return false;
            }
        }
        self.last_render_ms = Some(now_ms);
        let applied_at = *self.layout_applied_at_ms.get_or_insert(now_ms);
        self.elapsed_ms = now_ms.saturating_sub(applied_at);
        self.process_video(frames);
        true
    }

    fn process_video(&mut self, frames: &BTreeMap<u32, Frame>) {
        let t0 = std::time::Instant::now();
        let rects = self.slot_rects();
        self.composite = Compositor.compose(frames, &rects).into_frame();
        self.stats.last_duration_us = t0.elapsed().as_micros() as u64;
        self.stats.frames_rendered += 1;
    }

    fn process_audio(&mut self, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
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
            self.speaking_states.insert(*pid, is_speaking);

            if is_speaking && sum_sq > highest_energy {
                highest_energy = sum_sq;
                current_speaker = Some(*pid);
            }
        }

        // Active speaker with silence hysteresis (keep previous speaker if nobody is currently active)
        if let Some(spk) = current_speaker {
            self.active_speaker = Some(spk);
        }
    }

    fn process_media(&mut self, frames: &BTreeMap<u32, Frame>, audio_inputs: &[(ParticipantId, Vec<f32>)]) {
        self.process_video(frames);
        self.process_audio(audio_inputs);
    }
}

/// MCU engine (impl [`MediaEngine`]).
/// Room → (slot → frame): pluggable mock frame source.
type FrameSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<u32, Frame>>>>;
/// Room → (participant → 20 ms PCM frame): pluggable mock audio source.
type AudioSource = Arc<Mutex<BTreeMap<RoomId, BTreeMap<ParticipantId, Vec<f32>>>>>;

#[derive(Debug, Default)]
pub struct McuEngine {
    rooms: Arc<Mutex<BTreeMap<RoomId, RoomState>>>,
    /// Pluggable frame source (mock by default).
    frames: FrameSource,
    /// Pluggable audio source (mock).
    audio: AudioSource,
}

impl McuEngine {
    /// Creates an empty MCU engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Injects a (mock) frame for a slot of a room (test helper).
    pub fn set_frame(&self, room: RoomId, slot: u32, frame: Frame) {
        self.frames.lock().unwrap().entry(room).or_default().insert(slot, frame);
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
    pub fn tick(&self, room: RoomId) -> Option<Frame> {
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
    pub fn tick_at(&self, room: RoomId, now_ms: u64) -> Option<Frame> {
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
    /// Returns true if an audio tick was executed, or false if throttled within the 20 ms budget.
    pub fn tick_audio_at(&self, room: RoomId, now_ms: u64) -> bool {
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
    }

    /// Advances only the video pipeline at a specific monotonic timestamp `now_ms` (33 ms budget).
    /// Returns Some(Frame) if rendered, or None if dropped by frame-dropping backpressure.
    pub fn tick_video_at(&self, room: RoomId, now_ms: u64) -> Option<Frame> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room)?;
        let frames = self.frames.lock().unwrap().get(&room).cloned().unwrap_or_default();
        if state.tick_video_at(now_ms, &frames) {
            Some(state.composite.clone())
        } else {
            None
        }
    }

    /// Returns the rendering and backpressure stats of a room (M3.6).
    pub fn stats(&self, room: RoomId) -> Option<McuRenderStats> {
        let g = self.rooms.lock().unwrap();
        g.get(&room).map(|s| s.stats)
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
    pub fn active_speaker(&self, room: RoomId) -> Option<ParticipantId> {
        self.rooms.lock().unwrap().get(&room).and_then(|s| s.active_speaker)
    }

    /// Returns whether the given participant is currently speaking.
    pub fn is_speaking(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.rooms.lock().unwrap().get(&room).and_then(|s| s.speaking_states.get(&pid).copied()).unwrap_or(false)
    }

    /// Ensures a room exists in the MCU engine.
    pub fn ensure_room(&self, room: RoomId) {
        let mut g = self.rooms.lock().unwrap();
        g.entry(room).or_insert_with(|| RoomState::new(RoomMode::Mcu, RoomCaps::default()));
    }

    /// Synchronously bridges a participant into the MCU room as both subscriber and publisher.
    pub fn bridge_participant(&self, room: RoomId, pid: ParticipantId, tracks: Vec<MediaTrack>) {
        let mut g = self.rooms.lock().unwrap();
        let state = g.entry(room).or_insert_with(|| RoomState::new(RoomMode::Mcu, RoomCaps::default()));
        state.subscribers.insert(pid);
        if !tracks.is_empty() {
            let has_video = tracks.iter().any(|track| {
                matches!(track.kind, vks_core::track::TrackKind::Video | vks_core::track::TrackKind::Screen)
            });
            state.publisher_tracks.insert(pid, tracks);
            if has_video {
                let descriptor = state.layout.get_or_insert_with(|| LayoutDescriptor {
                    layout_id: "grid".into(),
                    version: 0,
                    assignments: BTreeMap::new(),
                    geometry: Vec::new(),
                });
                if !descriptor.assignments.contains_key(&pid) {
                    let slot = (0..u32::MAX)
                        .find(|slot| !descriptor.assignments.values().any(|existing| existing == slot))
                        .unwrap_or(0);
                    descriptor.assignments.insert(pid, slot);
                    descriptor.version += 1;
                    let layout = RoomState::layout_from_descriptor(descriptor);
                    state.bridge.apply(&layout);
                    state.current_layout = layout;
                    state.layout_applied_at_ms = None;
                    state.elapsed_ms = 0;
                }
            }
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
        self.frames.lock().unwrap().entry(room).or_default().insert(*slot, frame);
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
    pub fn composite_frame(&self, room: RoomId) -> Option<Frame> {
        self.rooms.lock().unwrap().get(&room).map(|state| state.composite.clone())
    }

    /// Synchronously removes a bridged participant from the MCU room, purging layout and audio.
    pub fn unbridge_participant(&self, room: RoomId, pid: ParticipantId) {
        let mut g = self.rooms.lock().unwrap();
        if let Some(state) = g.get_mut(&room) {
            state.subscribers.remove(&pid);
            state.publisher_tracks.remove(&pid);
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

    /// Returns whether the participant is registered as a publisher in the MCU room.
    pub fn has_publisher(&self, room: RoomId, pid: ParticipantId) -> bool {
        self.rooms.lock().unwrap().get(&room).map(|s| s.publisher_tracks.contains_key(&pid)).unwrap_or(false)
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
        let mut g = self.rooms.lock().unwrap();
        if g.remove(&room).is_none() {
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
        Ok(PublisherHandle { room, publisher })
    }

    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.publisher_tracks.remove(&publisher);
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
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        // Уже забридженные видео-паблишеры не должны потерять ячейку: раскладка
        // может прийти вообще без привязок («Очистить все видеоокна» → все окна
        // AUTO, auto-пресет, возврат к «Общей» после удаления ручной). Иначе
        // `set_participant_frame` перестал бы писать кадры и живой SIP-терминал
        // увидел бы замерший последний снимок вместо новой сетки.
        let bridged: Vec<ParticipantId> = state
            .publisher_tracks
            .iter()
            .filter(|(_, tracks)| {
                tracks.iter().any(|track| {
                    matches!(track.kind, vks_core::track::TrackKind::Video | vks_core::track::TrackKind::Screen)
                })
            })
            .map(|(pid, _)| *pid)
            .collect();
        let mut repaired = false;
        for pid in bridged {
            if layout.assignments.contains_key(&pid) {
                continue;
            }
            let slot =
                (0..u32::MAX).find(|slot| !layout.assignments.values().any(|existing| existing == slot)).unwrap_or(0);
            layout.assignments.insert(pid, slot);
            repaired = true;
        }
        if repaired {
            // Раскладка изменилась на глазах клиентов и шлюза — новая версия.
            layout.version = layout.version.saturating_add(1);
        }
        let new_layout = RoomState::layout_from_descriptor(&layout);
        state.layout = Some(layout);
        // Begin the crossfade from the previous layout's rects to the new ones.
        state.bridge.apply(&new_layout);
        state.current_layout = new_layout;
        state.elapsed_ms = 0;
        state.layout_applied_at_ms = None;
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
        assert!(matches!(
            e.apply_layout(missing, LayoutDescriptor::default()).await.unwrap_err(),
            MediaEngineError::RoomNotFound(_)
        ));
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
        let composite = e.tick(rid).expect("composite frame");
        assert_eq!(composite.data[270 * CANVAS_SIZE.0 as usize + 480], 99, "новый кадр виден в верхней левой ячейке");
    }
}

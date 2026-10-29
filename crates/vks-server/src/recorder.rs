//! Интеграция подсистемы записи с сервером VKS и RoomHub (Milestone M5).
//!
//! Управляет жизненным циклом сессий записи, регистрирует рекордер как
//! специального участника комнаты (is_recorder: true) в RoomHub,
//! фиксирует метаданные в хранилище и уведомляет админ-панель.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::task::AbortHandle;
use tracing::{info, warn};
use uuid::Uuid;

use vks_admin::hub::ParticipantSnapshot;
use vks_admin::state::RecordingControl;
use vks_admin::state::{now_unix_ms, AppState, RecordEntry};
use vks_core::RoomId;
use vks_media_mcu::farm::{FarmCodec, FarmTap};
use vks_media_mcu::EncoderSpec;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_recorder::{
    ContainerConfig, MemoryStore, RecorderProfile, RecordingError, RecordingMeta, RecordingMode, RecordingSession,
    RecordingStore, VideoCodec,
};
use vks_signaling::Envelope;

use crate::recorder_tap;

/// Server-side implementation of the admin recording port.
pub struct RecorderControl {
    recorder: RecorderManager,
    state: std::sync::Weak<AppState>,
    mcu_engine: Arc<vks_media_mcu::McuEngine>,
}

impl RecorderControl {
    pub fn new(
        recorder: RecorderManager,
        state: std::sync::Weak<AppState>,
        mcu_engine: Arc<vks_media_mcu::McuEngine>,
    ) -> Self {
        Self { recorder, state, mcu_engine }
    }
}

#[async_trait::async_trait]
impl RecordingControl for RecorderControl {
    async fn start(&self, room_id: RoomId) -> Result<String, String> {
        let state = self.state.upgrade().ok_or_else(|| "server state is gone".to_string())?;
        // Idempotent retry: the running session already owns its tap.
        if self.recorder.is_recording(&room_id) {
            return self
                .recorder
                .start_recording(&state, &room_id, RecordingMode::Mcu)
                .map(|record_id| record_id.to_string())
                .map_err(|error| error.to_string());
        }
        // A recording that was active when the process died leaves an
        // unfinalized row (duration_sec == 0). Starting again must reuse the
        // room's recording slot, not accumulate duplicate active rows.
        if let Some(storage) = state.storage.clone() {
            if let Ok(records) = storage.list_records(Some(room_id)).await {
                for stale in records.iter().filter(|entry| entry.duration_sec == 0) {
                    let _ = storage.delete_record(&stale.id).await;
                    state.records.lock().unwrap().remove(&stale.id);
                }
            }
        }
        // Tap = subscribe to an EXISTING encoder group: never `acquire`/
        // `viewer`, никогда не спавним скрытый FFmpeg. Нет живой H.264-группы
        // со зрителями — записывать нечего (admin уже отдаёт 409; здесь
        // закрываем гонку между 409-проверкой и стартом).
        let tap = self.mcu_engine.video_farm().subscribe(room_id, FarmCodec::H264).ok_or_else(|| {
            format!("no_active_h264_encoder_group: no live H.264 encoder group with viewers for room {room_id}")
        })?;
        let record_id = self
            .recorder
            .start_recording_with_tap(&state, &room_id, RecordingMode::Mcu, tap)
            .map_err(|error| error.to_string())?;
        // Persist synchronously: "started" must mean storage has the row.
        if let Some(storage) = state.storage.clone() {
            let entry = { state.records.lock().unwrap().get(&record_id.to_string()).cloned() };
            if let Some(entry) = entry {
                storage.upsert_record(&entry).await.map_err(|error| format!("record persistence failed: {error}"))?;
            }
        }
        Ok(record_id.to_string())
    }

    async fn stop(&self, room_id: RoomId) -> Result<(), String> {
        let state = self.state.upgrade().ok_or_else(|| "server state is gone".to_string())?;
        if !self.recorder.is_recording(&room_id) {
            // After a restart the in-memory session is gone but the DB may hold
            // an unfinalized row; finalize it so DELETE actually stops it.
            if let Some(storage) = state.storage.clone() {
                let records = storage
                    .list_records(Some(room_id))
                    .await
                    .map_err(|error| format!("recording metadata read failed: {error}"))?;
                {
                    for mut entry in records.into_iter().filter(|entry| entry.duration_sec == 0) {
                        let now_ms = vks_admin::state::now_unix_ms();
                        entry.duration_sec = (((now_ms.saturating_sub(entry.created_ms)) / 1000).max(1)) as u32;
                        storage
                            .upsert_record(&entry)
                            .await
                            .map_err(|error| format!("recording metadata persistence failed: {error}"))?;
                        state.records.lock().unwrap().insert(entry.id.clone(), entry);
                    }
                }
            }
            return Ok(());
        }
        self.recorder.stop_recording_async(&state, &room_id).await.map(|_| ()).map_err(|error| error.to_string())
    }

    fn active_h264_groups(&self, room_id: RoomId) -> usize {
        // Same predicate as `subscribe` (room + H.264 + current canvas + live
        // + viewers > 0), so the 409 gate cannot pass on a stale/cancelled group.
        self.mcu_engine.video_farm().active_group_count(room_id, FarmCodec::H264)
    }
}

/// Менеджер сессий записи комнат.
#[derive(Clone)]
pub struct RecorderManager {
    sessions: Arc<Mutex<HashMap<RoomId, RecordingSession>>>,
    storage: Arc<Mutex<MemoryStore>>,
    stopped_cache: Arc<Mutex<HashMap<RoomId, RecordingMeta>>>,
    containers: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Abort-хэндлы tap-тасков по комнатам (non-owning подписки на группы).
    taps: Arc<Mutex<HashMap<RoomId, AbortHandle>>>,
}

impl Default for RecorderManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RecorderManager {
    /// Создаёт новый менеджер записей.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            storage: Arc::new(Mutex::new(MemoryStore::new())),
            stopped_cache: Arc::new(Mutex::new(HashMap::new())),
            containers: Arc::new(Mutex::new(HashMap::new())),
            taps: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Проверяет, идёт ли сейчас запись в комнате.
    pub fn is_recording(&self, room_id: &RoomId) -> bool {
        let guard = self.sessions.lock().unwrap();
        guard.contains_key(room_id)
    }

    /// Сохраняет готовый медиаконтейнер записи в хранилище артефактов.
    pub fn save_container(&self, record_id: &str, data: Vec<u8>) {
        self.containers.lock().unwrap().insert(record_id.to_string(), data);
    }

    /// Извлекает медиаконтейнер записи по ID.
    pub fn get_container(&self, record_id: &str) -> Option<Vec<u8>> {
        self.containers.lock().unwrap().get(record_id).cloned()
    }

    /// Начинает запись комнаты в заданном режиме (MCU-композит или SFU multi-track).
    pub fn start_recording(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
        mode: RecordingMode,
    ) -> Result<Uuid, RecordingError> {
        self.start_recording_inner(state, room_id, mode, None)
    }

    /// Начинает запись и подписывает её на **существующую** группу фермы.
    ///
    /// `tap` уже получен через [`EncoderFarm::subscribe`]; здесь только
    /// включается MP4-контейнер с фактическим `EncoderSpec` группы и
    /// запускается tap-таск. Никакого `acquire`/refcount/spawn.
    pub fn start_recording_with_tap(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
        mode: RecordingMode,
        tap: FarmTap,
    ) -> Result<Uuid, RecordingError> {
        self.start_recording_inner(state, room_id, mode, Some(tap))
    }

    fn start_recording_inner(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
        mode: RecordingMode,
        tap: Option<FarmTap>,
    ) -> Result<Uuid, RecordingError> {
        let mut guard = self.sessions.lock().unwrap();
        if let Some(existing) = guard.get(room_id) {
            // Idempotent retry: return the id of the recording already running.
            return Ok(existing.record_id().unwrap_or(*room_id.as_uuid()));
        }

        let room_uuid = *room_id.as_uuid();
        let mut session = RecordingSession::new(room_uuid, Box::new(now_unix_ms));
        session.start(mode)?;
        if let Some(tap) = tap.as_ref() {
            session.enable_container(container_config_for(&tap.key.spec));
            session
                .set_video_parameter_sets(&tap.parameter_sets)
                .map_err(|error| RecordingError::Container(error.to_string()))?;
        }

        let rec_participant_id = format!("recorder-{room_id}");
        let now_sec = now_unix_ms() / 1000;

        let snapshot = ParticipantSnapshot {
            participant_id: rec_participant_id.clone(),
            display_name: "Conference Recorder".to_string(),
            is_recorder: true,
            is_speaking: false,
            track_ids: Vec::new(),
            is_audio_muted: true,
            is_video_muted: true,
            connected_at: now_sec,
        };

        // 1. Регистрация рекордера в RoomHub
        state.room_hub.register_participant(room_id, snapshot);

        // 2. Рассылка ParticipantJoined WebRTC-клиентам комнаты
        let joined_msg = SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::ParticipantJoined(ParticipantJoined {
                participant_id: rec_participant_id.clone(),
                display_name: "Conference Recorder".to_string(),
                is_recorder: true,
            })),
        };
        let payload_bytes = Envelope::new(joined_msg).encode();
        state.room_hub.broadcast(room_id, None, payload_bytes.into());

        // 3. The record row is kept in `AppState.records`; the async caller
        // (`RecorderControl::start`) awaits the storage upsert so a successful
        // POST is immediately visible to storage-backed GETs, and a failed
        // write is returned instead of being swallowed by a detached task.
        let record_id = session.record_id().ok_or(RecordingError::NotStarted)?;
        let profile = RecorderProfile::default();
        // Фактический EncoderSpec группы, а не `RecorderProfile::default()`:
        // UI/API не должны врать про 1280x720, если группа кодирует 640x360.
        let (width, height, bitrate_kbps) = tap.as_ref().map_or_else(
            || (profile.width, profile.height, profile.bitrate_kbps()),
            |tap| (tap.key.spec.width, tap.key.spec.height, tap.key.spec.bitrate_kbps),
        );
        let entry = RecordEntry {
            id: record_id.to_string(),
            room_id: *room_id,
            created_ms: now_unix_ms(),
            duration_sec: 0,
            size_bytes: 0,
            bitrate_kbps,
            width,
            height,
        };
        state.records.lock().unwrap().insert(entry.id.clone(), entry);

        // 4. Сохраняем сессию и запускаем tap уже после вставки в map.
        guard.insert(*room_id, session);
        self.stopped_cache.lock().unwrap().remove(room_id);

        if let Some(tap) = tap {
            let sessions = Arc::clone(&self.sessions);
            let tap_room = *room_id;
            let handle = tokio::spawn(recorder_tap::run(tap_room, tap, sessions));
            self.taps.lock().unwrap().insert(*room_id, handle.abort_handle());
        }

        // 4. Оповещение административного WebSocket
        let room_str = room_id.to_string();
        state.notify_admin(
            &format!("session:{room_str}:abonents"),
            serde_json::json!({
                "action": "joined",
                "room_id": room_str,
                "participant_id": rec_participant_id,
                "is_recorder": true
            }),
        );
        state.notify_admin(
            "sessions:updated",
            serde_json::json!({
                "id": room_str,
                "recording": true
            }),
        );

        info!(room_id = %room_id, mode = %mode.as_str(), "Recording started for room");
        Ok(record_id)
    }

    fn stop_recording_internal(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
        spawn_bg_persistence: bool,
    ) -> Result<StopOutcome, RecordingError> {
        let mut session = {
            let mut guard = self.sessions.lock().unwrap();
            match guard.remove(room_id) {
                Some(session) => session,
                None => {
                    // Проверяем кэш недавно остановленных сессий (идемпотентность)
                    if let Some(meta) = self.stopped_cache.lock().unwrap().get(room_id).cloned() {
                        return Ok(StopOutcome { meta, entry: None, finalize_error: None });
                    }
                    return Err(RecordingError::NotStarted);
                }
            }
        };

        // Останавливаем tap до финализации: он не должен дописывать сэмплы,
        // пока writer собирает moov.
        if let Some(handle) = self.taps.lock().unwrap().remove(room_id) {
            handle.abort();
        }

        let record_id = session.record_id();
        // `stop` больше не глотает ошибку finalize: она вернётся наружу, но
        // метаданные уже закэшированы внутри сессии — запись/участник будут
        // закрыты ниже в любом случае (P1: не терять RoomHub-участника).
        let stop_result = session.stop();
        if let Some(bytes) = session.container_bytes() {
            if let Some(record_id) = record_id {
                self.save_container(&record_id.to_string(), bytes.to_vec());
            }
        }

        // RoomHub/WS/уведомления снимаем ДО возможного early-return по ошибке.
        self.detach_room_participant(state, room_id);

        let meta = match &stop_result {
            Ok(meta) => Some(meta.clone()),
            Err(_) => session.finalized_meta().cloned(),
        };
        let Some(meta) = meta else {
            // Теоретически недостижимо (сессия стартовала), но не оставляем
            // «активную» строку/участника даже в этом случае.
            return Err(stop_result.err().unwrap_or(RecordingError::NotStarted));
        };

        // 3. Фиксация в хранилище записей
        self.storage.lock().unwrap().save(meta.clone());
        self.stopped_cache.lock().unwrap().insert(*room_id, meta.clone());

        // 4. Строка AppState.records: сохраняем фактические width/height/
        // bitrate из старта (группа), обновляем длительность/размер.
        let fallback = RecorderProfile::default();
        let entry = {
            let mut records = state.records.lock().unwrap();
            let mut entry = records.get(&meta.id.to_string()).cloned().unwrap_or_else(|| RecordEntry {
                id: meta.id.to_string(),
                room_id: *room_id,
                created_ms: meta.started_at_ms,
                duration_sec: 0,
                size_bytes: 0,
                bitrate_kbps: fallback.bitrate_kbps(),
                width: fallback.width,
                height: fallback.height,
            });
            // A finished recording must never look ACTIVE (duration 0 is the
            // active marker): sub-second recordings round up to 1 s.
            entry.duration_sec = ((meta.duration_ms / 1000) as u32).max(1);
            entry.size_bytes = meta.size_bytes;
            records.insert(entry.id.clone(), entry.clone());
            entry
        };

        if spawn_bg_persistence {
            if let Some(ref storage) = state.storage {
                let st_clone = storage.clone();
                let entry_clone = entry.clone();
                tokio::spawn(async move {
                    let _ = st_clone.upsert_record(&entry_clone).await;
                });
            }
        }

        info!(room_id = %room_id, duration_ms = meta.duration_ms, size_bytes = meta.size_bytes, "Recording stopped for room");
        Ok(StopOutcome { meta, entry: Some(entry), finalize_error: stop_result.err() })
    }

    /// Снимает recorder-участника с RoomHub и рассылает ParticipantLeft.
    fn detach_room_participant(&self, state: &Arc<AppState>, room_id: &RoomId) {
        let rec_participant_id = format!("recorder-{room_id}");
        let _ = state.room_hub.unregister_participant(room_id, &rec_participant_id);
        let left_msg = SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                participant_id: rec_participant_id.clone(),
                reason: 0,
            })),
        };
        let payload_bytes = Envelope::new(left_msg).encode();
        state.room_hub.broadcast(room_id, None, payload_bytes.into());
        let room_str = room_id.to_string();
        state.notify_admin(
            &format!("session:{room_str}:abonents"),
            serde_json::json!({
                "action": "left",
                "room_id": room_str,
                "participant_id": rec_participant_id,
                "is_recorder": true
            }),
        );
        state.notify_admin("sessions:updated", serde_json::json!({ "id": room_str, "recording": false }));
    }

    /// Останавливает запись комнаты и сохраняет метаданные.
    /// Повторный вызов идемпотентно возвращает сохранённые метаданные без ошибки.
    ///
    /// Ошибка финализации контейнера возвращается вызывающему (а не
    /// превращается в «успех с пустым файлом»); строка записи и RoomHub уже
    /// приведены в согласованное состояние.
    pub fn stop_recording(&self, state: &Arc<AppState>, room_id: &RoomId) -> Result<RecordingMeta, RecordingError> {
        let outcome = self.stop_recording_internal(state, room_id, true)?;
        match outcome.finalize_error {
            Some(error) => Err(error),
            None => Ok(outcome.meta),
        }
    }

    /// Асинхронно останавливает запись комнаты с гарантированным ожиданием сохранения в Storage.
    pub async fn stop_recording_async(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
    ) -> Result<RecordingMeta, RecordingError> {
        let outcome = self.stop_recording_internal(state, room_id, false)?;
        if let (Some(ref storage), Some(entry)) = (&state.storage, outcome.entry.clone()) {
            // DELETE must not return 200 while the DB row still looks ACTIVE.
            storage.upsert_record(&entry).await.map_err(|error| RecordingError::Storage(error.to_string()))?;
        }
        match outcome.finalize_error {
            Some(error) => {
                warn!(room = %room_id, %error, "recording stopped, but container finalize failed");
                Err(error)
            }
            None => Ok(outcome.meta),
        }
    }
}

/// Итог остановки: метаданные + строка (для persistence) + ошибка finalize,
/// которую нельзя терять, но и нельзя мешать cleanup (P1-5).
struct StopOutcome {
    meta: RecordingMeta,
    entry: Option<RecordEntry>,
    finalize_error: Option<RecordingError>,
}

/// `ContainerConfig` из фактического профиля группы фермы (T09b/P1-1).
fn container_config_for(spec: &EncoderSpec) -> ContainerConfig {
    ContainerConfig {
        width: spec.width,
        height: spec.height,
        fps: spec.fps.max(1),
        video_codec: VideoCodec::H264,
        // Живого AAC-источника в записи MCU нет: файл video-only, пока не
        // появится audio tap + AAC-энкодер (отдельная задача).
        audio: None,
        max_size_bytes: 50 * 1024 * 1024,
    }
}

//! Интеграция подсистемы записи с сервером VKS и RoomHub (Milestone M5).
//!
//! Управляет жизненным циклом сессий записи, регистрирует рекордер как
//! специального участника комнаты (is_recorder: true) в RoomHub,
//! фиксирует метаданные в хранилище и уведомляет админ-панель.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;
use uuid::Uuid;

use vks_admin::hub::ParticipantSnapshot;
use vks_admin::state::RecordingControl;
use vks_admin::state::{now_unix_ms, AppState, RecordEntry};
use vks_core::RoomId;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_recorder::{
    MemoryStore, RecorderProfile, RecordingError, RecordingMeta, RecordingMode, RecordingSession, RecordingStore,
};
use vks_signaling::Envelope;

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
        // A recording that was active when the process died leaves an
        // unfinalized row (duration_sec == 0). Starting again must reuse the
        // room's recording slot, not accumulate duplicate active rows.
        if !self.recorder.is_recording(&room_id) {
            if let Some(storage) = state.storage.clone() {
                if let Ok(records) = storage.list_records(Some(room_id)).await {
                    for stale in records.iter().filter(|entry| entry.duration_sec == 0) {
                        let _ = storage.delete_record(&stale.id).await;
                        state.records.lock().unwrap().remove(&stale.id);
                    }
                }
            }
        }
        let record_id =
            self.recorder.start_recording(&state, &room_id, RecordingMode::Mcu).map_err(|error| error.to_string())?;
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
                if let Ok(records) = storage.list_records(Some(room_id)).await {
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
        self.mcu_engine
            .video_farm_stats()
            .into_iter()
            .filter(|stat| {
                stat.key.room == room_id
                    && stat.key.spec.codec == vks_media_mcu::farm::FarmCodec::H264
                    && stat.viewers > 0
            })
            .count()
    }
}

/// Менеджер сессий записи комнат.
#[derive(Clone)]
pub struct RecorderManager {
    sessions: Arc<Mutex<HashMap<RoomId, RecordingSession>>>,
    storage: Arc<Mutex<MemoryStore>>,
    stopped_cache: Arc<Mutex<HashMap<RoomId, RecordingMeta>>>,
    containers: Arc<Mutex<HashMap<String, Vec<u8>>>>,
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
        let mut guard = self.sessions.lock().unwrap();
        if let Some(existing) = guard.get(room_id) {
            // Idempotent retry: return the id of the recording already running.
            return Ok(existing.record_id().unwrap_or(*room_id.as_uuid()));
        }

        let room_uuid = *room_id.as_uuid();
        let mut session = RecordingSession::new(room_uuid, Box::new(now_unix_ms));
        session.start(mode)?;

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
        let entry = RecordEntry {
            id: record_id.to_string(),
            room_id: *room_id,
            created_ms: now_unix_ms(),
            duration_sec: 0,
            size_bytes: 0,
            bitrate_kbps: profile.bitrate_kbps(),
            width: profile.width,
            height: profile.height,
        };
        state.records.lock().unwrap().insert(entry.id.clone(), entry);

        // 4. Сохраняем сессию
        guard.insert(*room_id, session);
        self.stopped_cache.lock().unwrap().remove(room_id);

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
    ) -> Result<(RecordingMeta, Option<RecordEntry>), RecordingError> {
        let mut session = {
            let mut guard = self.sessions.lock().unwrap();
            match guard.remove(room_id) {
                Some(s) => s,
                None => {
                    // Проверяем кэш недавно остановленных сессий (идемпотентность)
                    if let Some(meta) = self.stopped_cache.lock().unwrap().get(room_id).cloned() {
                        return Ok((meta, None));
                    }
                    return Err(RecordingError::NotStarted);
                }
            }
        };

        let meta = session.stop()?;
        if let Some(bytes) = session.container_bytes() {
            self.save_container(&meta.id.to_string(), bytes.to_vec());
        }

        let rec_participant_id = format!("recorder-{room_id}");

        // 1. Убираем рекордер из RoomHub
        let _ = state.room_hub.unregister_participant(room_id, &rec_participant_id);

        // 2. Рассылка ParticipantLeft WebRTC-клиентам
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

        // 3. Фиксация в хранилище записей
        self.storage.lock().unwrap().save(meta.clone());
        self.stopped_cache.lock().unwrap().insert(*room_id, meta.clone());

        // 4. Добавляем запись в состояние AppState.records и персистируем в Storage
        let profile = RecorderProfile::default();
        let entry = RecordEntry {
            id: meta.id.to_string(),
            room_id: *room_id,
            created_ms: meta.started_at_ms,
            // A finished recording must never look ACTIVE (duration 0 is the
            // active marker): sub-second recordings round up to 1 s.
            duration_sec: ((meta.duration_ms / 1000) as u32).max(1),
            size_bytes: meta.size_bytes,
            bitrate_kbps: profile.bitrate_kbps(),
            width: profile.width,
            height: profile.height,
        };
        state.records.lock().unwrap().insert(entry.id.clone(), entry.clone());

        if spawn_bg_persistence {
            if let Some(ref storage) = state.storage {
                let st_clone = storage.clone();
                let entry_clone = entry.clone();
                tokio::spawn(async move {
                    let _ = st_clone.upsert_record(&entry_clone).await;
                });
            }
        }

        // 5. Оповещение административного WebSocket
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
        state.notify_admin(
            "sessions:updated",
            serde_json::json!({
                "id": room_str,
                "recording": false
            }),
        );

        info!(room_id = %room_id, duration_ms = meta.duration_ms, "Recording stopped for room");
        Ok((meta, Some(entry)))
    }

    /// Останавливает запись комнаты и сохраняет метаданные.
    /// Повторный вызов идемпотентно возвращает сохранённые метаданные без ошибки.
    pub fn stop_recording(&self, state: &Arc<AppState>, room_id: &RoomId) -> Result<RecordingMeta, RecordingError> {
        let (meta, _) = self.stop_recording_internal(state, room_id, true)?;
        Ok(meta)
    }

    /// Асинхронно останавливает запись комнаты с гарантированным ожиданием сохранения в Storage.
    pub async fn stop_recording_async(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
    ) -> Result<RecordingMeta, RecordingError> {
        let (meta, entry_opt) = self.stop_recording_internal(state, room_id, false)?;
        if let (Some(ref storage), Some(entry)) = (&state.storage, entry_opt) {
            // DELETE must not return 200 while the DB row still looks ACTIVE.
            storage.upsert_record(&entry).await.map_err(|error| RecordingError::Storage(error.to_string()))?;
        }
        Ok(meta)
    }
}

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
use vks_admin::state::{now_unix_ms, AppState, RecordEntry};
use vks_core::RoomId;
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_recorder::{MemoryStore, RecordingError, RecordingMeta, RecordingMode, RecordingSession, RecordingStore};
use vks_signaling::Envelope;

/// Менеджер сессий записи комнат.
#[derive(Clone)]
pub struct RecorderManager {
    sessions: Arc<Mutex<HashMap<RoomId, RecordingSession>>>,
    storage: Arc<Mutex<MemoryStore>>,
    stopped_cache: Arc<Mutex<HashMap<RoomId, RecordingMeta>>>,
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
        }
    }

    /// Проверяет, идёт ли сейчас запись в комнате.
    pub fn is_recording(&self, room_id: &RoomId) -> bool {
        let guard = self.sessions.lock().unwrap();
        guard.contains_key(room_id)
    }

    /// Список комнат с активной записью.
    pub fn active_recordings(&self) -> Vec<RoomId> {
        let guard = self.sessions.lock().unwrap();
        guard.keys().copied().collect()
    }

    /// Начинает запись комнаты в заданном режиме (MCU-композит или SFU multi-track).
    pub fn start_recording(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
        mode: RecordingMode,
    ) -> Result<Uuid, RecordingError> {
        let mut guard = self.sessions.lock().unwrap();
        if guard.contains_key(room_id) {
            // Уже пишется — возвращаем стабильный идентификатор комнаты
            return Ok(*room_id.as_uuid());
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

        // 3. Сохраняем сессию
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
        Ok(room_uuid)
    }

    /// Останавливает запись комнаты и сохраняет метаданные.
    /// Повторный вызов идемпотентно возвращает сохранённые метаданные без ошибки.
    pub fn stop_recording(&self, state: &Arc<AppState>, room_id: &RoomId) -> Result<RecordingMeta, RecordingError> {
        let mut session = {
            let mut guard = self.sessions.lock().unwrap();
            match guard.remove(room_id) {
                Some(s) => s,
                None => {
                    // Проверяем кэш недавно остановленных сессий (идемпотентность)
                    if let Some(meta) = self.stopped_cache.lock().unwrap().get(room_id).cloned() {
                        return Ok(meta);
                    }
                    return Err(RecordingError::NotStarted);
                }
            }
        };

        let meta = session.stop()?;

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
        let entry = RecordEntry {
            id: meta.id.to_string(),
            room_id: *room_id,
            created_ms: meta.started_at_ms,
            duration_sec: (meta.duration_ms / 1000) as u32,
            size_bytes: meta.size_bytes,
        };
        state.records.lock().unwrap().insert(entry.id.clone(), entry.clone());
        if let Some(ref storage) = state.storage {
            let st_clone = storage.clone();
            tokio::spawn(async move {
                let _ = st_clone.upsert_record(&entry).await;
            });
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
        Ok(meta)
    }

    /// Асинхронно останавливает запись комнаты с гарантированным ожиданием сохранения в Storage.
    pub async fn stop_recording_async(
        &self,
        state: &Arc<AppState>,
        room_id: &RoomId,
    ) -> Result<RecordingMeta, RecordingError> {
        let meta = self.stop_recording(state, room_id)?;
        if let Some(ref storage) = state.storage {
            let entry = RecordEntry {
                id: meta.id.to_string(),
                room_id: *room_id,
                created_ms: meta.started_at_ms,
                duration_sec: (meta.duration_ms / 1000) as u32,
                size_bytes: meta.size_bytes,
            };
            let _ = storage.upsert_record(&entry).await;
        }
        Ok(meta)
    }
}

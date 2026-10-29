//! Event Fan-out Hub (M2.7): RoomHub for signaling WebRTC sessions and AdminEvent bus.
//!
//! Provides lock-free ring-buffer broadcast channels (via `tokio::sync::broadcast`)
//! for inter-session fan-out within rooms, active participant roster tracking, and
//! admin WebSocket push notifications.

use std::collections::HashMap;
use std::sync::RwLock;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use vks_core::RoomId;

/// Capacity for room broadcast channels (ring buffer).
pub const ROOM_CHANNEL_CAPACITY: usize = 256;
/// Capacity for the admin event channel.
pub const ADMIN_CHANNEL_CAPACITY: usize = 256;

/// A signaling broadcast event within a room.
#[derive(Debug, Clone)]
pub struct RoomBroadcastEvent {
    /// ID of the sender participant (for filtering out self-echo).
    pub sender_participant_id: Option<String>,
    /// Serialized binary signaling payload (e.g. Protobuf).
    pub payload: Bytes,
}

/// An administrative push event for `/api/v3/ws`.
#[derive(Debug, Clone)]
pub struct AdminBroadcastEvent {
    /// Event topic name (e.g. "conferences:update", "participant:joined").
    pub topic: String,
    /// JSON payload.
    pub payload: Value,
}

/// Снимок участника для сигнального ростера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantSnapshot {
    pub participant_id: String,
    pub display_name: String,
    pub is_recorder: bool,
    pub is_speaking: bool,
    pub track_ids: Vec<String>,
    #[serde(default)]
    pub is_audio_muted: bool,
    #[serde(default)]
    pub is_video_muted: bool,
    #[serde(default)]
    pub connected_at: u64,
}

#[derive(Debug)]
struct RoomEntry {
    sender: broadcast::Sender<RoomBroadcastEvent>,
    participants: HashMap<String, ParticipantSnapshot>,
}

/// Fan-out hub for signaling sessions and participant rosters in rooms.
#[derive(Debug, Default)]
pub struct RoomHub {
    channels: RwLock<HashMap<RoomId, RoomEntry>>,
}

impl RoomHub {
    /// Creates a new empty RoomHub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes to events in the given room.
    pub fn subscribe(&self, room_id: &RoomId) -> broadcast::Receiver<RoomBroadcastEvent> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let entry = map.entry(*room_id).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel(ROOM_CHANNEL_CAPACITY);
            RoomEntry { sender: tx, participants: HashMap::new() }
        });
        entry.sender.subscribe()
    }

    /// Registers a participant in the room roster and atomically returns
    /// the broadcast receiver along with the complete participant snapshot.
    pub fn register_participant(
        &self,
        room_id: &RoomId,
        participant: ParticipantSnapshot,
    ) -> (broadcast::Receiver<RoomBroadcastEvent>, Vec<ParticipantSnapshot>) {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let entry = map.entry(*room_id).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel(ROOM_CHANNEL_CAPACITY);
            RoomEntry { sender: tx, participants: HashMap::new() }
        });
        let rx = entry.sender.subscribe();
        entry.participants.insert(participant.participant_id.clone(), participant);
        let snapshot = entry.participants.values().cloned().collect();
        (rx, snapshot)
    }

    /// Unregisters a participant from the room roster, returning the remaining count if the room was found.
    pub fn unregister_participant(&self, room_id: &RoomId, participant_id: &str) -> Option<usize> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        if let Some(entry) = map.get_mut(room_id) {
            if entry.participants.remove(participant_id).is_none() {
                let clean = crate::api_v3::sessions::sanitize_id(participant_id);
                if entry.participants.remove(clean).is_none() {
                    let matching_key =
                        entry.participants.keys().find(|k| crate::api_v3::sessions::sanitize_id(k) == clean).cloned();
                    if let Some(k) = matching_key {
                        entry.participants.remove(&k);
                    }
                }
            }
            let remaining = entry.participants.len();
            if remaining == 0 && entry.sender.receiver_count() == 0 {
                map.remove(room_id);
            }
            Some(remaining)
        } else {
            None
        }
    }

    /// Returns the current participant roster for a room.
    pub fn list_participants(&self, room_id: &RoomId) -> Vec<ParticipantSnapshot> {
        let map = self.channels.read().expect("RoomHub lock poisoned");
        map.get(room_id).map(|e| e.participants.values().cloned().collect()).unwrap_or_default()
    }

    /// Broadcasts an event to all subscribers in the room.
    pub fn broadcast(&self, room_id: &RoomId, sender_pid: Option<&str>, payload: Bytes) {
        let map = self.channels.read().expect("RoomHub lock poisoned");
        if let Some(entry) = map.get(room_id) {
            if entry.sender.receiver_count() > 0 {
                let event = RoomBroadcastEvent { sender_participant_id: sender_pid.map(str::to_string), payload };
                let _ = entry.sender.send(event);
            }
        }
    }

    /// Unsubscribes from the room, cleaning up empty channels and rosters.
    pub fn unsubscribe(&self, room_id: &RoomId) {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        if let Some(entry) = map.get(room_id) {
            if entry.sender.receiver_count() == 0 && entry.participants.is_empty() {
                map.remove(room_id);
            }
        }
    }

    /// Finds which room contains the given participant_id, returning (RoomId, ParticipantSnapshot).
    pub fn find_participant_room(&self, participant_id: &str) -> Option<(RoomId, ParticipantSnapshot)> {
        let map = self.channels.read().expect("RoomHub lock poisoned");
        for (room_id, entry) in map.iter() {
            if let Some(snap) = entry.participants.get(participant_id) {
                return Some((*room_id, snap.clone()));
            }
        }
        let clean = crate::api_v3::sessions::sanitize_id(participant_id);
        if clean != participant_id {
            for (room_id, entry) in map.iter() {
                if let Some(snap) = entry.participants.get(clean) {
                    return Some((*room_id, snap.clone()));
                }
            }
        }
        for (room_id, entry) in map.iter() {
            for (pid, snap) in entry.participants.iter() {
                if crate::api_v3::sessions::sanitize_id(pid) == clean {
                    return Some((*room_id, snap.clone()));
                }
            }
        }
        None
    }

    /// Заменяет список треков участника (например, при деградации до audio-only).
    pub fn update_participant_tracks(
        &self,
        room_id: &RoomId,
        participant_id: &str,
        track_ids: Vec<String>,
    ) -> Option<ParticipantSnapshot> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let entry = map.get_mut(room_id)?;
        let participant = entry.participants.get_mut(participant_id)?;
        participant.track_ids = track_ids;
        Some(participant.clone())
    }

    /// Обновляет признак «говорит» в ростере (S12: состояние VAD доходит до
    /// админки, а не остаётся в движке).
    pub fn update_participant_speaking(
        &self,
        room_id: &RoomId,
        participant_id: &str,
        is_speaking: bool,
    ) -> Option<ParticipantSnapshot> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let entry = map.get_mut(room_id)?;
        let participant = entry.participants.get_mut(participant_id)?;
        participant.is_speaking = is_speaking;
        Some(participant.clone())
    }

    /// Atomically updates a participant's media state (audio/video mute or display name) in the roster.
    pub fn update_participant_media(
        &self,
        room_id: &RoomId,
        participant_id: &str,
        audio_muted: Option<bool>,
        video_muted: Option<bool>,
        display_name: Option<String>,
    ) -> Option<ParticipantSnapshot> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let entry = map.get_mut(room_id)?;
        let clean = crate::api_v3::sessions::sanitize_id(participant_id);
        let p = if entry.participants.contains_key(participant_id) {
            entry.participants.get_mut(participant_id)?
        } else if entry.participants.contains_key(clean) {
            entry.participants.get_mut(clean)?
        } else {
            let matching_key =
                entry.participants.keys().find(|k| crate::api_v3::sessions::sanitize_id(k) == clean)?.clone();
            entry.participants.get_mut(&matching_key)?
        };
        if let Some(a) = audio_muted {
            p.is_audio_muted = a;
        }
        if let Some(v) = video_muted {
            p.is_video_muted = v;
        }
        if let Some(name) = display_name {
            p.display_name = name;
        }
        Some(p.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_hub_sanitized_id_fallbacks() {
        let hub = RoomHub::new();
        let room_id = RoomId::new();
        let p_snap = ParticipantSnapshot {
            participant_id: "terminal-alpha".into(),
            display_name: "Alpha Terminal".into(),
            is_recorder: false,
            is_speaking: false,
            track_ids: vec!["t1".into()],
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 1000,
        };
        let _ = hub.register_participant(&room_id, p_snap);

        // 1. Direct search
        assert!(hub.find_participant_room("terminal-alpha").is_some());

        // 2. Fallback search with curly braces {id}}
        let found = hub.find_participant_room("{terminal-alpha}}");
        assert!(found.is_some());
        assert_eq!(found.unwrap().1.participant_id, "terminal-alpha");

        // 3. Fallback update media with braces
        let updated = hub.update_participant_media(
            &room_id,
            "{{terminal-alpha}}",
            Some(true),
            None,
            Some("Renamed Alpha".into()),
        );
        assert!(updated.is_some());
        let snap = updated.unwrap();
        assert!(snap.is_audio_muted);
        assert_eq!(snap.display_name, "Renamed Alpha");

        // 4. Fallback unregister with braces
        let remaining = hub.unregister_participant(&room_id, "{terminal-alpha}}");
        assert_eq!(remaining, Some(0));
        assert!(hub.find_participant_room("terminal-alpha").is_none());
    }
}

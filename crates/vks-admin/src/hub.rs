//! Event Fan-out Hub (M2.7): RoomHub for signaling WebRTC sessions and AdminEvent bus.
//!
//! Provides lock-free ring-buffer broadcast channels (via `tokio::sync::broadcast`)
//! for inter-session fan-out within rooms and admin WebSocket push notifications.

use std::collections::HashMap;
use std::sync::RwLock;

use bytes::Bytes;
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

/// Fan-out hub for signaling sessions in rooms.
#[derive(Debug, Default)]
pub struct RoomHub {
    channels: RwLock<HashMap<RoomId, broadcast::Sender<RoomBroadcastEvent>>>,
}

impl RoomHub {
    /// Creates a new empty RoomHub.
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes to events in the given room.
    pub fn subscribe(&self, room_id: &RoomId) -> broadcast::Receiver<RoomBroadcastEvent> {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        let sender = map.entry(*room_id).or_insert_with(|| {
            let (tx, _rx) = broadcast::channel(ROOM_CHANNEL_CAPACITY);
            tx
        });
        sender.subscribe()
    }

    /// Broadcasts an event to all subscribers in the room.
    pub fn broadcast(&self, room_id: &RoomId, sender_pid: Option<&str>, payload: Bytes) {
        let map = self.channels.read().expect("RoomHub lock poisoned");
        if let Some(tx) = map.get(room_id) {
            if tx.receiver_count() > 0 {
                let event = RoomBroadcastEvent {
                    sender_participant_id: sender_pid.map(str::to_string),
                    payload,
                };
                let _ = tx.send(event);
            }
        }
    }

    /// Unsubscribes from the room, cleaning up empty channels.
    pub fn unsubscribe(&self, room_id: &RoomId) {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        if let Some(tx) = map.get(room_id) {
            if tx.receiver_count() == 0 {
                map.remove(room_id);
            }
        }
    }

    /// Closes all subscriptions in a room (e.g. when room is deleted).
    pub fn close_room(&self, room_id: &RoomId) {
        let mut map = self.channels.write().expect("RoomHub lock poisoned");
        map.remove(room_id);
    }
}

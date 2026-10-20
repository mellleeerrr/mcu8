use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::auth_middleware::AuthType;
use crate::state::{AppState, OutboundDialer};

#[derive(Default)]
struct PendingDialer {
    started: tokio::sync::Notify,
    cancelled: Arc<AtomicBool>,
}

struct PendingGuard(Arc<AtomicBool>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl OutboundDialer for PendingDialer {
    async fn dial(&self, _: RoomId, _: &str, _: Option<&str>) -> Result<String, String> {
        let _guard = PendingGuard(self.cancelled.clone());
        self.started.notify_one();
        std::future::pending().await
    }
}

#[derive(Default)]
struct CapturingDialer {
    #[allow(clippy::type_complexity)] // (audio_tx, audio_rx, video_tx, video_rx, gain_tx, gain_rx)
    controls: Mutex<Vec<(bool, bool, bool, bool, u32, u32)>>,
    complete: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl OutboundDialer for CapturingDialer {
    async fn dial(&self, _: RoomId, _: &str, _: Option<&str>) -> Result<String, String> {
        Ok("test-participant@sip-gateway".into())
    }
    fn set_media_control(
        &self,
        _: &str,
        at: Option<bool>,
        ar: Option<bool>,
        vt: Option<bool>,
        vr: Option<bool>,
        gt: Option<u32>,
        gr: Option<u32>,
    ) -> bool {
        self.controls.lock().unwrap().push((
            at.unwrap(),
            ar.unwrap(),
            vt.unwrap(),
            vr.unwrap(),
            gt.unwrap(),
            gr.unwrap(),
        ));
        self.complete.notify_one();
        true
    }
}

fn record(id: &str, uri: &str, payload: Value) -> CallStringRecord {
    CallStringRecord {
        id: id.into(),
        uri: uri.into(),
        display_name: "Terminal".into(),
        created_at: 123,
        payload: default_contact_payload(Some(&payload)),
    }
}

#[test]
fn native_contact_payload_uses_tenths_db_and_address_options() {
    let payload = default_contact_payload(Some(&json!({"volume_rx":-60,"volume_tx":200,"crown":"SELECTED",
        "call_string":{"sip":{"audio_codecs":[{"name":"G711","bitrate":null}],"self_view":false}}})));
    assert_eq!(payload["volume_rx"], -60);
    assert_eq!(gain_percent(&payload["volume_rx"]), 50);
    assert_eq!(gain_percent(&payload["volume_tx"]), 1000);
    assert_eq!(payload["crown"], "SELECTED");
    assert_eq!(payload["audio_rx_enabled"], true);
    assert_eq!(payload["call_string"]["sip"]["audioCodecs"][0]["name"], "G711");
    assert_eq!(payload["call_string"]["sip"]["isSelfViewEnabled"], false);
    assert!(payload["call_string"]["h323"].is_object());
    let address = address_book_record(&record("address", "sip:192.168.1.132", payload));
    assert!(address["payload"]["sip"].is_object());
    assert!(
        address["payload"].get("audio_rx_enabled").is_none(),
        "Address payload is protocol options, not the session controls envelope"
    );
    assert_eq!(canonical_uri("H323:192.168.1.132"), "h323:192.168.1.132");
    assert_eq!(canonical_uri("192.168.1.132:5060"), "sip:192.168.1.132:5060");
    assert_eq!(canonical_uri("rtsp://192.168.1.132/stream"), "rtsp://192.168.1.132/stream");
    let patched = merged_contact_payload(
        Some(&json!({"volume_rx":-60,"audio_rx_enabled":false})),
        Some(&json!({"call_string":{"sip":{"bitrate":512}}})),
    );
    assert_eq!(patched["volume_rx"], -60);
    assert_eq!(patched["audio_rx_enabled"], false);
    assert_eq!(patched["call_string"]["sip"]["bitrate"], 512);
}

#[tokio::test]
async fn pending_call_cancellation_drops_future_and_keeps_stable_outcome() {
    let state = Arc::new(AppState::new());
    let dialer = Arc::new(PendingDialer::default());
    state.set_dialer(dialer.clone());
    let room = RoomId::new();
    queue_call(&state, room, record("contact", "sip:192.168.1.132", json!({"volume_rx":-60}))).unwrap();
    tokio::time::timeout(Duration::from_secs(2), dialer.started.notified()).await.unwrap();
    state.api_v3.stopped_sessions.lock().unwrap().insert(room);
    cancel_pending_calls(&state, &room);
    for _ in 0..20 {
        if dialer.cancelled.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(dialer.cancelled.load(Ordering::SeqCst), "Stopping must cancel the actual pending signaling future");
    assert!(state.api_v3.in_flight_calls.lock().unwrap().is_empty());
    let attempts = state.api_v3.call_attempts.lock().unwrap();
    let attempt = attempts.get(&(room, "sip:192.168.1.132".into())).unwrap();
    assert!(attempt.participant_id.is_none());
    assert_eq!(attempt.failure_reason.as_deref(), Some("Session ended before connection"));
    assert!(attempt.completed_at.is_some());
    assert_eq!(attempt.record.payload["volume_rx"], -60);
}

#[tokio::test]
async fn connected_call_applies_native_rx_to_bridge_tx_and_preserves_gain() {
    let state = Arc::new(AppState::new());
    let dialer = Arc::new(CapturingDialer::default());
    state.set_dialer(dialer.clone());
    let room = RoomId::new();
    queue_call(
        &state,
        room,
        record(
            "contact",
            "sip:192.168.1.132",
            json!({
                "audio_rx_enabled":false,"audio_tx_enabled":true,"video_rx_enabled":true,"video_tx_enabled":false,
                "volume_rx":-60,"volume_tx":60
            }),
        ),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), dialer.complete.notified()).await.unwrap();
    assert_eq!(dialer.controls.lock().unwrap()[0], (true, false, false, true, 50, 200));
    let media = state.api_v3.abonent_settings.lock().unwrap();
    let media = media.get("test-participant@sip-gateway").unwrap();
    assert!(media.audio_rx_muted);
    assert!(!media.audio_tx_muted);
    assert_eq!(media.volume_rx, -60);
    assert_eq!(media.volume_tx, 60);
}

#[test]
fn recall_finds_standby_only_in_its_room_even_with_duplicate_contact_ids() {
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let other = RoomId::new();
    let local = record("same-id", "sip:192.168.1.132", json!({"volume_rx":-60}));
    let foreign = record("same-id", "h323:192.168.1.250", json!({"volume_rx":100}));
    state.api_v3.scheduled_calls.lock().unwrap().insert(room, vec![local.clone()]);
    state.api_v3.call_attempts.lock().unwrap().insert(
        (other, foreign.uri.clone()),
        CallAttempt {
            record: foreign,
            generation: Uuid::new_v4(),
            participant_id: None,
            failure_reason: Some("Foreign failure".into()),
            completed_at: Some(100),
            abort_handle: None,
        },
    );
    let found = lookup_session_record(&state, &room, Some("same-id")).unwrap();
    assert_eq!(found.uri, local.uri);
    assert_eq!(found.payload["volume_rx"], -60);
    assert!(lookup_session_record(&state, &RoomId::new(), Some("same-id")).is_none());
}

#[tokio::test]
async fn global_address_lookup_respects_room_scope_and_returns_protocol_payload() {
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let other = RoomId::new();
    state.api_v3.scheduled_calls.lock().unwrap().insert(room, vec![record("local", "sip:192.168.1.132", json!({}))]);
    state
        .api_v3
        .scheduled_calls
        .lock()
        .unwrap()
        .insert(other, vec![record("foreign", "h323:192.168.1.250", json!({}))]);
    let auth = AuthContext {
        subject: "operator".into(),
        role: Role::Operator,
        auth_type: AuthType::Jwt,
        scopes: vec!["*".into()],
        room_scope: Some(room.to_string()),
        generation: 0,
        client_ip: "127.0.0.1".into(),
    };
    let records = address_book_records(&state, &auth).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "local");
    assert!(records[0]["payload"]["sip"].is_object());
}

#[test]
fn missing_gateway_never_admits_a_fake_connected_participant() {
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let response = queue_call(&state, room, record("contact", "sip:192.168.1.132", json!({}))).unwrap_err();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(state.api_v3.call_attempts.lock().unwrap().is_empty());
    assert!(state.api_v3.in_flight_calls.lock().unwrap().is_empty());
    assert!(state.room_hub.list_participants(&room).is_empty());
}

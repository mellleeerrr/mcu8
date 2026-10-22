use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::api_v3::tests::json_body as response_json;
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
    assert_eq!(canonical_uri("192.168.1.132:5060"), "sip:192.168.1.132");
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
            connected_at: None,
            stopped_at: None,
            stop_reason: None,
            incoming: false,
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

fn operator_auth(role: Role) -> AuthContext {
    AuthContext {
        subject: "operator".into(),
        role,
        auth_type: AuthType::Jwt,
        scopes: vec!["*".into()],
        room_scope: None,
        generation: 0,
        client_ip: "127.0.0.1".into(),
    }
}

/// T1.5: адресная книга — CRUD, фильтры UI (`uri`, `protocols`, `search`,
/// пагинация), персистентность и ремонт битых записей при загрузке.
#[tokio::test]
async fn address_book_crud_filters_and_persists() {
    let mut state = AppState::new();
    state.storage = Some(Arc::new(vks_storage::memory::MemoryStorage::new()));
    let state = Arc::new(state);
    crate::users::Users::bootstrap_operator(&state, "operator");
    let auth = operator_auth(Role::Operator);
    let create = |uri: &str| {
        create_call_string(
            State(state.clone()),
            Extension(auth.clone()),
            Json(json!({"uri": uri, "payload": {"sip": {"bitrate": 2048}}})),
        )
    };

    let first = create("192.168.1.50").await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first = response_json(first).await;
    let sip = first["call_string"].clone();
    assert_eq!(sip["uri"], "sip:192.168.1.50");
    assert_eq!(sip["display_name"], "192.168.1.50");
    assert_eq!(sip["contact"]["contact_type"], "call_strings");
    assert_eq!(sip["payload"]["sip"]["bitrate"], 2048);

    let second = create("h323:192.168.1.60").await;
    let second = response_json(second).await["call_string"].clone();
    assert_eq!(second["display_name"], "192.168.1.60");

    let list = |params: &[(&str, &str)]| {
        let params: HashMap<String, String> =
            params.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect();
        list_call_strings(State(state.clone()), Extension(auth.clone()), Query(params))
    };
    let filtered = response_json(list(&[("uri", "1.50"), ("protocols", "sip")]).await).await;
    assert_eq!(filtered["count"], 1, "{filtered}");
    assert_eq!(filtered["call_strings"][0]["id"], sip["id"]);

    let by_name = response_json(list(&[("search", "1.60")]).await).await;
    assert_eq!(by_name["count"], 1, "{by_name}");
    assert_eq!(by_name["call_strings"][0]["id"], second["id"]);

    let paged = response_json(
        list(&[("page", "2"), ("page_size", "1"), ("sort_field", "created_at"), ("sort_order", "0")]).await,
    )
    .await;
    assert_eq!(paged["count"], 2);
    assert_eq!(paged["call_strings"].as_array().map(Vec::len), Some(1));

    let id = sip["id"].as_str().unwrap().to_string();
    let patched = update_call_string(
        State(state.clone()),
        Extension(auth.clone()),
        Path(id.clone()),
        Json(json!({"display_name": "Зал", "payload": {"sip": {"bitrate": 4096}}})),
    )
    .await;
    let patched = response_json(patched).await["call_string"].clone();
    assert_eq!(patched["display_name"], "Зал");
    assert_eq!(patched["payload"]["sip"]["bitrate"], 4096);
    assert_eq!(patched["uri"], "sip:192.168.1.50");

    let ws = ws_search(&state, "operator", None, &json!({"search": "зал", "page_size": 5})).await.unwrap();
    assert_eq!(ws["count"], 1, "{ws}");
    assert_eq!(ws["call_strings"][0]["id"], id);

    let forbidden = update_call_string(
        State(state.clone()),
        Extension(operator_auth(Role::Viewer)),
        Path(id.clone()),
        Json(json!({"display_name": "x"})),
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    assert_eq!(
        delete_call_string(State(state.clone()), Extension(auth.clone()), Path(id.clone())).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete_call_string(State(state.clone()), Extension(auth.clone()), Path(id.clone())).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(state.api_v3.resources.lock().await.addresses.len(), 1);
    let raw = state
        .storage
        .as_ref()
        .unwrap()
        .get_configuration_document(super::super::resources::CONFIGURATION_KEY)
        .await
        .unwrap()
        .unwrap();
    let stored: Value = serde_json::from_str(&raw).unwrap();
    let stored_id = second["id"].as_str().unwrap();
    assert_eq!(stored["resources"]["addresses"][stored_id]["uri"], "h323:192.168.1.60");
    assert_eq!(stored["resources"]["addresses"][stored_id]["payload"]["sip"]["bitrate"], 2048);
}

/// T1.5: чтение книги — Moderator+, запись — Operator без room_scope; пустой
/// payload WS и «космический» page не ломают ответы.
#[tokio::test]
async fn address_book_access_page_bounds_and_ws_defaults() {
    let state = Arc::new(AppState::new());
    crate::users::Users::bootstrap_operator(&state, "operator");
    crate::users::Users::create(&state, "viewer", Role::Viewer);
    let auth = operator_auth(Role::Operator);
    for index in 0..3 {
        let response = create_call_string(
            State(state.clone()),
            Extension(auth.clone()),
            Json(json!({"uri": format!("sip:10.0.0.{index}")})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let viewer = operator_auth(Role::Viewer);
    let denied = list_call_strings(State(state.clone()), Extension(viewer.clone()), Query(HashMap::new())).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        ws_search(&state, "operator", None, &json!({})).await.unwrap()["call_strings"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(ws_search(&state, "viewer", None, &json!({})).await.unwrap_err().0, 403);
    let params: HashMap<String, String> =
        [("page".to_string(), usize::MAX.to_string()), ("page_size".to_string(), "2".to_string())].into();
    let response = list_call_strings(State(state.clone()), Extension(auth.clone()), Query(params)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_json(response).await["count"], 3);

    let empty = create_call_string(State(state.clone()), Extension(auth.clone()), Json(json!({"uri": "sip:"}))).await;
    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);
}

/// T1.5: `PATCH`/`DELETE /call_strings/{id}` работают и для контактов комнаты
/// (vendor `editAddress(addressId)` в редакторе контакта), с проверкой комнаты.
#[tokio::test]
async fn address_patch_and_delete_resolve_room_contacts() {
    let state = Arc::new(AppState::new());
    let room = RoomId::new();
    let contact = json!({
        "id": "cs-contact", "contact_id": "cs-contact", "contact_type": "call_strings",
        "uri": "sip:192.168.1.132", "connection_url": "sip:192.168.1.132",
        "display_name": "Зал 132", "created_at": 5,
        "payload": {"auto_call": false, "volume_rx": -60, "audio_tx_enabled": false,
                    "crown": false, "slot_render_type": 3,
                    "call_string": {"sip": {"bitrate": 1}}},
        "call_string": {"id": "cs-contact", "uri": "sip:192.168.1.132", "display_name": "Зал 132"}
    });
    state.api_v3.resources.lock().await.contacts.insert(room.to_string(), vec![contact]);
    let auth = operator_auth(Role::Operator);
    let patched = update_call_string(
        State(state.clone()),
        Extension(auth.clone()),
        Path("cs-contact".into()),
        Json(json!({"payload": {"sip": {"bitrate": 9}}})),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    let patched = response_json(patched).await;
    assert_eq!(patched["call_string"]["payload"]["sip"]["bitrate"], 9);
    assert_eq!(patched["call_string"]["display_name"], "Зал 132");
    // vendor `editAddress` меняет одно поле: top-level настройки контакта не теряются
    {
        let resources = state.api_v3.resources.lock().await;
        let stored = &resources.contacts[&room.to_string()][0]["payload"];
        assert_eq!(stored["auto_call"], false, "{stored}");
        assert_eq!(stored["volume_rx"], -60, "{stored}");
        assert_eq!(stored["audio_tx_enabled"], false, "{stored}");
        assert_eq!(stored["crown"], "EQUAL", "{stored}");
        assert_eq!(stored["slot_render_type"], "DEFAULT", "{stored}");
        assert_eq!(stored["call_string"]["sip"]["bitrate"], 9, "{stored}");
    }

    let mut viewer = operator_auth(Role::Viewer);
    viewer.room_scope = Some(room.to_string());
    let forbidden = update_call_string(
        State(state.clone()),
        Extension(viewer),
        Path("cs-contact".into()),
        Json(json!({"display_name": "x"})),
    )
    .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    assert_eq!(
        delete_call_string(State(state.clone()), Extension(auth), Path("cs-contact".into())).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(state.api_v3.resources.lock().await.contacts[&room.to_string()].is_empty());
}

/// T1.5: битые persisted-записи адресной книги не роняют `ResourceState`
/// (а с ним layout'ы): чинятся поля, отбрасываются записи без uri.
#[test]
fn address_records_tolerate_broken_persisted_document() {
    let raw = json!({"addresses": {
        "good": {"uri": "h323:192.168.1.132", "payload": {"h323": {"bitrate": 4096}, "sip": "broken"}},
        "broken": {"uri": 42},
        "empty": {}
    }});
    let loaded: crate::api_v3::resources::ResourceState = serde_json::from_value(raw).expect("tolerant load");
    assert_eq!(loaded.addresses.len(), 1);
    let good = &loaded.addresses["good"];
    assert_eq!(good.uri, "h323:192.168.1.132");
    assert_eq!(good.display_name, "h323:192.168.1.132");
    assert_eq!(good.payload["h323"]["bitrate"], 4096);
    assert!(good.payload.get("sip").is_none(), "не-объектные протоколы отбрасываются");
}

/// T1.5: битый persisted payload протокола не должен ронять PATCH (release abort).
#[tokio::test]
async fn address_patch_tolerates_broken_stored_protocol_payload() {
    let state = Arc::new(AppState::new());
    let auth = operator_auth(Role::Operator);
    let created =
        create_call_string(State(state.clone()), Extension(auth.clone()), Json(json!({"uri": "sip:192.168.1.77"})))
            .await;
    let id = response_json(created).await["call_string"]["id"].as_str().unwrap().to_string();
    state.api_v3.resources.lock().await.addresses.get_mut(&id).unwrap().payload = json!({"sip": "broken"});
    let patched = update_call_string(
        State(state.clone()),
        Extension(auth.clone()),
        Path(id.clone()),
        Json(json!({"payload": {"sip": {"bitrate": 64}}})),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);
    assert_eq!(response_json(patched).await["call_string"]["payload"]["sip"]["bitrate"], 64);
}

/// T1.5: нестроковые поля отклоняются 400 (как create, так и PATCH), а
/// повторяющийся id контакта в разных комнатах не резолвится молчаливым выбором.
#[tokio::test]
async fn address_patch_rejects_bad_types_and_ambiguous_ids() {
    let state = Arc::new(AppState::new());
    let auth = operator_auth(Role::Operator);
    let created =
        create_call_string(State(state.clone()), Extension(auth.clone()), Json(json!({"uri": "sip:10.9.9.9"}))).await;
    let id = response_json(created).await["call_string"]["id"].as_str().unwrap().to_string();
    let bad =
        update_call_string(State(state.clone()), Extension(auth.clone()), Path(id.clone()), Json(json!({"uri": 123})))
            .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let body = response_json(bad).await;
    assert!(
        body["error"]["details"][0]["reason"].as_str().unwrap_or_default().contains("field must be a string"),
        "{body}"
    );

    let contact = |uri: &str| {
        json!({"id": "cs-dup", "uri": uri, "display_name": uri, "created_at": 1,
               "payload": {"call_string": {}}})
    };
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.contacts.insert("room-a".into(), vec![contact("sip:10.1.1.1")]);
        resources.contacts.insert("room-b".into(), vec![contact("sip:10.1.1.2")]);
    }
    let ambiguous = update_call_string(
        State(state.clone()),
        Extension(auth.clone()),
        Path("cs-dup".into()),
        Json(json!({"display_name": "x"})),
    )
    .await;
    assert_eq!(ambiguous.status(), StatusCode::CONFLICT);
    assert_eq!(
        delete_call_string(State(state.clone()), Extension(auth), Path("cs-dup".into())).await.status(),
        StatusCode::CONFLICT
    );
}

/// T1.5: room-scoped Operator не может вытащить глобальный адрес по id через
/// приглашение — глобальная книга требует unscoped-чтения (Moderator+).
#[tokio::test]
async fn invite_global_address_requires_global_read_scope() {
    let state = Arc::new(AppState::new());
    let room = crate::rooms::Rooms::create_async(&state, "T1.5 invite scope").await.unwrap();
    let auth = operator_auth(Role::Operator);
    let created =
        create_call_string(State(state.clone()), Extension(auth.clone()), Json(json!({"uri": "sip:10.7.7.7"}))).await;
    let id = response_json(created).await["call_string"]["id"].as_str().unwrap().to_string();
    let mut scoped = operator_auth(Role::Operator);
    scoped.room_scope = Some(room.id.to_string());
    let response = invite_conference_call_strings(
        State(state.clone()),
        Extension(scoped),
        None,
        Path(room.id.to_string()),
        Json(InviteCallStringsRequest {
            call_strings: vec![CallStringItem { id: Some(id), uri: None, display_name: None, payload: None }],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

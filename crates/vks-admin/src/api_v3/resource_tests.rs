use std::sync::Arc;

use axum::body::to_bytes;

use super::*;
use crate::auth_middleware::AuthType;
use crate::rooms::Rooms;
use crate::state::AppState;

fn state() -> SharedState {
    let mut state = AppState::new();
    state.storage = Some(Arc::new(vks_storage::memory::MemoryStorage::new()));
    Arc::new(state)
}

fn operator() -> AuthContext {
    AuthContext {
        subject: "resource-test-operator".into(),
        role: vks_core::Role::Operator,
        auth_type: AuthType::Jwt,
        scopes: vec!["*".into()],
        room_scope: None,
        generation: 0,
        client_ip: "127.0.0.1".into(),
    }
}

fn uri(path: &str) -> OriginalUri {
    OriginalUri(path.parse().unwrap())
}

async fn body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn pnm_scoped_resources_require_attachment_and_persist_edits() {
    let state = state();
    let room = Rooms::create_async(&state, "Scoped definition").await.unwrap();
    let other = Rooms::create_async(&state, "Other definition").await.unwrap();
    for (kind, singular_kind) in [("video_layouts", "video_layout"), ("audiochannels", "audiochannel")] {
        let response = create(
            State(state.clone()),
            Extension(operator()),
            uri(&format!("/api/v3/{kind}")),
            Json(json!({"display_name":"Original","payload":{}})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let record = body(response).await[singular_kind].clone();
        let id = record["id"].as_str().unwrap().to_string();
        let path = format!("/api/v3/conferences/{}/{kind}/{id}", room.id);
        let response = get_linked(
            State(state.clone()),
            Extension(operator()),
            uri(&path),
            Path((room.id.to_string(), id.clone())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "Unattached IDs must not leak into a conference");
        let response =
            attach(State(state.clone()), Extension(operator()), uri(&path), Path((room.id.to_string(), id.clone())))
                .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let response = edit_linked(
            State(state.clone()),
            Extension(operator()),
            uri(&path),
            Path((room.id.to_string(), id.clone())),
            Json(json!({"display_name":"Edited","payload":{"autoscroll":20,"vad":false}})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let result = body(response).await[singular_kind].clone();
        assert_eq!(result["id"], id);
        assert_eq!(result["display_name"], "Edited");
        assert_eq!(result["payload"]["autoscroll"], 20);
        let response = get_linked(
            State(state.clone()),
            Extension(operator()),
            uri(&path),
            Path((other.id.to_string(), id.clone())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let raw = state.storage.as_ref().unwrap().get_configuration_document(CONFIGURATION_KEY).await.unwrap().unwrap();
        let stored: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            stored["resources"][if kind == "video_layouts" { "layouts" } else { "audiochannels" }][&id]["display_name"],
            "Edited"
        );
    }
}

#[tokio::test]
async fn pnm_schedule_crud_retains_native_cron_contract_and_room_scope() {
    let state = state();
    let room = Rooms::create_async(&state, "Scheduled definition").await.unwrap();
    let response = create_schedule(
        State(state.clone()),
        Extension(operator()),
        Json(json!({
            "object_id":room.id.to_string(),"object_type":"conferences","cron_expression":"30 9 * * 1,3,5",
            "time_zone":"Europe/Moscow","valid_from":0,"valid_until":null,"payload":{"duration":3600}
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let schedule = body(response).await["schedule"].clone();
    let id = schedule["id"].as_str().unwrap().to_string();
    let response = edit_schedule(
        State(state.clone()),
        Extension(operator()),
        Path(id.clone()),
        Json(json!({"cron_expression":"0 10 * * 2","payload":{"duration":1800}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["schedule"]["object_id"], room.id.to_string());
    let mut restricted = operator();
    restricted.room_scope = Some(vks_core::RoomId::new().to_string());
    let response = get_schedule(State(state.clone()), Extension(restricted), Path(id.clone())).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let raw = state.storage.as_ref().unwrap().get_configuration_document(CONFIGURATION_KEY).await.unwrap().unwrap();
    let stored: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(stored["resources"]["schedules"][&id]["cron_expression"], "0 10 * * 2");
    assert_eq!(stored["resources"]["schedules"][&id]["payload"]["duration"], 1800);
    let response = remove_schedule(State(state.clone()), Extension(operator()), Path(id.clone())).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let raw = state.storage.as_ref().unwrap().get_configuration_document(CONFIGURATION_KEY).await.unwrap().unwrap();
    let stored: Value = serde_json::from_str(&raw).unwrap();
    assert!(stored["resources"]["schedules"].get(id).is_none());
}

#[tokio::test]
async fn pnm_invalid_schedule_does_not_replace_existing_definition() {
    let state = state();
    let room = Rooms::create_async(&state, "Scheduled definition").await.unwrap();
    let response = create_schedule(
        State(state.clone()),
        Extension(operator()),
        Json(json!({
            "object_id":room.id.to_string(),"object_type":"conferences","cron_expression":"0 10 * * 2",
            "time_zone":"Europe/Moscow","payload":{"duration":1800}
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let id = body(response).await["schedule"]["id"].as_str().unwrap().to_string();
    for patch in [
        json!({"cron_expression":"0 25 * * 2"}),
        json!({"valid_from":100,"valid_until":50}),
        json!({"payload":{"duration":-1}}),
    ] {
        let response = edit_schedule(State(state.clone()), Extension(operator()), Path(id.clone()), Json(patch)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let response = get_schedule(State(state), Extension(operator()), Path(id)).await;
    let retained = body(response).await;
    assert_eq!(retained["schedule"]["cron_expression"], "0 10 * * 2");
    assert_eq!(retained["schedule"]["payload"]["duration"], 1800);
}

#[test]
fn pnm_partial_resource_payload_preserves_other_options() {
    let mut record = normalize_resource(
        "video_layouts",
        &json!({"payload":{"mode":"manual","slots":[{"position":7,"role":"AUTO"}]}}),
        "creator",
    );
    update_resource(&mut record, &json!({"payload":{"autoscroll":20}}));
    assert_eq!(record["payload"]["mode"], "manual");
    assert_eq!(record["payload"]["slots"][0]["position"], 7);
    assert!(record["contacts"]["contacts"].is_array());
    assert!(record["abonents"]["abonents"]["abonents"].is_array());
    assert!(validate_schedule(&json!({"cron_expression":"0 10 * * 2","time_zone":"UTC","payload":{"duration":null}}))
        .is_ok());
}

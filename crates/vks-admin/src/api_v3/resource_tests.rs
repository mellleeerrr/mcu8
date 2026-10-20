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
    update_resource(&mut record, &json!({"payload":{"autoscroll":20}})).expect("valid patch");
    assert_eq!(record["payload"]["mode"], "manual");
    assert_eq!(record["payload"]["slots"][0]["position"], 7);
    // `my → Mee` ждёт `{abonents:{count,abonents:[...]}}`; плоский/null роняют `.map`.
    assert!(record["abonents"]["abonents"]["abonents"].is_array());
    assert_eq!(record["abonents"]["abonents"]["count"], 0);
    assert!(record["contacts"]["contacts"].is_array());
    assert!(validate_schedule(&json!({"cron_expression":"0 10 * * 2","time_zone":"UTC","payload":{"duration":null}}))
        .is_ok());
}

async fn attach_abonent_call(state: SharedState, room: vks_core::RoomId, layout_id: String, pid: &str) -> Response {
    attach_layout_abonent(
        State(state),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{room}/video_layouts/{layout_id}/abonents/{pid}")),
        Path((room.to_string(), layout_id.clone(), pid.to_string())),
        Json(json!({"connection_id":pid,"contact_id":"cs-1","contact_type":"call_strings"})),
    )
    .await
}

fn participant(pid: &str, name: &str) -> crate::hub::ParticipantSnapshot {
    crate::hub::ParticipantSnapshot {
        participant_id: pid.to_string(),
        display_name: name.to_string(),
        is_recorder: false,
        is_speaking: false,
        track_ids: Vec::new(),
        is_audio_muted: false,
        is_video_muted: false,
        connected_at: 0,
    }
}

/// T1.4: ручная раскладка отдаёт только занятые слоты, attach занимает свободное окно,
/// detach освобождает окно без сдвига геометрии, битый JSON не роняет процесс
/// (release: panic=abort), individual-layout доезжает до участника.
#[tokio::test]
async fn pnm_layout_abonents_attach_detach_and_malformed_payloads() {
    let state = state();
    let room = Rooms::create_async(&state, "Layout room").await.unwrap();
    state.room_hub.register_participant(&room.id, participant("sip-1@sip-gateway", "Terminal"));
    state.room_hub.register_participant(&room.id, participant("sip-2@sip-gateway", "Second"));

    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({
            "display_name":"Individual",
            "payload":{"mode":"manual","preset":"preset_equal_4","type":"individual","slots":[]}
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let record = body(response).await["video_layout"].clone();
    let layout_id = record["id"].as_str().unwrap().to_string();
    assert_eq!(record["payload"]["size"], 4, "размер выводится из пресета");
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, layout_id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&base), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Ручная раскладка без слотов НЕ показывает живой ростер (в отличие от auto).
    let response = layout_abonents(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents")),
        Path((room.id.to_string(), layout_id.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["count"], 0);

    let response = attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-1@sip-gateway").await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-2@sip-gateway").await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = layout_abonents(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents")),
        Path((room.id.to_string(), layout_id.clone())),
    )
    .await;
    let abonents = body(response).await;
    assert_eq!(abonents["count"], 2);
    assert_eq!(abonents["abonents"][0]["display_name"], "Terminal");
    assert_eq!(abonents["abonents"][0]["position"], 1);
    assert_eq!(abonents["abonents"][0]["role"], "PEER");
    assert_eq!(abonents["abonents"][1]["position"], 2);

    // Individual-layout доезжает до DTO участника (hasIndividualLayout в UI).
    let response = crate::api_v3::sessions::session_abonents(
        State(state.clone()),
        Extension(operator()),
        Path(room.id.to_string()),
    )
    .await;
    let connected = body(response).await["connected"].clone();
    assert_eq!(connected[0]["video_layout_id"], layout_id);

    // Битые slots: 422 на запись, без паники на чтение/удаление.
    for malformed in [json!(null), json!([1]), json!("nope")] {
        let response = edit_global(
            State(state.clone()),
            Extension(operator()),
            uri(&format!("/api/v3/video_layouts/{layout_id}")),
            Path(layout_id.clone()),
            Json(json!({"payload":{"slots":malformed}})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "slots must be validated");
    }

    // Испорченное storage-состояние (slots:null) — attach/detach отвечают 409, не паникуют.
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.layouts.get_mut(&layout_id).unwrap()["payload"]["slots"] = Value::Null;
    }
    let response = attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-1@sip-gateway").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = detach_layout_abonent(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents/sip-1@sip-gateway")),
        Path((room.id.to_string(), layout_id.clone(), "sip-1@sip-gateway".to_string())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    // detach из нормального состояния: окно освобождается (AUTO), позиции не сдвигаются.
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.layouts.get_mut(&layout_id).unwrap()["payload"]["slots"] = json!([
            {"position":1,"role":"PEER","abonent_id":"sip-1@sip-gateway","display_name":"Terminal"},
            {"position":2,"role":"PEER","abonent_id":"sip-2@sip-gateway","display_name":"Second"}
        ]);
    }
    let response = detach_layout_abonent(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents/sip-1@sip-gateway")),
        Path((room.id.to_string(), layout_id.clone(), "sip-1@sip-gateway".to_string())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = layout_abonents(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents")),
        Path((room.id.to_string(), layout_id.clone())),
    )
    .await;
    let abonents = body(response).await;
    assert_eq!(abonents["count"], 1);
    assert_eq!(abonents["abonents"][0]["position"], 2);
}

/// T1.4: auto-раскладка показывает живой ростер; «Общая» наследуется сессией один раз
/// (без orphan-записи), `normalize_loaded` чинит битые записи и висячие ссылки.
#[tokio::test]
async fn pnm_default_layout_inheritance_and_storage_normalization() {
    let state = state();
    let room = Rooms::create_async(&state, "Inheritance").await.unwrap();
    state.room_hub.register_participant(&room.id, participant("h323-a@h323-gateway", "Legacy terminal"));

    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/conferences/video_layouts"),
        Json(json!({"payload":{"mode":"auto","type":"default"}})),
    )
    .await;
    let created = body(response).await;
    let default_id = created["video_layout"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, default_id);
    let response = attach(
        State(state.clone()),
        Extension(operator()),
        uri(&base),
        Path((room.id.to_string(), default_id.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = layout_abonents(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents")),
        Path((room.id.to_string(), default_id.clone())),
    )
    .await;
    let abonents = body(response).await;
    assert_eq!(abonents["count"], 1);
    assert_eq!(abonents["abonents"][0]["contact_type"], "h323");

    let mut resources = state.api_v3.resources.lock().await;
    inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "creator");
    let layouts_before = resources.layouts.len();
    inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "creator");
    assert_eq!(resources.layouts.len(), layouts_before, "повторный вызов идемпотентен");
    assert_eq!(
        resources.links.get(&format!("sessions:{}:video_layouts", room.id)).map(Vec::len),
        Some(1),
        "сессия наследует раскладку конференции без дубля"
    );

    // Битые записи и висячие ссылки чинятся при загрузке.
    resources.layouts.insert("broken".to_string(), json!({"payload":"not-an-object"}));
    resources.layouts.insert("broken-record".to_string(), Value::Null);
    resources.layouts.insert("orphan".to_string(), json!({"id":"orphan","payload":{"mode":"manual"}}));
    resources.links.entry("sessions:dangling:video_layouts".to_string()).or_default().push("ghost".to_string());
    normalize_loaded(&mut resources);
    assert!(resources.layouts.contains_key(&default_id));
    assert!(resources.layouts.contains_key("orphan"), "непривязанная раскладка не должна теряться при рестарте");
    assert!(!resources.layouts.contains_key("broken-record"), "не-объект отбрасывается");
    assert!(resources.layouts.contains_key("broken"), "объект с битым payload чинится, а не удаляется");
    let fixed = resources.layouts.get(&default_id).unwrap();
    assert!(fixed["abonents"]["abonents"]["abonents"].is_array());
    assert!(resources.links.values().flatten().all(|id| resources.layouts.contains_key(id)));
}

/// attach в auto-раскладку не меняет mode: получатели individual видны по type.
#[tokio::test]
async fn pnm_attach_to_auto_layout_keeps_mode() {
    let state = state();
    let room = Rooms::create_async(&state, "Auto layout room").await.unwrap();
    state.room_hub.register_participant(&room.id, participant("sip-9@sip-gateway", "Terminal"));
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Auto", "payload":{"mode":"auto","preset":"preset_equal_4","slots":[]}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout = body(response).await["video_layout"].clone();
    let layout_id = layout["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, layout_id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&base), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-9@sip-gateway").await;
    assert!(response.status().is_success());
    let stored = state.api_v3.resources.lock().await.layouts.get(&layout_id).cloned().unwrap();
    assert_eq!(stored["payload"]["mode"], "auto", "режим раскладки не подменяется");
    assert_eq!(stored["payload"]["slots"][0]["abonent_id"], "sip-9@sip-gateway");
}

/// P1.1: сетка из 25 окон с 1 занятым — второй получатель занимает свободное окно,
/// а не получает ложный 409 «Layout is full».
#[tokio::test]
async fn pnm_attach_second_receiver_into_grid_with_free_windows() {
    let state = state();
    let room = Rooms::create_async(&state, "Receivers room").await.unwrap();
    state.room_hub.register_participant(&room.id, participant("sip-1@sip-gateway", "First"));
    state.room_hub.register_participant(&room.id, participant("sip-2@sip-gateway", "Second"));
    let mut slots: Vec<Value> = (1..=25).map(|position| json!({"position": position, "role": "AUTO"})).collect();
    slots[0] = json!({"position":1,"role":"PEER","abonent_id":"sip-1@sip-gateway","display_name":"First"});
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Individual","payload":{"mode":"manual","preset":"preset_equal_25","type":"individual","slots":slots}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, layout_id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&base), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert!(response.status().is_success());

    let response = attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-2@sip-gateway").await;
    assert!(response.status().is_success(), "свободные окна есть — 409 недопустим");
    let stored = state.api_v3.resources.lock().await.layouts.get(&layout_id).cloned().unwrap();
    let occupied: Vec<_> = stored["payload"]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|slot| slot["abonent_id"].as_str().is_some())
        .collect();
    assert_eq!(occupied.len(), 2);
    assert_eq!(occupied[1]["position"], 2, "окно занято без сдвига геометрии");
    assert_eq!(occupied[1]["display_name"], "Second");
}

/// P1.2: у individual-раскладки получатели — слоты, независимо от mode (auto).
#[tokio::test]
async fn pnm_individual_auto_layout_shows_only_attached_receivers() {
    let state = state();
    let room = Rooms::create_async(&state, "Individual auto").await.unwrap();
    state.room_hub.register_participant(&room.id, participant("sip-1@sip-gateway", "First"));
    state.room_hub.register_participant(&room.id, participant("sip-2@sip-gateway", "Second"));
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Auto individual","payload":{"mode":"auto","preset":"preset_equal_4","type":"individual","slots":[]}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, layout_id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&base), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert!(response.status().is_success());
    assert!(attach_abonent_call(state.clone(), room.id, layout_id.clone(), "sip-1@sip-gateway")
        .await
        .status()
        .is_success());

    let response = layout_abonents(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/abonents")),
        Path((room.id.to_string(), layout_id.clone())),
    )
    .await;
    let abonents = body(response).await;
    assert_eq!(abonents["count"], 1, "auto-режим не должен подмешивать всех участников сессии");
    assert_eq!(abonents["abonents"][0]["display_name"], "First");
}

/// P2.4: PATCH без display_name сохраняет имена получателей в слотах.
#[test]
fn pnm_layout_patch_preserves_slot_recipient_metadata() {
    let mut record = normalize_resource(
        "video_layouts",
        &json!({"payload":{"mode":"manual","type":"individual","slots":[
            {"position":1,"role":"PEER","abonent_id":"sip-1@sip-gateway","display_name":"Terminal","contact_id":"cs-1"}
        ]}}),
        "creator",
    );
    update_resource(
        &mut record,
        &json!({"payload":{"slots":[{"position":1,"role":"PEER","abonent_id":"sip-1@sip-gateway"},{"position":2,"role":"AUTO"}]}}),
    )
    .expect("valid patch");
    assert_eq!(record["payload"]["slots"][0]["display_name"], "Terminal");
    assert_eq!(record["payload"]["slots"][0]["contact_id"], "cs-1");
    assert_eq!(record["payload"]["size"], 2);
}

/// P2.6: удаление комнаты не оставляет её раскладки/links в storage и глобальном списке.
#[tokio::test]
async fn pnm_room_purge_removes_scoped_resources() {
    let state = state();
    let room = Rooms::create_async(&state, "Purge room").await.unwrap();
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Only room","payload":{"mode":"manual","preset":"preset_equal_4","slots":[]}})),
    )
    .await;
    let layout_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts/{}", room.id, layout_id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&base), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert!(response.status().is_success());
    {
        let resources = state.api_v3.resources.lock().await;
        assert!(resources.layouts.contains_key(&layout_id));
    }
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Global draft","payload":{"mode":"auto","preset":"preset_default_13","slots":[]}})),
    )
    .await;
    let global_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    purge_room_resources(&state, &room.id.to_string()).await;
    {
        let resources = state.api_v3.resources.lock().await;
        assert!(!resources.layouts.contains_key(&layout_id), "раскладка удалённой комнаты вычищается");
        assert!(resources.layouts.contains_key(&global_id), "непривязанная глобальная раскладка сохраняется");
        assert!(
            !resources.links.keys().any(|key| key.contains(&room.id.to_string())),
            "scoped-ссылки удалённой комнаты удалены"
        );
    }
}

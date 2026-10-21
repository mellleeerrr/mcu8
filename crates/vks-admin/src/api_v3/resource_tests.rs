use std::sync::Arc;

use super::*;
use crate::api_v3::tests::json_body as body;
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

/// Вкладка аудиоканалов ходит в `list_linked`: пагинация/поиск должны работать
/// и для привязанных к комнате записей, не только для глобальной коллекции.
#[tokio::test]
async fn pnm_linked_audiochannels_list_filters_and_pages() {
    let state = state();
    let room = Rooms::create_async(&state, "Linked audio definitions").await.unwrap();
    let path = format!("/api/v3/conferences/{}/audiochannels", room.id);
    for (name, created_at) in [("Зал", 10), ("Сцена", 20)] {
        let response = create(
            State(state.clone()),
            Extension(operator()),
            uri(&path),
            Json(json!({"display_name": name, "payload": {}})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let id = body(response).await["audiochannel"]["id"].as_str().unwrap().to_string();
        {
            let mut resources = state.api_v3.resources.lock().await;
            resources.audiochannels.get_mut(&id).unwrap()["created_at"] = json!(created_at);
        }
        let response =
            attach(State(state.clone()), Extension(operator()), uri(&path), Path((room.id.to_string(), id))).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let list = |params: HashMap<String, String>| {
        list_linked(State(state.clone()), Extension(operator()), uri(&path), Path(room.id.to_string()), Query(params))
    };
    let searched = body(list(HashMap::from([("search".to_string(), "зал".to_string())])).await).await;
    assert_eq!(searched["count"], 1, "{searched}");
    assert_eq!(searched["audiochannels"][0]["display_name"], "Зал");
    let paged = body(
        list(HashMap::from([
            ("page".to_string(), "2".to_string()),
            ("page_size".to_string(), "1".to_string()),
            ("sort_field".to_string(), "created_at".to_string()),
            ("sort_order".to_string(), "1".to_string()),
        ]))
        .await,
    )
    .await;
    assert_eq!(paged["count"], 2);
    assert_eq!(paged["audiochannels"][0]["display_name"], "Зал");
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

/// После рестарта привязки аудиоканалов не должны теряться: prune в
/// `normalize_loaded` знает про оба вида ресурсов.
#[test]
fn pnm_normalize_keeps_audiochannel_links_and_drops_dangling() {
    let mut resources = ResourceState::default();
    resources.audiochannels.insert("audio-1".into(), json!({"id": "audio-1", "payload": {}}));
    resources.layouts.insert("layout-1".into(), json!({"id": "layout-1", "payload": {"type": "default"}}));
    resources.links.insert("sessions:r1:audiochannels".into(), vec!["audio-1".into(), "gone".into()]);
    resources.links.insert("sessions:r1:video_layouts".into(), vec!["layout-1".into()]);
    resources.links.insert("sessions:r2:audiochannels".into(), vec!["gone".into()]);

    normalize_loaded(&mut resources);

    assert_eq!(resources.links.get("sessions:r1:audiochannels"), Some(&vec!["audio-1".to_string()]));
    assert_eq!(resources.links.get("sessions:r1:video_layouts"), Some(&vec!["layout-1".to_string()]));
    assert!(!resources.links.contains_key("sessions:r2:audiochannels"), "пустые привязки удалены");
}

/// Аудиоканал: счётчики таблицы считаются из payload, список поддерживает
/// page/page_size/search/sort (контракт вкладки «Аудиоканалы»).
#[tokio::test]
async fn pnm_audiochannels_list_filters_and_counts_speakers_listeners() {
    let state = Arc::new(crate::state::AppState::new());
    let auth = AuthContext {
        subject: "operator".into(),
        role: vks_core::Role::Operator,
        auth_type: crate::auth_middleware::AuthType::Jwt,
        scopes: vec!["*".into()],
        room_scope: None,
        generation: 0,
        client_ip: "127.0.0.1".into(),
    };
    let mut resources = state.api_v3.resources.lock().await;
    resources.audiochannels.insert(
        "a1".into(),
        json!({"id":"a1","display_name":"Зал","created_at":1,"payload":{
            "type":"individual","speakers":["p1","p2"],"listeners":["p3"]}}),
    );
    resources.audiochannels.insert(
        "a2".into(),
        json!({"id":"a2","display_name":"Сцена","created_at":2,"payload":{
            "type":"individual","speakers":[],"listeners":[]}}),
    );
    drop(resources);
    let response = list_global(
        State(state.clone()),
        Extension(auth.clone()),
        OriginalUri("/api/v3/audiochannels".parse().unwrap()),
        Query(HashMap::from([("search".to_string(), "зал".to_string())])),
    )
    .await;
    let listed = body(response).await;
    assert_eq!(listed["count"], 1, "{listed}");
    assert_eq!(listed["audiochannels"][0]["id"], "a1");

    let response = list_global(
        State(state.clone()),
        Extension(auth),
        OriginalUri("/api/v3/audiochannels".parse().unwrap()),
        Query(HashMap::from([
            ("page".to_string(), "2".to_string()),
            ("page_size".to_string(), "1".to_string()),
            ("sort_field".to_string(), "created_at".to_string()),
            ("sort_order".to_string(), "1".to_string()),
        ])),
    )
    .await;
    let paged = body(response).await;
    assert_eq!(paged["count"], 2);
    assert_eq!(paged["audiochannels"][0]["id"], "a1", "created_at desc, вторая страница");

    let mut resources = state.api_v3.resources.lock().await;
    let channel = resources.audiochannels.get_mut("a1").unwrap();
    channel["payload"]["speakers"] = json!([]);
    channel["payload"]["listeners"] = json!(["p3", "p4"]);
    update_resource(channel, &json!({})).unwrap();
    assert_eq!(resources.audiochannels["a1"]["speakers"], 0);
    assert_eq!(resources.audiochannels["a1"]["listeners"], 2);
}

/// `GET /video_layout_grids` — эталон 2.2.1.66 отдаёт **ровно 6** встроенных
/// custom-схем (`video_layout_grids._`), а не синтетический список пресетов:
/// пресеты `preset_*` — статическая карта UI и в grid-каталог не входят.
#[tokio::test]
async fn pnm_video_layout_grids_return_six_custom_schemes() {
    let all = all_grids();
    assert_eq!(all.len(), 6, "эталонный каталог custom-схем");
    assert!(all.iter().all(|grid| !grid["id"].as_str().unwrap_or_default().starts_with("preset_")));

    let response = grids(Query(HashMap::new())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body(response).await;
    assert_eq!(listed["count"], 6);
    let items = listed["grids"].as_array().unwrap();
    assert_eq!(items.len(), 6);
    // payload в единицах сетки (8×8, 10×8, 4×4), а не в пикселях/10000.
    assert_eq!(items[0]["payload"]["width"], 8);
    assert_eq!(items[0]["payload"]["height"], 8);
    assert_eq!(items[0]["payload"]["slots"].as_array().unwrap().len(), 2);
    assert_eq!(items[0]["payload"]["centered_row"], json!(null));
    // Вендорные custom-схемы редактируемые и удаляемые, но не отключаемые.
    assert_eq!(items[0]["defaults"], json!({"deletable":true,"editable":true,"disableable":false}));

    // Поиск UI-маппера приходит как `display_name`.
    let response = grids(Query(HashMap::from([("display_name".to_string(), "Custom scheme 6".to_string())]))).await;
    let found = body(response).await;
    assert_eq!(found["count"], 1, "{found}");
    assert_eq!(found["grids"][0]["id"], "9bf78747-e1b2-42c9-b10e-1ece67a269e7");
    assert_eq!(found["grids"][0]["payload"]["slots"][0], json!({"x":1,"y":0,"width":2,"height":2}));

    // Пагинация: count — total, не размер страницы.
    let response = grids(Query(HashMap::from([
        ("page".to_string(), "2".to_string()),
        ("page_size".to_string(), "2".to_string()),
        ("sort_field".to_string(), "id".to_string()),
        ("sort_order".to_string(), "0".to_string()),
    ])))
    .await;
    let paged = body(response).await;
    assert_eq!(paged["count"], 6);
    assert_eq!(paged["grids"].as_array().unwrap().len(), 2);

    // get_grid: UUID custom-схемы; id пресета больше не является grid-записью.
    let response = get_grid(Path("9bf78747-e1b2-42c9-b10e-1ece67a269e7".to_string())).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["grid"]["display_name"], "Custom scheme 6");
    let response = get_grid(Path("preset_equal_4".to_string())).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Шаблоны — полные записи привязанных manual-раскладок (эталон: id,
/// display_name, creator_id, created_at, payload); auto/dynamic не синтезируются.
async fn fetch_templates(state: SharedState, room: vks_core::RoomId, params: HashMap<String, String>) -> Value {
    let path = format!("/api/v3/sessions/{}/video_layouts/templates", room);
    let response =
        templates(State(state), Extension(operator()), uri(&path), Path(room.to_string()), Query(params)).await;
    body(response).await
}

#[tokio::test]
async fn pnm_layout_templates_match_reference_contract() {
    let state = state();
    let room = Rooms::create_async(&state, "Templates room").await.unwrap();

    // Пустая комната: шаблонов нет — сервер ничего не выдумывает.
    let empty = fetch_templates(state.clone(), room.id, HashMap::new()).await;
    assert_eq!(empty["count"], 0, "{empty}");
    assert_eq!(empty["video_layout_templates"], json!([]));

    // dynamic-раскладка в шаблоны не попадает.
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Dynamic 2x2","payload":{"mode":"dynamic","preset":"preset_equal","type":"individual","size":4,"autoscroll":false}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let dynamic_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let base = format!("/api/v3/sessions/{}/video_layouts", room.id);
    let response = attach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/{dynamic_id}")),
        Path((room.id.to_string(), dynamic_id)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let dynamic = fetch_templates(state.clone(), room.id, HashMap::new()).await;
    assert_eq!(dynamic["count"], 0, "{dynamic}");

    // manual — полная запись layout в `video_layout_templates`.
    let slots = json!([
        {"position":1,"role":"PEER","abonent_id":"sip-1@sip-gateway","contact_id":"cs-1"}
    ]);
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Manual","payload":{"mode":"manual","preset":"preset_selected_b_4","type":"individual","slots":slots}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let manual_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let response = attach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("{base}/{manual_id}")),
        Path((room.id.to_string(), manual_id.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let manual = fetch_templates(state, room.id, HashMap::new()).await;
    assert_eq!(manual["count"], 1, "{manual}");
    let template = &manual["video_layout_templates"][0];
    assert_eq!(template["id"], json!(manual_id));
    assert!(template["display_name"].is_string());
    assert!(template["creator_id"].is_string());
    assert!(template["created_at"].is_number());
    assert_eq!(template["payload"]["mode"], "manual");
    assert_eq!(template["payload"]["preset"], "preset_selected_b_4");
    assert_eq!(template["payload"]["slots"][0]["position"], 1);
}

/// `POST /ui/video_layouts/calculate` — точная геометрия эталона 2.2.1.66:
/// 1-based позиции, крупная зона selected_b_4 сверху 2/3, equal_2 центрирован,
/// `_(2)` даёт 5/9, pip-окна с z_order=1, неизвестный/dynamic preset → 400.
async fn calculate_geometry(preset: &str) -> Value {
    let response =
        crate::api_v3::shell::video_layouts_calculate(Json(json!({"preset":preset,"width":1280,"height":720}))).await;
    body(response).await
}

#[tokio::test]
async fn pnm_calculate_uses_reference_preset_geometry() {
    let selected = calculate_geometry("preset_selected_b_4").await["slots"].as_array().unwrap().clone();
    assert_eq!(selected.len(), 4);
    assert_eq!(selected[0]["position"], 1);
    assert_eq!(selected[3]["position"], 4);
    assert_eq!(
        (
            selected[0]["x"].as_f64(),
            selected[0]["y"].as_f64(),
            selected[0]["width"].as_f64(),
            selected[0]["height"].as_f64()
        ),
        (Some(0.0), Some(0.0), Some(1280.0), Some(480.0)),
        "эталон: крупная зона selected_b_4 сверху, 2/3 высоты"
    );
    assert_eq!(selected[1]["width"], 426.0);
    assert_eq!(selected[2]["width"], 427.0);

    let equal_two = calculate_geometry("preset_equal_2").await["slots"].as_array().unwrap().clone();
    assert_eq!(equal_two[0]["y"], 180.0, "equal_2 центрирован по вертикали");
    assert_eq!(equal_two[0]["height"], 360.0);

    let variant = calculate_geometry("preset_selected_b_4_(2)").await["slots"].as_array().unwrap().clone();
    assert_eq!(variant[0]["height"], 400.0, "вариант _(2): крупная зона 5/9");

    let overlap = calculate_geometry("preset_selected_b_overlap_4").await["slots"].as_array().unwrap().clone();
    assert_eq!(overlap.len(), 4);
    assert_eq!(overlap[0]["width"], 1280.0);
    assert_eq!(overlap[0]["z_order"], 0);
    assert_eq!(overlap[1]["z_order"], 1, "pip-окна поверх большой зоны");

    // dynamic/base id не являются manual-пресетами — эталон отвечает 400.
    for invalid in ["preset_default_13", "preset_equal", "preset_unknown_4", "preset_equal_5"] {
        let response = crate::api_v3::shell::video_layouts_calculate(Json(json!({
            "preset": invalid, "width":1280, "height":720
        })))
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{invalid}");
    }
}

/// Запоминает раскладки, доехавшие до медиа-движка.
#[derive(Default)]
struct RecordingLayoutDialer {
    layouts: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl crate::state::OutboundDialer for RecordingLayoutDialer {
    async fn dial(
        &self,
        _room_id: vks_core::RoomId,
        _uri: &str,
        _display_name: Option<&str>,
    ) -> Result<String, String> {
        Err("dial must not be used by layout tests".into())
    }

    async fn apply_layout(&self, room_id: vks_core::RoomId, layout: Value) -> Result<(), String> {
        let id = layout["id"].as_str().unwrap_or_default().to_string();
        self.layouts.lock().expect("layouts poisoned").push((room_id.to_string(), id));
        Ok(())
    }
}

/// «Применить» в UI — это `POST …/video_layouts/{id}` (attach). Раскладка
/// обязана доехать до MCU сразу, без ожидания PATCH/abonent-мутации.
#[tokio::test]
async fn pnm_attach_layout_pushes_it_to_media_engine() {
    let dialer = Arc::new(RecordingLayoutDialer::default());
    let mut app = AppState::new().with_dialer(dialer.clone());
    app.storage = Some(Arc::new(vks_storage::memory::MemoryStorage::new()));
    let state: SharedState = Arc::new(app);
    let room = Rooms::create_async(&state, "Apply layout").await.unwrap();

    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({
            "display_name": "Applied 4",
            "payload": {"mode":"manual","type":"individual","preset":"preset_equal_4",
                "slots":[{"position":1,"role":"AUTO"}]}
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();

    let path = format!("/api/v3/conferences/{}/video_layouts/{layout_id}", room.id);
    let response =
        attach(State(state.clone()), Extension(operator()), uri(&path), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let applied = dialer.layouts.lock().expect("layouts poisoned").clone();
    assert_eq!(applied.len(), 1, "attach обязан один раз передать layout в движок");
    assert_eq!(applied[0], (room.id.to_string(), layout_id.clone()));

    let response =
        detach(State(state.clone()), Extension(operator()), uri(&path), Path((room.id.to_string(), layout_id.clone())))
            .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    // T02: scope после detach не остаётся без раскладки — добавляется/применяется
    // каноническая «Общая», и движок получает валидный active.
    assert_eq!(dialer.layouts.lock().expect("layouts poisoned").len(), 2);
    let resources = state.api_v3.resources.lock().await;
    assert!(resources.links[&format!("conferences:{}:video_layouts", room.id)]
        .iter()
        .any(|id| resources.layouts.get(id).is_some_and(|layout| layout["payload"]["type"] == "default")));
}

/// Корень дублей default-раскладок: повторный старт комнаты не должен
/// создавать новую «Общую», если канонический payload уже есть.
#[test]
fn default_layout_is_reused_between_rooms() {
    let mut resources = ResourceState::default();
    inherit_and_ensure_default_layout(&mut resources, "room-a", "creator");
    inherit_and_ensure_default_layout(&mut resources, "room-b", "creator");
    assert_eq!(resources.layouts.len(), 1, "одна каноническая default-раскладка: {:?}", resources.layouts.keys());
    let a = &resources.links["sessions:room-a:video_layouts"];
    let b = &resources.links["sessions:room-b:video_layouts"];
    assert_eq!(a, b, "обе комнаты ссылаются на один id");
    assert_eq!(a.len(), 1);
}

/// Copy-on-write: правка общей default-раскладки из одной комнаты не меняет
/// раскладку другой; ссылка текущего scope переключается на клон.
#[tokio::test]
async fn editing_shared_default_layout_clones_for_current_scope() {
    let state = state();
    let room_a = Rooms::create_async(&state, "COW room A").await.unwrap();
    let room_b = Rooms::create_async(&state, "COW room B").await.unwrap();
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name":"Shared default","payload":{"type":"default","mode":"auto","preset":"preset_default_13","size":13}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let layout = body(response).await["video_layout"].clone();
    let layout_id = layout["id"].as_str().unwrap().to_string();
    for room in [&room_a, &room_b] {
        let path = format!("/api/v3/conferences/{}/video_layouts/{layout_id}", room.id);
        let response = attach(
            State(state.clone()),
            Extension(operator()),
            uri(&path),
            Path((room.id.to_string(), layout_id.clone())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let path = format!("/api/v3/conferences/{}/video_layouts/{layout_id}", room_a.id);
    let response = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&path),
        Path((room_a.id.to_string(), layout_id.clone())),
        Json(json!({"display_name":"Edited in A"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let edited = body(response).await["video_layout"].clone();
    let edited_id = edited["id"].as_str().unwrap().to_string();
    assert_ne!(edited_id, layout_id, "при 2 ссылках правка создаёт клон");

    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.layouts[&layout_id]["display_name"], "Shared default", "оригинал не изменён");
    assert_eq!(resources.layouts[&edited_id]["display_name"], "Edited in A");
    assert_eq!(
        resources.links[&format!("conferences:{}:video_layouts", room_a.id)],
        vec![edited_id],
        "scope A переключён на клон"
    );
    assert_eq!(
        resources.links[&format!("conferences:{}:video_layouts", room_b.id)],
        vec![layout_id],
        "scope B остался на оригинале"
    );
}

// ---------------------------------------------------------------------------
// T02: active layout, COW atomicity, refcounted delete, canonical default.
// ---------------------------------------------------------------------------

async fn create_layout(state: &SharedState, name: &str, payload: serde_json::Value) -> String {
    let response = create(
        State(state.clone()),
        Extension(operator()),
        uri("/api/v3/video_layouts"),
        Json(json!({"display_name": name, "payload": payload})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    body(response).await["video_layout"]["id"].as_str().unwrap().to_string()
}

async fn attach_to(state: &SharedState, room: &crate::state::RoomRecord, layout_id: &str) -> StatusCode {
    attach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout_id}", room.id)),
        Path((room.id.to_string(), layout_id.to_string())),
    )
    .await
    .status()
}

#[tokio::test]
async fn t02_attach_apply_sequence_uses_last_attached_without_reordering_links() {
    let dialer = Arc::new(RecordingLayoutDialer::default());
    let mut app = AppState::new().with_dialer(dialer.clone());
    app.storage = Some(Arc::new(vks_storage::memory::MemoryStorage::new()));
    let state: SharedState = Arc::new(app);
    let room = Rooms::create_async(&state, "A/B/apply A").await.unwrap();
    let a =
        create_layout(&state, "A", json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}))
            .await;
    let b =
        create_layout(&state, "B", json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}))
            .await;
    assert_eq!(attach_to(&state, &room, &a).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room, &b).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room, &a).await, StatusCode::NO_CONTENT);
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(
        resources.links[&format!("conferences:{}:video_layouts", room.id)],
        vec![a.clone(), b.clone()],
        "attach не пересортировывает UI-порядок links"
    );
    assert_eq!(resources.active_layouts[&format!("conferences:{}:video_layouts", room.id)], a);
    drop(resources);
    let applied = dialer.layouts.lock().unwrap().clone();
    assert_eq!(applied.last(), Some(&(room.id.to_string(), a)), "последний attach должен победить");
}

#[tokio::test]
async fn t02_attach_default_after_manual_activates_default() {
    let state = state();
    let room = Rooms::create_async(&state, "default after manual").await.unwrap();
    let shared = create_layout(&state, "Common", json!({"type":"default","mode":"auto"})).await;
    let manual = create_layout(
        &state,
        "Manual",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &manual).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room, &shared).await, StatusCode::NO_CONTENT);
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.active_layouts[&format!("conferences:{}:video_layouts", room.id)], shared);
}

#[tokio::test]
async fn t02_generic_create_reuses_canonical_default() {
    let state = state();
    let first = create_layout(&state, "", json!({"type":"default"})).await;
    let second = create_layout(&state, "", json!({"type":"default"})).await;
    assert_eq!(first, second, "каноническая «Общая» должна переиспользоваться");
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.layouts.len(), 1);
}

#[tokio::test]
async fn t02_remove_shared_layout_is_409_without_mutation() {
    let state = state();
    let room_a = Rooms::create_async(&state, "shared A").await.unwrap();
    let room_b = Rooms::create_async(&state, "shared B").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room_a, &layout).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room_b, &layout).await, StatusCode::NO_CONTENT);
    let before = state.api_v3.resources.lock().await.clone();
    let response = remove_global(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/video_layouts/{layout}")),
        Path(layout.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let after = state.api_v3.resources.lock().await.clone();
    assert_eq!(before.layouts, after.layouts);
    assert_eq!(before.links, after.links);
}

#[tokio::test]
async fn t02_detach_inherited_or_absent_layout_is_404() {
    let state = state();
    let room = Rooms::create_async(&state, "detach absent").await.unwrap();
    let layout = create_layout(
        &state,
        "Only conference",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    let response = detach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "inherited id нельзя снять из сессии как свой");
    let response = detach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let resources = state.api_v3.resources.lock().await;
    let key = format!("conferences:{}:video_layouts", room.id);
    assert!(resources.active_layouts.contains_key(&key), "после detach active должен указывать на default");
    assert!(resources.links[&key].iter().any(|id| resources.layouts[id]["payload"]["type"] == "default"));
}

#[tokio::test]
async fn t02_cow_validation_error_leaves_no_ghost() {
    let state = state();
    let room_a = Rooms::create_async(&state, "cow A").await.unwrap();
    let room_b = Rooms::create_async(&state, "cow B").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room_a, &layout).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room_b, &layout).await, StatusCode::NO_CONTENT);
    let before = state.api_v3.resources.lock().await.clone();
    let response = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout}", room_a.id)),
        Path((room_a.id.to_string(), layout.clone())),
        Json(json!({"payload":{"slots":"not-an-array"}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let after = state.api_v3.resources.lock().await.clone();
    assert_eq!(
        serde_json::to_string(&before).unwrap(),
        serde_json::to_string(&after).unwrap(),
        "422 не должен менять документ ни на байт"
    );
}

#[tokio::test]
async fn t02_cow_from_inherited_session_keeps_conference_original() {
    let state = state();
    let room = Rooms::create_async(&state, "inherited COW").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    let response = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Session copy"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let edited_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    assert_ne!(edited_id, layout, "inherited layout должен клонироваться в session");
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.layouts[&layout]["display_name"], "Shared", "оригинал конференции не изменён");
    let session_key = format!("sessions:{}:video_layouts", room.id);
    assert!(resources.links[&session_key].contains(&edited_id), "клон должен быть привязан к session");
    assert_eq!(resources.active_layouts[&session_key], edited_id);
}

#[tokio::test]
async fn t02_edit_global_shared_is_409() {
    let state = state();
    let room_a = Rooms::create_async(&state, "global A").await.unwrap();
    let room_b = Rooms::create_async(&state, "global B").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room_a, &layout).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room_b, &layout).await, StatusCode::NO_CONTENT);
    let response = edit_global(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/video_layouts/{layout}")),
        Path(layout.clone()),
        Json(json!({"display_name":"Changed"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn t02_mode_switch_on_shared_layout_copies_on_write() {
    let state = state();
    let room_a = Rooms::create_async(&state, "mode A").await.unwrap();
    let room_b = Rooms::create_async(&state, "mode B").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared default",
        json!({"type":"default","mode":"auto","preset":"preset_default_13","size":13}),
    )
    .await;
    assert_eq!(attach_to(&state, &room_a, &layout).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room_b, &layout).await, StatusCode::NO_CONTENT);
    let response = crate::api_v3::shell::switch_video_layout_mode(
        State(state.clone()),
        Extension(operator()),
        Json(crate::api_v3::shell::SwitchVideoLayoutModeRequest {
            session_id: Some(room_a.id.to_string()),
            conference_id: None,
            mode: Some("manual".into()),
            video_layout_id: Some(layout.clone()),
            extra: serde_json::Map::new(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cloned = body(response).await;
    let cloned_id = cloned["id"].as_str().expect("layout id in mode response").to_string();
    assert_ne!(cloned_id, layout, "mode на shared-раскладке должен сделать COW-клон");
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.layouts[&layout]["payload"]["mode"], "auto", "оригинал не изменён");
    assert_eq!(resources.layouts[&cloned_id]["payload"]["mode"], "manual");
    let session_key = format!("sessions:{}:video_layouts", room_a.id);
    assert_eq!(resources.active_layouts[&session_key], cloned_id);
    let other_key = format!("conferences:{}:video_layouts", room_b.id);
    assert_eq!(resources.links[&other_key], vec![layout], "другая комната осталась на оригинале");
}

#[tokio::test]
async fn t02_remove_default_is_409_and_single_layout_removal_restores_default() {
    let state = state();
    let room = Rooms::create_async(&state, "remove rules").await.unwrap();
    let shared = create_layout(&state, "Common", json!({"type":"default"})).await;
    assert_eq!(attach_to(&state, &room, &shared).await, StatusCode::NO_CONTENT);
    let response = remove_global(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/video_layouts/{shared}")),
        Path(shared.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT, "default нельзя удалить, пока он привязан");

    let manual = create_layout(
        &state,
        "Manual",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &manual).await, StatusCode::NO_CONTENT);
    let response = remove_global(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/video_layouts/{manual}")),
        Path(manual.clone()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let resources = state.api_v3.resources.lock().await;
    assert!(!resources.layouts.contains_key(&manual));
    let key = format!("conferences:{}:video_layouts", room.id);
    assert!(resources.active_layouts.contains_key(&key));
    assert!(resources.links[&key].iter().any(|id| resources.layouts[id]["payload"]["type"] == "default"));
}

#[tokio::test]
async fn t02_cow_alias_resolves_repeated_old_id_without_clone_storm() {
    let state = state();
    let room = Rooms::create_async(&state, "alias room").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    let first = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"First copy"})),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first_id = body(first).await["video_layout"]["id"].as_str().unwrap().to_string();
    let count_before = state.api_v3.resources.lock().await.layouts.len();
    // UI продолжает присылать старый id — alias должен вести в клон, не плодя новые.
    let second = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Second edit"})),
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_id = body(second).await["video_layout"]["id"].as_str().unwrap().to_string();
    assert_eq!(second_id, first_id);
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.layouts.len(), count_before, "alias не должен создавать второй клон");
    assert_eq!(resources.layouts[&first_id]["display_name"], "Second edit");
}

#[tokio::test]
async fn t02_read_path_resolves_old_id_to_cow_clone_and_list_has_no_duplicate() {
    let state = state();
    let room = Rooms::create_async(&state, "read alias").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    {
        let mut resources = state.api_v3.resources.lock().await;
        inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "tester");
    }
    let response = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Session copy"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let clone_id = body(response).await["video_layout"]["id"].as_str().unwrap().to_string();
    let response = get_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "GET по старому id после COW не должен 404");
    assert_eq!(body(response).await["video_layout"]["id"], clone_id);

    let response = list_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts", room.id)),
        Path(room.id.to_string()),
        Query(std::collections::HashMap::new()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let listed = body(response).await;
    let ids: Vec<&str> =
        listed["video_layouts"].as_array().unwrap().iter().filter_map(|item| item["id"].as_str()).collect();
    assert_eq!(ids.iter().filter(|id| **id == clone_id).count(), 1);
    assert!(!ids.contains(&layout.as_str()), "оригинал не должен дублироваться в списке сессии");
}

#[tokio::test]
async fn t02_detach_clone_does_not_leave_alias_404() {
    let state = state();
    let room = Rooms::create_async(&state, "detach clone alias").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    {
        let mut resources = state.api_v3.resources.lock().await;
        inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "tester");
    }
    let edited = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Clone"})),
    )
    .await;
    assert_eq!(edited.status(), StatusCode::OK);
    let clone_id = body(edited).await["video_layout"]["id"].as_str().unwrap().to_string();
    let detached = detach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{clone_id}", room.id)),
        Path((room.id.to_string(), clone_id.clone())),
    )
    .await;
    assert_eq!(detached.status(), StatusCode::NO_CONTENT);
    let got = get_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(got.status(), StatusCode::OK, "после detach клона старый id должен снова открывать оригинал");
    assert_eq!(body(got).await["video_layout"]["id"], layout);
}

#[tokio::test]
async fn t02_conference_cow_updates_live_session_mirror_without_duplicate() {
    let state = state();
    let room = Rooms::create_async(&state, "conference cow mirror").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    {
        let mut resources = state.api_v3.resources.lock().await;
        inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "tester");
    }
    let edited = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Conference clone"})),
    )
    .await;
    assert_eq!(edited.status(), StatusCode::OK);
    let clone_id = body(edited).await["video_layout"]["id"].as_str().unwrap().to_string();
    {
        let resources = state.api_v3.resources.lock().await;
        let session_key = format!("sessions:{}:video_layouts", room.id);
        assert!(resources.links[&session_key].contains(&clone_id), "mirror сессии перешёл на клон");
        assert!(!resources.links[&session_key].contains(&layout), "stale original убран из mirror");
        assert!(resources.links[&session_key].contains(&resources.active_layouts[&session_key]));
    }
    let listed = list_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts", room.id)),
        Path(room.id.to_string()),
        Query(std::collections::HashMap::new()),
    )
    .await;
    let listed = body(listed).await;
    let ids: Vec<&str> =
        listed["video_layouts"].as_array().unwrap().iter().filter_map(|item| item["id"].as_str()).collect();
    assert!(ids.contains(&clone_id.as_str()), "клон виден");
    assert!(!ids.contains(&layout.as_str()), "в списке сессии не должно быть stale оригинала");
    let got = get_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(got.status(), StatusCode::OK);
    assert_eq!(body(got).await["video_layout"]["id"], clone_id);
}

#[tokio::test]
async fn t02_inherit_uses_conference_active_for_first_bridge() {
    let mut resources = ResourceState::default();
    let manual = normalize_resource(
        "video_layouts",
        &json!({"display_name":"Manual","payload":{"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}}),
        "tester",
    );
    let manual_id = manual["id"].as_str().unwrap().to_string();
    resources.layouts.insert(manual_id.clone(), manual);
    let room = "11111111-1111-4111-8111-111111111111";
    let conference_key = format!("conferences:{room}:video_layouts");
    resources.links.insert(conference_key.clone(), vec![manual_id.clone()]);
    resources.active_layouts.insert(conference_key, manual_id.clone());
    inherit_and_ensure_default_layout(&mut resources, room, "tester");
    let session_key = format!("sessions:{room}:video_layouts");
    assert_eq!(resources.active_layouts[&session_key], manual_id, "T01: active конференции должен наследоваться");
}

#[tokio::test]
async fn t02_normalize_legacy_picks_last_non_default() {
    let mut resources = ResourceState::default();
    let default = normalize_resource("video_layouts", &json!({"payload":{"type":"default"}}), "tester");
    let default_id = default["id"].as_str().unwrap().to_string();
    let manual = normalize_resource(
        "video_layouts",
        &json!({"payload":{"type":"individual","mode":"manual","slots":[]}}),
        "tester",
    );
    let manual_id = manual["id"].as_str().unwrap().to_string();
    resources.layouts.insert(default_id.clone(), default);
    resources.layouts.insert(manual_id.clone(), manual);
    let key = "conferences:22222222-2222-4222-8222-222222222222:video_layouts";
    resources.links.insert(key.to_string(), vec![default_id, manual_id.clone()]);
    normalize_loaded(&mut resources);
    assert_eq!(resources.active_layouts[key], manual_id, "миграция без active = T01-effective (последняя non-default)");
}

#[tokio::test]
async fn t02_detach_inactive_layout_keeps_active() {
    let state = state();
    let room = Rooms::create_async(&state, "detach inactive").await.unwrap();
    let a =
        create_layout(&state, "A", json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}))
            .await;
    let b =
        create_layout(&state, "B", json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}))
            .await;
    assert_eq!(attach_to(&state, &room, &a).await, StatusCode::NO_CONTENT);
    assert_eq!(attach_to(&state, &room, &b).await, StatusCode::NO_CONTENT);
    let key = format!("conferences:{}:video_layouts", room.id);
    assert_eq!(state.api_v3.resources.lock().await.active_layouts[&key], b);
    let response = detach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{a}", room.id)),
        Path((room.id.to_string(), a)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.active_layouts[&key], b, "detach неактивной не должен трогать active");
}

#[tokio::test]
async fn t02_conference_alias_survives_detach_of_mirrored_clone_in_session() {
    let state = state();
    let room = Rooms::create_async(&state, "alias scope cleanup").await.unwrap();
    let layout = create_layout(
        &state,
        "Shared",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &layout).await, StatusCode::NO_CONTENT);
    {
        let mut resources = state.api_v3.resources.lock().await;
        inherit_and_ensure_default_layout(&mut resources, &room.id.to_string(), "tester");
    }
    let edited = edit_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
        Json(json!({"display_name":"Conference clone"})),
    )
    .await;
    let clone_id = body(edited).await["video_layout"]["id"].as_str().unwrap().to_string();
    // Detach клона в session не должен сносить conference-alias.
    let detached = detach(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/sessions/{}/video_layouts/{clone_id}", room.id)),
        Path((room.id.to_string(), clone_id.clone())),
    )
    .await;
    assert_eq!(detached.status(), StatusCode::NO_CONTENT);
    let got = get_linked(
        State(state.clone()),
        Extension(operator()),
        uri(&format!("/api/v3/conferences/{}/video_layouts/{layout}", room.id)),
        Path((room.id.to_string(), layout.clone())),
    )
    .await;
    assert_eq!(got.status(), StatusCode::OK, "conference alias старого id должен сохраниться");
    assert_eq!(body(got).await["video_layout"]["id"], clone_id);
}

#[tokio::test]
async fn t02_start_session_pushes_conference_active_to_engine() {
    let dialer = Arc::new(RecordingLayoutDialer::default());
    let mut app = AppState::new().with_dialer(dialer.clone());
    app.storage = Some(Arc::new(vks_storage::memory::MemoryStorage::new()));
    let state: SharedState = Arc::new(app);
    let room = Rooms::create_async(&state, "first bridge active").await.unwrap();
    let manual = create_layout(
        &state,
        "Manual",
        json!({"type":"individual","mode":"manual","slots":[{"position":1,"role":"AUTO"}]}),
    )
    .await;
    assert_eq!(attach_to(&state, &room, &manual).await, StatusCode::NO_CONTENT);
    let response = crate::api_v3::sessions::start_session(
        State(state.clone()),
        Extension(operator()),
        None,
        Json(crate::api_v3::sessions::StartSessionRequest { conference_id: Some(room.id.to_string()) }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let applied = dialer.layouts.lock().unwrap().clone();
    assert!(
        applied.iter().any(|(room_id, layout_id)| room_id == &room.id.to_string() && layout_id == &manual),
        "на первый мост должна уйти active раскладка конференции, получено {applied:?}"
    );
}

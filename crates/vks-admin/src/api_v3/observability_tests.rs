//! T2.4: наблюдаемость/сервис — log_events/export, сеть, utils, терминалы,
//! видеозаписи; read-only RBAC и явные отказы на мутации.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use crate::api_v3::tests::{app, json_body, login_token, send, test_state};
use crate::users::Users;
use vks_core::Role;

fn auth_request(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn role_token(issuer: &std::sync::Arc<vks_auth::jwt::JwtIssuer>, state: &crate::state::AppState, role: Role) -> String {
    let user = Users::create(state, format!("t24-{}", role.as_str()), role);
    issuer.issue(&user.id.to_string(), None).unwrap()
}

#[tokio::test]
async fn log_events_paginate_filter_search_and_export() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    state.record_audit_entry("admin", "login_ok", None, "10.0.0.1");
    state.record_audit_entry("alice", "create_user", None, "10.0.0.2");
    state.record_audit_entry("admin", "create_room", Some("room-1".into()), "10.0.0.1");

    let body = json_body(send(&app, auth_request("GET", "/api/v3/log_events?page=1&page_size=2", &token)).await).await;
    // 3 записи с vendor-шаблонами (create_room не имеет шаблона и скрыт)
    // + login_ok, записанный самим логином.
    assert_eq!(body["count"], 3);
    assert_eq!(body["log_events"].as_array().unwrap().len(), 2);
    let event = &body["log_events"][0];
    assert!(event["id"].is_number() && event["event_type"].is_string());
    assert_eq!(event["object_type"], "users");
    assert!(event["payload"]["ip"].is_string());

    let filtered = json_body(
        send(
            &app,
            auth_request(
                "GET",
                "/api/v3/log_events?event_types=login_success,creating_credentials_success&search=alice",
                &token,
            ),
        )
        .await,
    )
    .await;
    assert_eq!(filtered["count"], 1, "{filtered}");
    assert_eq!(filtered["log_events"][0]["event_type"], "creating_credentials_success");

    let future =
        json_body(send(&app, auth_request("GET", "/api/v3/log_events?date_from=9999999999", &token)).await).await;
    assert_eq!(future["count"], 0);

    let export = json_body(send(&app, auth_request("GET", "/api/v3/log_events/export", &token)).await).await;
    let filename = export["filename"].as_str().unwrap().to_string();
    assert!(filename.starts_with("log_events_") && filename.ends_with(".zip"));
    let download = app
        .clone()
        .oneshot(Request::builder().uri(format!("/static/log_events/{filename}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(download.headers()[header::CONTENT_TYPE], "application/zip");
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert!(bytes.windows(b"login_success".len()).any(|window| window == b"login_success"));
    let missing = app
        .clone()
        .oneshot(Request::builder().uri("/static/log_events/nope.zip").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn log_events_rbac_requires_operator() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = role_token(&issuer, &state, Role::Viewer);
    let response = send(&app, auth_request("GET", "/api/v3/log_events", &token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn network_interfaces_are_real_and_read_only() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let body = json_body(send(&app, auth_request("GET", "/api/v3/system/network/interfaces", &token)).await).await;
    assert!(body["count"].as_u64().unwrap_or(0) >= 1, "host must have a non-loopback interface: {body}");
    let interface = body["interfaces"][0]["interface"].as_str().unwrap().to_string();
    assert!(body["interfaces"][0]["addresses"][0]["ipaddress"].is_string());
    assert!(body["interfaces"][0]["mac"].is_string());
    let detail = json_body(
        send(&app, auth_request("GET", &format!("/api/v3/system/network/interfaces/{interface}"), &token)).await,
    )
    .await;
    assert!(detail["interface"]["addresses"].is_array());
    // Изменение сети не применяется — явный 501, а не мнимый успех.
    let refused =
        send(&app, auth_request("PATCH", &format!("/api/v3/system/network/interfaces/{interface}"), &token)).await;
    assert_eq!(refused.status(), StatusCode::NOT_IMPLEMENTED);
    let unknown = send(&app, auth_request("GET", "/api/v3/system/network/interfaces/eth404", &token)).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn utils_ping_tcpdump_and_terminals_are_honest() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let ping = send(&app, auth_request("GET", "/api/v3/system/utils/ping?address=127.0.0.1", &token)).await;
    assert_eq!(ping.status(), StatusCode::OK);
    let ping = json_body(ping).await;
    assert!(ping.get("stdout").is_some() && ping.get("stderr").is_some(), "{ping}");
    let invalid = send(&app, auth_request("GET", "/api/v3/system/utils/ping?address=;rm%20-rf", &token)).await;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let status = json_body(send(&app, auth_request("GET", "/api/v3/system/utils/tcpdumps/status", &token)).await).await;
    assert_eq!(status["running"], false);
    let files = json_body(send(&app, auth_request("GET", "/api/v3/system/utils/tcpdumps", &token)).await).await;
    assert_eq!(files["files"], json!([]));
    let start = send(&app, auth_request("POST", "/api/v3/system/utils/tcpdumps", &token)).await;
    assert_eq!(start.status(), StatusCode::NOT_IMPLEMENTED);
    let body = json_body(start).await;
    assert!(body["error"]["description"].as_str().unwrap().contains("not supported"));

    let terminals = json_body(send(&app, auth_request("GET", "/api/v3/terminals", &token)).await).await;
    assert_eq!(terminals["count"], 0);
    assert_eq!(terminals["terminals"], json!([]));
    let credentials = send(&app, auth_request("GET", "/api/v3/terminals/credentials", &token)).await;
    assert_eq!(credentials.status(), StatusCode::NOT_FOUND);
    let create = send(&app, auth_request("POST", "/api/v3/terminals", &token)).await;
    assert_eq!(create.status(), StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn videorecords_map_real_records_and_refuse_delete() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    {
        let mut records = state.records.lock().unwrap();
        records.insert(
            "rec-1".into(),
            crate::state::RecordEntry {
                id: "rec-1".into(),
                room_id: vks_core::RoomId::new(),
                created_ms: 1_800_000_000_000,
                duration_sec: 120,
                size_bytes: 10_485_760,
            },
        );
    }
    let body = json_body(send(&app, auth_request("GET", "/api/v3/videorecords", &token)).await).await;
    assert_eq!(body["count"], 1);
    let record = &body["videorecords"][0];
    assert_eq!(record["id"], "rec-1");
    assert_eq!(record["started_at"], 1_800_000_000);
    assert_eq!(record["payload"]["meta"]["size"], 10_485_760);
    let estimated = json_body(
        send(&app, auth_request("GET", "/api/v3/conferences/room/videorecords/estimated_time?quality=high", &token))
            .await,
    )
    .await;
    assert!(estimated["recording_time"].is_number());
    let refused = send(&app, auth_request("DELETE", "/api/v3/videorecords/rec-1", &token)).await;
    assert_eq!(refused.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(json_body(refused).await["error"]["code"], 501);
}

#[tokio::test]
async fn lower_role_cannot_read_network_or_terminals() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    for role in [Role::Moderator, Role::Participant] {
        let token = role_token(&issuer, &state, role);
        for uri in ["/api/v3/system/network/interfaces", "/api/v3/system/utils/tcpdumps/status", "/api/v3/terminals"] {
            assert_eq!(
                send(&app, auth_request("GET", uri, &token)).await.status(),
                StatusCode::FORBIDDEN,
                "{role:?} {uri}"
            );
        }
    }
}

#[tokio::test]
async fn system_stats_are_real_and_operator_only() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let memory = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/memory", &token)).await).await;
    assert!(memory["total"].as_u64().unwrap_or(0) > 0, "{memory}");
    assert!(memory["used"].as_u64().unwrap() <= memory["total"].as_u64().unwrap());
    let cpu = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/cpu", &token)).await).await;
    assert!(cpu["count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(cpu["load"].as_array().unwrap().len(), cpu["count"].as_u64().unwrap() as usize);
    assert!(cpu["load"][0]["idle"].as_f64().is_some());
    let disk = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/disk", &token)).await).await;
    assert!(disk["count"].as_u64().unwrap_or(0) >= 1, "{disk}");
    assert!(disk["spaces"][0]["total"].as_u64().unwrap() > 0);
    let network = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/network", &token)).await).await;
    assert!(network["count"].as_u64().unwrap_or(0) >= 1, "{network}");
    let packages = json_body(send(&app, auth_request("GET", "/api/v3/system/packages", &token)).await).await;
    assert_eq!(packages["packages"], json!([]));
    let engine = json_body(send(&app, auth_request("GET", "/api/v3/engine/log", &token)).await).await;
    assert_eq!(engine["log"].as_array().unwrap().len(), 1);
    assert!(engine["bounds"]["id"]["max"].as_u64().unwrap() >= 1);

    let viewer = role_token(&issuer, &state, Role::Viewer);
    assert_eq!(
        send(&app, auth_request("GET", "/api/v3/system/stats/memory", &viewer)).await.status(),
        StatusCode::FORBIDDEN
    );
}

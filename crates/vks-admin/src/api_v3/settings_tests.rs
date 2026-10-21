//! T2.3: административные настройки PNM — vendor-контракт GET/PATCH,
//! multipart TLS, SMTP test/reset, NTP/time/timezone, RBAC и персистентность.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};

use crate::api_v3::tests::{app, json_body, login_token, send, test_state};
use crate::users::Users;
use vks_core::Role;

fn login(issuer: &std::sync::Arc<vks_auth::jwt::JwtIssuer>, state: &crate::state::AppState, role: Role) -> String {
    let user = Users::create(state, format!("t23-{}", role.as_str()), role);
    issuer.issue(&user.id.to_string(), None).unwrap()
}

fn auth_request(method: &str, uri: &str, token: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn multipart_request(uri: &str, token: &str, boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Request<Body> {
    let mut body = Vec::new();
    for (name, filename, bytes) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match filename {
            Some(filename) => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                    .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn payload_settings_get_defaults_and_patch_persist() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let token = token.as_str();

    let sip = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls/sip", token, json!({}))).await).await;
    assert_eq!(sip["payload"]["sip_port"], 5060);
    assert_eq!(sip["payload"]["srtp"], "DISABLED");
    let patched = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/calls/sip",
            token,
            json!({"payload": {"sip_port": 5070, "srtp": "ENABLED", "sip_username": "mcu"}}),
        ),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK, "{:?}", json_body(patched).await);
    let sip = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls/sip", token, json!({}))).await).await;
    assert_eq!(sip["payload"]["sip_port"], 5070);
    assert_eq!(sip["payload"]["srtp"], "ENABLED");
    assert_eq!(sip["payload"]["sip_username"], "mcu");
    assert_eq!(sip["payload"]["sip_server"], "", "непереданные поля сохраняются");

    for (uri, field, value) in [
        ("/api/v3/settings/calls/h323", "h323_id", json!("mcu-1")),
        ("/api/v3/settings/rtp", "mtu", json!(1400)),
        ("/api/v3/settings/security", "password_min_length", json!(12)),
        ("/api/v3/settings/styles", "video_background", json!("BLACK")),
    ] {
        let response = send(&app, auth_request("PATCH", uri, token, json!({"payload": {field: value}}))).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}: {:?}", json_body(response).await);
        let body = json_body(send(&app, auth_request("GET", uri, token, json!({}))).await).await;
        assert_eq!(body["payload"][field], value, "{uri}");
    }

    // Вложенные styles сохраняются, чужие поля не теряются.
    let styles = json_body(send(&app, auth_request("GET", "/api/v3/settings/styles", token, json!({}))).await).await;
    assert_eq!(styles["payload"]["font"]["style_font_family"], "DEFAULT");
    assert_eq!(styles["payload"]["border"]["args"]["thickness"], 10);
}

#[tokio::test]
async fn payload_settings_validate_schema_and_rbac() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let admin = login_token(&app, "admin", "s3cret").await;

    let unknown =
        send(&app, auth_request("PATCH", "/api/v3/settings/rtp", &admin, json!({"payload": {"nope": 1}}))).await;
    assert_eq!(unknown.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let wrong_type =
        send(&app, auth_request("PATCH", "/api/v3/settings/rtp", &admin, json!({"payload": {"mtu": "big"}}))).await;
    assert_eq!(wrong_type.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let unknown_nested =
        send(&app, auth_request("PATCH", "/api/v3/settings/styles", &admin, json!({"payload": {"font": {"nope": 1}}})))
            .await;
    assert_eq!(unknown_nested.status(), StatusCode::UNPROCESSABLE_ENTITY);

    for role in [Role::Viewer, Role::Participant, Role::Moderator] {
        let token = login(&issuer, &state, role);
        let response = send(&app, auth_request("GET", "/api/v3/settings/calls/sip", &token, json!({}))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "role={role:?}");
    }
    let _ = issuer;
}

#[tokio::test]
async fn calls_patch_keeps_vendor_fields_and_legacy_media() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let source = json!({"payload": {
        "mcu_display_name": "MCU T23", "enabled_protocol": "SIP", "auto_gain_ctrl": true,
        "stop_rules": {"empty": 300},
        "codecs": {"audio": ["G722"], "video": ["H264"]}
    }});
    let response = send(&app, auth_request("PATCH", "/api/v3/settings/calls", &token, source)).await;
    assert_eq!(response.status(), StatusCode::OK, "{:?}", json_body(response).await);
    let body = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls", &token, json!({}))).await).await;
    assert_eq!(body["payload"]["mcu_display_name"], "MCU T23");
    assert_eq!(body["payload"]["enabled_protocol"], "SIP");
    assert_eq!(body["payload"]["auto_gain_ctrl"], true);
    assert_eq!(body["payload"]["stop_rules"]["empty"], 300);
    assert_eq!(body["payload"]["codecs"]["audio"][0], "G722");
}

#[tokio::test]
async fn smtp_password_is_masked_and_preserved_on_empty_patch() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let response = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/smtp",
            &token,
            json!({"payload": {"smtp_server": "smtp.local", "smtp_password": "secret", "smtp_sender_email": "a@b.c"}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(send(&app, auth_request("GET", "/api/v3/settings/smtp", &token, json!({}))).await).await;
    assert_eq!(body["payload"]["smtp_password"], "", "пароль не отдаётся");
    assert_eq!(body["payload"]["smtp_server"], "smtp.local");

    // Пустой пароль из формы не затирает сохранённый.
    let response =
        send(&app, auth_request("PATCH", "/api/v3/settings/smtp", &token, json!({"payload": {"smtp_password": ""}})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let stored = state.api_v3.resources.lock().await.admin_settings.get("smtp").cloned().unwrap();
    assert_eq!(stored["smtp_password"], "secret");
}

#[tokio::test]
async fn smtp_test_reports_configuration_and_connectivity() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let empty = Request::builder()
        .method("PUT")
        .uri("/api/v3/smtp/test")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"smtp_server": "", "smtp_port": 25}).to_string()))
        .unwrap();
    assert_eq!(send(&app, empty).await.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Живой TCP-листенер: проверка реально подключается.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = json!({"smtp_server": "127.0.0.1", "smtp_port": port});
    let request = Request::builder()
        .method("PUT")
        .uri("/api/v3/smtp/test")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK, "{:?}", json_body(response).await);

    let reset = send(&app, auth_request("POST", "/api/v3/smtp/reset", &token, json!({}))).await;
    assert_eq!(reset.status(), StatusCode::OK);
    let body = json_body(send(&app, auth_request("GET", "/api/v3/settings/smtp", &token, json!({}))).await).await;
    assert_eq!(body["payload"]["smtp_server"], "");
}

#[tokio::test]
async fn http_settings_multipart_cert_upload_check_and_reset() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let uploaded = send(
        &app,
        multipart_request(
            "/api/v3/settings/http",
            &token,
            "BOUND",
            &[
                ("port", None, b"8443"),
                ("password", None, b"keypass"),
                ("crt", Some("server.crt"), b"-----BEGIN CERTIFICATE-----\nMIIB\n"),
                ("key", Some("server.key"), b"-----BEGIN PRIVATE KEY-----\nMIIB\n"),
            ],
        ),
    )
    .await;
    assert_eq!(uploaded.status(), StatusCode::OK, "{:?}", json_body(uploaded).await);
    let body = json_body(send(&app, auth_request("GET", "/api/v3/settings/http", &token, json!({}))).await).await;
    assert_eq!(body["port"], 8443);
    assert_eq!(body["has_password"], true);
    assert_eq!(body["crt"]["common_name"], "server.crt");

    // Vendor-UI проверяет каждый файл ОТДЕЛЬНО: одиночный crt валиден.
    let single_cert = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v3/settings/http/check")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B2")
            .body(Body::from(
                "--B2\r\nContent-Disposition: form-data; name=\"crt\"; filename=\"a.crt\"\r\n\r\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\r\n--B2--\r\n",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(single_cert.status(), StatusCode::OK, "{:?}", json_body(single_cert).await);
    // Не-PEM мусор отклоняется, а не «valid:true».
    let garbage = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v3/settings/http/check")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B4")
            .body(Body::from(
                "--B4\r\nContent-Disposition: form-data; name=\"crt\"; filename=\"a.crt\"\r\n\r\nnot a cert\r\n--B4--\r\n",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(garbage.status(), StatusCode::BAD_REQUEST);

    let checked = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v3/settings/http/check")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "multipart/form-data; boundary=B3")
            .body(Body::from(
                "--B3\r\nContent-Disposition: form-data; name=\"crt\"; filename=\"a.crt\"\r\n\r\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\r\n--B3\r\nContent-Disposition: form-data; name=\"key\"; filename=\"a.key\"\r\n\r\n-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\r\n--B3--\r\n",
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(checked.status(), StatusCode::OK);

    let reset = send(&app, auth_request("POST", "/api/v3/settings/http/reset", &token, json!({}))).await;
    assert_eq!(reset.status(), StatusCode::OK);
    let body = json_body(send(&app, auth_request("GET", "/api/v3/settings/http", &token, json!({}))).await).await;
    assert_eq!(body["port"], 443);
    assert_eq!(body["has_password"], false);
}

#[tokio::test]
async fn ntp_and_time_timezone_endpoints() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let zones = json_body(send(&app, auth_request("GET", "/api/v3/time-zones", &token, json!({}))).await).await;
    assert!(zones["time_zones"].as_array().unwrap().len() > 100);

    let ntp = json_body(send(&app, auth_request("GET", "/api/v3/system/ntp", &token, json!({}))).await).await;
    assert!(ntp["ntp"]["fallback"].is_array());
    let response = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/system/ntp",
            &token,
            json!({"payload": {"server": "pool.ntp.org", "poll_interval_min": 64, "poll_interval_max": 1024}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{:?}", json_body(response).await);
    let ntp = json_body(send(&app, auth_request("GET", "/api/v3/system/ntp", &token, json!({}))).await).await;
    assert_eq!(ntp["ntp"]["server"], "pool.ntp.org");
    assert_eq!(ntp["ntp"]["poll_interval_min"], 64);

    let enabled = send(&app, auth_request("POST", "/api/v3/system/ntp/enable", &token, json!({}))).await;
    assert_eq!(enabled.status(), StatusCode::OK);
    let time = json_body(send(&app, auth_request("GET", "/api/v3/system/time", &token, json!({}))).await).await;
    assert_eq!(time["ntp"], true);
    assert_eq!(time["ntp_synchronized"], true);
    let disabled = send(&app, auth_request("POST", "/api/v3/system/ntp/disable", &token, json!({}))).await;
    assert_eq!(disabled.status(), StatusCode::OK);
    let time = json_body(send(&app, auth_request("GET", "/api/v3/system/time", &token, json!({}))).await).await;
    assert_eq!(time["ntp"], false);

    // Таймзона: валидная меняет system/time и profile, неизвестная — 422.
    let bad =
        send(&app, auth_request("PATCH", "/api/v3/system/timezone", &token, json!({"timezone": "Mars/Olympus"}))).await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let ok = send(&app, auth_request("PATCH", "/api/v3/system/timezone", &token, json!({"timezone": "Europe/Moscow"})))
        .await;
    assert_eq!(ok.status(), StatusCode::OK, "{:?}", json_body(ok).await);
    let time = json_body(send(&app, auth_request("GET", "/api/v3/system/time", &token, json!({}))).await).await;
    assert_eq!(time["timezone"], "Europe/Moscow");
    let profile = json_body(send(&app, auth_request("GET", "/api/v3/profile", &token, json!({}))).await).await;
    assert_eq!(profile["user"]["time_zone_id"], "Europe/Moscow");

    // Время: логический сдвиг отражается в system/time.
    let target = crate::state::now_unix_ms() / 1000 + 3600;
    let patched = send(&app, auth_request("PATCH", "/api/v3/system/time", &token, json!({"time": target}))).await;
    assert_eq!(patched.status(), StatusCode::OK);
    let time = json_body(send(&app, auth_request("GET", "/api/v3/system/time", &token, json!({}))).await).await;
    let actual = time["time"]["time"].as_i64().unwrap();
    assert!((actual - target as i64).abs() <= 5, "time={actual} target={target}");
}

#[tokio::test]
async fn settings_survive_restart() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());
    let mut state = crate::state::AppState::new();
    state.storage = Some(storage.clone());
    let state = std::sync::Arc::new(state);
    {
        let mut resources = state.api_v3.resources.lock().await;
        resources.admin_settings.insert("rtp".to_string(), json!({"mtu": 1300}));
        crate::api_v3::resources::persist(&state, &resources).await.expect("persist");
    }

    let mut restarted = crate::state::AppState::new();
    restarted.storage = Some(storage);
    restarted.sync_from_storage().await.expect("hydrate");
    assert_eq!(restarted.api_v3.resources.lock().await.admin_settings["rtp"]["mtu"], 1300);
}

#[tokio::test]
async fn vendor_ui_shapes_for_ntp_time_and_stop_rules_are_accepted() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    // 1) NTP: vendor шлёт сервер строкой и fallback/intervals соседними полями.
    let response = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/system/ntp",
            &token,
            json!({"ntp":"pool.ntp.org","fallback":["time.google.com"],"poll_interval_min":null,"poll_interval_max":null}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", json_body(response).await);
    let ntp = json_body(send(&app, auth_request("GET", "/api/v3/system/ntp", &token, json!({}))).await).await;
    assert_eq!(ntp["ntp"]["server"], "pool.ntp.org");
    assert_eq!(ntp["ntp"]["fallback"][0], "time.google.com");
    // Неверный тип при null-дефолте больше не принимается.
    let bad = send(&app, auth_request("PATCH", "/api/v3/system/ntp", &token, json!({"payload":{"server":123}}))).await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // И поле очищается обратно в null.
    let cleared =
        send(&app, auth_request("PATCH", "/api/v3/system/ntp", &token, json!({"payload":{"server":null}}))).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let ntp = json_body(send(&app, auth_request("GET", "/api/v3/system/ntp", &token, json!({}))).await).await;
    assert!(ntp["ntp"]["server"].is_null());

    // 2) date_time: локальная строка vendor "YYYY-MM-DD HH:mm:ss".
    let target = "2026-10-22 03:49:48";
    let response = send(&app, auth_request("PATCH", "/api/v3/system/time", &token, json!({"time": target}))).await;
    assert_eq!(response.status(), StatusCode::OK, "{}", json_body(response).await);
    let time = json_body(send(&app, auth_request("GET", "/api/v3/system/time", &token, json!({}))).await).await;
    use chrono::TimeZone;
    let zone: chrono_tz::Tz = time["time"]["timezone"].as_str().unwrap_or("UTC").parse().unwrap();
    let expected = zone
        .from_local_datetime(&chrono::NaiveDateTime::parse_from_str(target, "%Y-%m-%d %H:%M:%S").unwrap())
        .earliest()
        .unwrap()
        .timestamp();
    let actual = time["time"]["time"].as_i64().unwrap();
    assert!((actual - expected).abs() <= 5, "{time}");
    let _ = send(&app, auth_request("PATCH", "/api/v3/system/time", &token, json!({"time": 0}))).await;
    let _ = send(&app, auth_request("PATCH", "/api/v3/system/time", &token, json!({"payload":{"seconds":0}}))).await;

    // 3) stop_rules: null выключает любое правило, неверный тип отклоняется.
    let response = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/calls",
            &token,
            json!({"payload":{"stop_rules":{"empty":null,"quit_all":null,"one_abonent":60}}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{}", json_body(response).await);
    let calls = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls", &token, json!({}))).await).await;
    assert!(calls["payload"]["stop_rules"]["empty"].is_null());
    assert!(calls["payload"]["stop_rules"]["quit_all"].is_null());
    assert_eq!(calls["payload"]["stop_rules"]["one_abonent"], 60);
    let bad = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/calls",
            &token,
            json!({"payload":{"stop_rules":{"one_abonent":"soon"}}}),
        ),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let cleared = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/calls",
            &token,
            json!({"payload":{"stop_rules":{"empty":600,"quit_all":60,"one_abonent":null}}}),
        ),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let calls = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls", &token, json!({}))).await).await;
    assert!(calls["payload"]["stop_rules"]["one_abonent"].is_null());
}

#[tokio::test]
async fn rejected_calls_patch_does_not_touch_media_codecs_and_restart_services_is_200() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let before = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls", &token, json!({}))).await).await;
    let response = send(
        &app,
        auth_request(
            "PATCH",
            "/api/v3/settings/calls",
            &token,
            json!({"payload":{"codecs":{"audio":["G722"],"video":["H264"]},"vendor_unknown":1}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let after = json_body(send(&app, auth_request("GET", "/api/v3/settings/calls", &token, json!({}))).await).await;
    assert_eq!(after["payload"]["codecs"], before["payload"]["codecs"], "422 must be atomic");
    let restart = send(&app, auth_request("POST", "/api/v3/system/restart_services", &token, json!({}))).await;
    assert_eq!(restart.status(), StatusCode::OK);
    assert_eq!(json_body(restart).await["status"], "ok");
}

#[tokio::test]
async fn room_scoped_operator_cannot_change_global_video_settings() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = issuer.issue(&uuid::Uuid::new_v4().to_string(), Some("room-scope")).unwrap();
    let response = send(
        &app,
        auth_request("PATCH", "/api/v3/settings/video", &token, json!({"payload":{"rotate_timeout":21000}})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

//! T2.4: наблюдаемость/сервис — log_events/export, сеть, utils, терминалы,
//! видеозаписи; read-only RBAC и явные отказы на мутации.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

use crate::api_v3::tests::{app, json_body, login_token, request, send, test_state};
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
    // Vendor-UI сохраняет интерфейс по короткому пути — он тоже должен
    // отвечать понятным 501, а не catch-all «Not implemented».
    let vendor_alias = send(&app, auth_request("PATCH", &format!("/api/v3/system/network/{interface}"), &token)).await;
    assert_eq!(vendor_alias.status(), StatusCode::NOT_IMPLEMENTED);
    let alias_body = json_body(vendor_alias).await;
    assert!(alias_body["error"]["description"].as_str().unwrap().contains("Network interface"));
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
    let room_id = vks_core::RoomId::new();
    {
        let mut records = state.records.lock().unwrap();
        records.insert(
            "rec-1".into(),
            crate::state::RecordEntry {
                bitrate_kbps: 2350,
                width: 1280,
                height: 720,
                id: "rec-1".into(),
                room_id,
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
    // session-алиас фильтрует по id из пути, а не отдаёт все записи.
    let own =
        json_body(send(&app, auth_request("GET", &format!("/api/v3/sessions/{room_id}/videorecords"), &token)).await)
            .await;
    assert_eq!(own["count"], 1, "{own}");
    let foreign = json_body(
        send(&app, auth_request("GET", &format!("/api/v3/sessions/{}/videorecords", vks_core::RoomId::new()), &token))
            .await,
    )
    .await;
    assert_eq!(foreign["count"], 0, "{foreign}");
    let refused = send(&app, auth_request("DELETE", "/api/v3/videorecords/rec-1", &token)).await;
    assert_eq!(refused.status(), StatusCode::NOT_IMPLEMENTED);
    assert_eq!(json_body(refused).await["error"]["code"], 501);
}

#[tokio::test]
async fn log_events_map_real_v3_directory_actions() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    for action in [
        "user.created",
        "user.updated",
        "user.deleted",
        "user.credentials.created",
        "user.credentials.updated",
        "user.credentials.deleted",
        "profile.credentials.updated",
    ] {
        state.record_audit_entry("admin", action, None, "10.0.0.9");
    }
    let body = json_body(
        send(
            &app,
            auth_request(
                "GET",
                "/api/v3/log_events?event_types=creating_credentials_success,updating_password_success,updating_login_success,deleting_credentials_success",
                &token,
            ),
        )
        .await,
    )
    .await;
    let types: Vec<&str> =
        body["log_events"].as_array().unwrap().iter().filter_map(|event| event["event_type"].as_str()).collect();
    assert!(types.contains(&"creating_credentials_success"), "{body}");
    assert!(types.contains(&"updating_password_success"), "{body}");
    assert!(types.contains(&"updating_login_success"), "{body}");
    assert!(types.contains(&"deleting_credentials_success"), "{body}");
    assert_eq!(types.len(), 7, "все семь v3-действий должны иметь шаблон: {body}");
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

#[tokio::test]
async fn stats_disk_is_megabytes_and_network_is_kbps() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let disk = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/disk", &token)).await).await;
    assert!(disk["count"].as_u64().unwrap() >= 1, "{disk}");
    let spaces = disk["spaces"].as_array().unwrap();
    let space = spaces
        .iter()
        .find(|space| space["name"] == "/")
        .unwrap_or_else(|| spaces.first().expect("at least one filesystem"));
    let total = space["total"].as_u64().unwrap();
    let used = space["used"].as_u64().unwrap();
    let free = space["free"].as_u64().unwrap();
    assert!(total > 0 && used <= total && free <= total, "{space}");
    // Значения — MB, а не KiB: сравниваем с df -Pk (KiB/1024).
    if let Ok(output) = std::process::Command::new("df").arg("-Pk").arg("/").output() {
        let text = String::from_utf8_lossy(&output.stdout);
        let target = space["name"].as_str().unwrap_or("/");
        if let Some(line) = text.lines().skip(1).find(|line| line.split_whitespace().nth(5) == Some(target)) {
            let kib: u64 = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
            if kib > 0 {
                let expected = kib / 1024;
                assert!(
                    total.abs_diff(expected) <= expected / 50 + 8,
                    "disk total={total} MB, expected ~{expected} MB ({line})"
                );
            }
        }
    }

    let network = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/network", &token)).await).await;
    assert!(network["count"].as_u64().unwrap() >= 1, "{network}");
    for interface in network["interfaces"].as_array().unwrap() {
        let rx = interface["rx"].as_u64().unwrap();
        let tx = interface["tx"].as_u64().unwrap();
        // kbps, а не накопительные байты: монотонные гигабайты здесь невозможны.
        assert!(rx < 100_000_000, "rx={rx} looks like cumulative bytes: {interface}");
        assert!(tx < 100_000_000, "tx={tx} looks like cumulative bytes: {interface}");
    }
}

#[tokio::test]
async fn log_export_csv_uses_local_time_and_invoker() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    state.record_audit_entry("admin", "login_ok", None, "10.9.8.7");
    // settings-событие, у которого нет payload.login — имя берём из invoker.
    state.record_audit_entry("operator", "settings.sip.updated", None, "10.9.8.8");

    let export = json_body(send(&app, auth_request("GET", "/api/v3/log_events/export", &token)).await).await;
    let filename = export["filename"].as_str().unwrap();
    // 128-битный токен, а не 32-битный предсказуемый суффикс.
    let suffix = filename.trim_end_matches(".zip").rsplit('_').next().unwrap();
    assert_eq!(suffix.len(), 32, "expected 128-bit token in {filename}");
    let download = app
        .clone()
        .oneshot(Request::builder().uri(format!("/static/log_events/{filename}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(download.into_body(), usize::MAX).await.unwrap();
    let csv = String::from_utf8_lossy(&bytes);
    assert!(csv.contains("operator"), "settings event must keep invoker: {csv}");
    let timezone = crate::api_v3::history::public_timezone(&state).await;
    let tz = timezone.parse::<chrono_tz::Tz>().expect("valid host timezone");
    let event = crate::api_v3::observability::collect_events(&state)
        .into_iter()
        .find(|event| event.event_type == "login_success")
        .expect("login event");
    let expected = chrono::DateTime::from_timestamp(event.created_at, 0)
        .unwrap()
        .with_timezone(&tz)
        .format("%d.%m.%Y %H:%M:%S")
        .to_string();
    assert!(csv.contains(&expected), "CSV must use local time {expected}: {csv}");
}

#[tokio::test]
async fn license_features_status_network_and_videorecord_alias_shapes() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let license = json_body(send(&app, auth_request("GET", "/api/v3/license", &token)).await).await;
    assert_eq!(license["state"], "VALID");
    assert_eq!(license["type"], "PERMANENT");
    assert!(license["last_expired"].as_i64().unwrap() > 4_000_000_000, "{license}");
    assert_eq!(license["last_update_failed"], false);
    assert_eq!(license["params"]["protocol"]["sip"], true);
    assert_eq!(license["params"]["server"]["max_conferences"], 4);

    let features = json_body(send(&app, auth_request("GET", "/api/v3/features", &token)).await).await;
    assert!(features["features"].as_array().is_some_and(|items| items.len() > 40), "{features}");

    let status = json_body(send(&app, auth_request("GET", "/api/v3/system/status/network", &token)).await).await;
    // editable=true обязателен: иначе vendor-UI показывает пустой список.
    assert_eq!(status["network"]["editable"], true);

    // Vendor-UI зовёт session-алиасы (оценка записи) — не 501.
    let estimated =
        send(&app, auth_request("GET", "/api/v3/sessions/any-id/videorecords/estimated_time?quality=high", &token))
            .await;
    assert_eq!(estimated.status(), StatusCode::OK, "{:?}", json_body(estimated).await);
    let list = send(&app, auth_request("GET", "/api/v3/sessions/any-id/videorecords", &token)).await;
    assert_eq!(list.status(), StatusCode::OK);

    // Нижние роли не читают лицензию/фичи/диагностику/калибровку.
    let viewer = role_token(&issuer, &state, Role::Viewer);
    for uri in ["/api/v3/license", "/api/v3/features", "/api/v3/engine/diagnostics", "/api/v3/engine/calibration"] {
        let response = send(&app, auth_request("GET", uri, &viewer)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri} must be Operator-only");
    }
}

#[tokio::test]
async fn utilities_reject_option_like_addresses_and_missing_traceroute() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    for address in ["-V", "--help", "-c100000"] {
        let response =
            send(&app, auth_request("GET", &format!("/api/v3/system/utils/ping?address={address}"), &token)).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY, "{address}");
    }
    let traceroute = send(&app, auth_request("GET", "/api/v3/system/utils/traceroute?address=127.0.0.1", &token)).await;
    let installed = std::process::Command::new("sh")
        .args(["-c", "command -v traceroute >/dev/null 2>&1"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if installed {
        assert_eq!(traceroute.status(), StatusCode::OK);
    } else {
        // Честный 503, а не 200 с OS-ошибкой в stderr.
        assert_eq!(traceroute.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = json_body(traceroute).await;
        assert!(body["error"]["description"].as_str().unwrap().contains("not installed"));
    }
}

#[tokio::test]
async fn engine_log_has_stable_startup_timestamp() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let first = json_body(send(&app, auth_request("GET", "/api/v3/engine/log", &token)).await).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let second = json_body(send(&app, auth_request("GET", "/api/v3/engine/log", &token)).await).await;
    assert_eq!(first["log"][0]["timestamp"], second["log"][0]["timestamp"], "startup record must be stable");
    assert_eq!(first["bounds"]["timestamp"]["min"], first["log"][0]["timestamp"]);
    // Это время старта процесса, а не первого запроса: не позже текущего и
    // не старше 24 часов (в тесте процесс живёт секунды).
    let now = crate::state::now_unix_ms() as i64 / 1000;
    let started = first["log"][0]["timestamp"].as_i64().unwrap();
    assert!(started <= now && started > now - 86_400, "started={started} now={now}");
}

#[tokio::test]
async fn statistics_aggregates_system_and_engine() {
    let (state, _issuer) = test_state();
    let room = crate::rooms::Rooms::create(&state, "stats-room");
    state.room_hub.register_participant(
        &room.id,
        crate::hub::ParticipantSnapshot {
            participant_id: "p1".to_string(),
            display_name: "P1".to_string(),
            is_recorder: false,
            is_speaking: false,
            track_ids: Vec::new(),
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: 0,
        },
    );
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let stats = json_body(send(&app, auth_request("GET", "/api/v3/statistics", &token)).await).await;
    assert!(stats["timestamp"].as_i64().unwrap() > 0, "{stats}");
    assert!(stats["system"]["cpu"]["load"].is_array(), "{stats}");
    assert!(stats["system"]["cpu"]["count"].as_u64().unwrap() >= 1, "{stats}");
    assert!(stats["system"]["memory"]["total"].as_u64().unwrap() > 0, "{stats}");
    assert!(stats["system"]["disk"]["spaces"].is_array(), "{stats}");
    assert!(stats["system"]["network"]["interfaces"].is_array(), "{stats}");
    assert_eq!(stats["system"]["media_healthy"], true, "{stats}");
    assert_eq!(stats["engine"]["rooms"], 1, "{stats}");
    assert_eq!(stats["engine"]["participants"], 1, "{stats}");
    assert!(stats["engine"]["mode"].is_string(), "{stats}");
    assert!(stats["engine"]["codecs"]["audio"].as_array().is_some_and(|codecs| !codecs.is_empty()), "{stats}");
    assert!(stats["engine"]["codecs"]["video"].as_array().is_some_and(|codecs| !codecs.is_empty()), "{stats}");
}

#[tokio::test]
async fn stats_cpu_network_have_no_sleep_and_memory_has_timestamp() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    // Два подряд вызова каждого эндпоинта: со sleep(120ms)+sleep(200ms) это
    // заняло бы >=640 мс; без sleep — единицы миллисекунд.
    let started = std::time::Instant::now();
    for _ in 0..2 {
        let cpu = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/cpu", &token)).await).await;
        assert_eq!(
            cpu["count"].as_u64().unwrap_or(0) as usize,
            cpu["load"].as_array().unwrap().len(),
            "cpu shape unchanged: {cpu}"
        );
        let network = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/network", &token)).await).await;
        assert_eq!(
            network["count"].as_u64().unwrap() as usize,
            network["interfaces"].as_array().unwrap().len(),
            "network shape unchanged: {network}"
        );
    }
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "stats handlers must not sleep: {:?}",
        started.elapsed()
    );

    let memory = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/memory", &token)).await).await;
    assert!(memory["total"].as_u64().unwrap() > 0, "{memory}");
    assert!(memory["timestamp"].as_i64().unwrap() > 0, "memory must carry timestamp: {memory}");
    assert!(memory["used"].as_u64().unwrap() <= memory["total"].as_u64().unwrap());

    let disk = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/disk", &token)).await).await;
    assert!(disk["count"].as_u64().unwrap_or(0) >= 1, "{disk}");
    assert!(disk["spaces"][0]["total"].as_u64().unwrap() > 0, "{disk}");
}

#[tokio::test]
async fn stats_disk_prefers_real_filesystems_and_root_first() {
    let (state, _issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    let disk = json_body(send(&app, auth_request("GET", "/api/v3/system/stats/disk", &token)).await).await;
    let spaces = disk["spaces"].as_array().unwrap_or_else(|| panic!("spaces missing: {disk}"));
    assert!(!spaces.is_empty(), "{disk}");
    let names: Vec<&str> = spaces.iter().filter_map(|space| space["name"].as_str()).collect();

    // UI берёт первый элемент как выбранный диск: это должен быть корень,
    // а не tmpfs (`/run`, `/dev/shm`, `/tmp`) — иначе виджет показывал «/run 11 Gb».
    assert_eq!(names[0], "/", "root filesystem must be first: {names:?}");
    for pseudo in ["/run", "/dev/shm", "/tmp"] {
        assert!(!names.contains(&pseudo), "pseudo filesystem {pseudo} leaked into disk list: {names:?}");
    }
    assert!(spaces[0]["total"].as_u64().unwrap_or(0) > 100, "root must be a real disk: {disk}");
    assert!(spaces[0]["used"].as_u64().unwrap_or(0) <= spaces[0]["total"].as_u64().unwrap_or(0), "{disk}");
}

// ---------- T09a: recording start/stop contract ----------

struct FakeRecording {
    h264_groups: usize,
    started: std::sync::Mutex<Vec<vks_core::RoomId>>,
    stopped: std::sync::Mutex<Vec<vks_core::RoomId>>,
}

impl FakeRecording {
    fn new(h264_groups: usize) -> Self {
        Self { h264_groups, started: std::sync::Mutex::new(Vec::new()), stopped: std::sync::Mutex::new(Vec::new()) }
    }
}

#[async_trait::async_trait]
impl crate::state::RecordingControl for FakeRecording {
    async fn start(&self, room_id: vks_core::RoomId) -> Result<String, String> {
        self.started.lock().unwrap().push(room_id);
        Ok(uuid::Uuid::new_v4().to_string())
    }

    async fn stop(&self, room_id: vks_core::RoomId) -> Result<(), String> {
        self.stopped.lock().unwrap().push(room_id);
        Ok(())
    }

    fn active_h264_groups(&self, _room_id: vks_core::RoomId) -> usize {
        self.h264_groups
    }
}

#[tokio::test]
async fn start_recording_without_h264_group_returns_409_and_creates_nothing() {
    let (state, _issuer) = test_state();
    state.mode.lock().unwrap().global_mode = vks_core::RoomMode::Mcu;
    let room = crate::rooms::Rooms::create_async(&state, "rec-409".to_string()).await.expect("room");
    let control = std::sync::Arc::new(FakeRecording::new(0));
    state.set_recording(control.clone());
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = json_body(response).await;
    assert_eq!(body["error"]["reason"], "no_active_h264_encoder_group", "{body}");
    assert!(control.started.lock().unwrap().is_empty(), "no recording may start without an encoder group");
    assert!(state.records.lock().unwrap().is_empty(), "409 must not create a record");
}

#[tokio::test]
async fn start_and_stop_recording_hit_the_control_port_once() {
    let (state, _issuer) = test_state();
    state.mode.lock().unwrap().global_mode = vks_core::RoomMode::Mcu;
    let room = crate::rooms::Rooms::create_async(&state, "rec-ok".to_string()).await.expect("room");
    let control = std::sync::Arc::new(FakeRecording::new(1));
    state.set_recording(control.clone());
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let started =
        send(&server, request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&token), json!({})))
            .await;
    assert_eq!(started.status(), StatusCode::OK);
    let body = json_body(started).await;
    let record_id = body["id"].as_str().expect("record id").to_string();
    assert!(uuid::Uuid::parse_str(&record_id).is_ok());
    assert_eq!(control.started.lock().unwrap().len(), 1);

    let stopped = send(
        &server,
        request("DELETE", &format!("/api/v3/sessions/{}/videorecords/{record_id}", room.id), Some(&token), json!({})),
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::OK);
    assert_eq!(control.stopped.lock().unwrap().as_slice(), &[*room.id.as_uuid()].map(vks_core::RoomId::from_uuid));
}

#[tokio::test]
async fn start_recording_in_sfu_room_returns_501_before_any_encoder_probe() {
    let (state, _issuer) = test_state();
    let room = crate::rooms::Rooms::create_async(&state, "rec-sfu".to_string()).await.expect("room");
    state.mode.lock().unwrap().global_mode = vks_core::RoomMode::Sfu;
    let control = std::sync::Arc::new(FakeRecording::new(1));
    state.set_recording(control.clone());
    let server = app(state.clone());
    let token = login_token(&server, "admin", "s3cret").await;
    let response =
        send(&server, request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    assert!(control.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recording_control_is_moderator_plus_and_unknown_session_is_404() {
    let (state, issuer) = test_state();
    state.mode.lock().unwrap().global_mode = vks_core::RoomMode::Mcu;
    let room = crate::rooms::Rooms::create_async(&state, "rec-rbac".to_string()).await.expect("room");
    state.set_recording(std::sync::Arc::new(FakeRecording::new(1)));
    let server = app(state.clone());
    let moderator = role_token(&issuer, &state, Role::Moderator);
    let started = send(
        &server,
        request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&moderator), json!({})),
    )
    .await;
    assert_eq!(started.status(), StatusCode::OK, "ManageRecording is Moderator+");
    let viewer = role_token(&issuer, &state, Role::Viewer);
    let forbidden =
        send(&server, request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&viewer), json!({})))
            .await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let missing = send(
        &server,
        request(
            "POST",
            &format!("/api/v3/sessions/{}/videorecords", uuid::Uuid::new_v4()),
            Some(&moderator),
            json!({}),
        ),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn recording_control_honours_the_token_room_scope() {
    let (state, issuer) = test_state();
    state.mode.lock().unwrap().global_mode = vks_core::RoomMode::Mcu;
    let room = crate::rooms::Rooms::create_async(&state, "rec-scope".to_string()).await.expect("room");
    state.set_recording(std::sync::Arc::new(FakeRecording::new(1)));
    let server = app(state.clone());
    let moderator = Users::create(&state, "t24-scope-moderator".to_string(), Role::Moderator);
    let foreign_scope = uuid::Uuid::new_v4().to_string();
    let scoped = issuer.issue(&moderator.id.to_string(), Some(&foreign_scope)).expect("token");

    let start =
        send(&server, request("POST", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&scoped), json!({})))
            .await;
    assert_eq!(start.status(), StatusCode::FORBIDDEN, "room-scoped token must not record a foreign room");
    let list = send(
        &server,
        request("GET", &format!("/api/v3/sessions/{}/videorecords", room.id), Some(&scoped), serde_json::Value::Null),
    )
    .await;
    assert_eq!(list.status(), StatusCode::FORBIDDEN, "read must honour the same scope");
    let stop = send(
        &server,
        request(
            "DELETE",
            &format!("/api/v3/sessions/{}/videorecords/{}", room.id, uuid::Uuid::new_v4()),
            Some(&scoped),
            json!({}),
        ),
    )
    .await;
    assert_eq!(stop.status(), StatusCode::FORBIDDEN);
    assert_eq!(state.records.lock().unwrap().len(), 0, "no row is created by a rejected request");
}

#[tokio::test]
async fn estimated_time_is_derived_from_the_bitrate_model_not_a_constant() {
    let (state, _issuer) = test_state();
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let body =
        json_body(send(&server, auth_request("GET", "/api/v3/sessions/any/videorecords/estimated_time", &token)).await)
            .await;
    let seconds = body["recording_time"].as_u64().expect("recording_time");
    assert_ne!(seconds, 3600, "must not be the old hardcoded constant: {body}");
    // 64..8192 kbps at 720p30 on any real disk gives far more than 1 hour.
    assert!(seconds == 0 || seconds > 3600, "{body}");
}

#[tokio::test]
async fn videorecords_report_the_stored_profile_instead_of_hardcoded_720p() {
    let (state, _issuer) = test_state();
    state.records.lock().unwrap().insert(
        "rec-1".into(),
        crate::state::RecordEntry {
            id: "rec-1".into(),
            room_id: vks_core::RoomId::new(),
            created_ms: 1_700_000_000_000,
            duration_sec: 61,
            size_bytes: 12_345_678,
            bitrate_kbps: 2350,
            width: 1920,
            height: 1080,
        },
    );
    let server = app(state);
    let token = login_token(&server, "admin", "s3cret").await;
    let body = json_body(send(&server, auth_request("GET", "/api/v3/videorecords", &token)).await).await;
    let record = &body["videorecords"][0];
    assert_eq!(record["payload"]["bitrate"], 2350, "{body}");
    assert_eq!(record["payload"]["resolution"], json!([1920, 1080]), "{body}");
    assert_eq!(record["payload"]["profile"], "1920x1080", "{body}");
    assert_eq!(record["stopped_at"].as_i64().unwrap() - record["started_at"].as_i64().unwrap(), 61);
}

#[tokio::test]
async fn videorecords_survive_sqlite_restart_and_hydrate_state_records() {
    use vks_core::ports::Storage;
    let path = std::env::temp_dir().join(format!("vks-t09a-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let record = crate::state::RecordEntry {
        id: "restart-rec-1".into(),
        room_id: vks_core::RoomId::new(),
        created_ms: 1_700_000_000_000,
        duration_sec: 90,
        size_bytes: 4_096,
        bitrate_kbps: 2350,
        width: 1280,
        height: 720,
    };
    {
        let storage = std::sync::Arc::new(vks_storage::SqliteStorage::init(&url).await.expect("init sqlite"));
        storage.upsert_record(&record).await.expect("persist record");
        let state = std::sync::Arc::new(crate::state::AppState::with_storage(storage));
        state.sync_from_storage().await.expect("cold-start hydration");
        let hydrated = state.records.lock().unwrap().get("restart-rec-1").cloned().expect("hydrated record");
        assert_eq!(hydrated.room_id, record.room_id);
        assert_eq!(hydrated.duration_sec, 90);
    }
    // "Restart": a brand-new storage instance on the same file + new AppState.
    let storage = std::sync::Arc::new(vks_storage::SqliteStorage::init(&url).await.expect("reopen sqlite"));
    let state = std::sync::Arc::new(crate::state::AppState::with_storage(storage));
    state.sync_from_storage().await.expect("restart hydration");
    let record_after = state.records.lock().unwrap().get("restart-rec-1").cloned().expect("record survives restart");
    assert_eq!(record_after.size_bytes, 4_096);
    assert_eq!((record_after.width, record_after.height), (1280, 720));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn hot_reload_does_not_resurrect_deleted_records() {
    use vks_core::ports::Storage;
    let path = std::env::temp_dir().join(format!("vks-t09a-hot-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let storage = std::sync::Arc::new(vks_storage::SqliteStorage::init(&url).await.expect("init sqlite"));
    storage
        .upsert_record(&crate::state::RecordEntry {
            id: "hot-rec-1".into(),
            room_id: vks_core::RoomId::new(),
            created_ms: 1,
            duration_sec: 1,
            size_bytes: 1,
            bitrate_kbps: 2350,
            width: 1280,
            height: 720,
        })
        .await
        .expect("persist");
    let state = std::sync::Arc::new(crate::state::AppState::with_storage(storage));
    state.sync_from_storage().await.expect("cold start");
    state.records.lock().unwrap().remove("hot-rec-1");
    state.hot_reload_from_storage().await.expect("hot reload");
    assert!(
        state.records.lock().unwrap().get("hot-rec-1").is_none(),
        "hot reload must not resurrect a record deleted in this process"
    );
    let _ = std::fs::remove_file(path);
}

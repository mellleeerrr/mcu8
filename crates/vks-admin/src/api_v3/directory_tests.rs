//! HTTP/WS тесты справочника PNM (T2.1): users CRUD, группы, роли,
//! учётные записи и личные адресные книги — через полный роутер `/api/v3`.

use axum::http::StatusCode;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{ws_users_search, ADMIN_ROLE_ID, OPERATOR_ROLE_ID};
use crate::api_v3::resources::ResourceState;
use crate::api_v3::tests::{app, json_body, login_token, request, send, test_state};
use crate::users::Users;
use vks_core::ports::storage::Storage;
use vks_core::Role;

async fn created_user(app: &axum::Router, token: &str, display_name: &str) -> String {
    let response = send(
        app,
        request("POST", "/api/v3/users", Some(token), json!({"display_name": display_name, "emails": ["a@b.c"]})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["user"]["display_name"], display_name);
    assert_eq!(body["user"]["emails"][0], "a@b.c");
    body["user"]["id"].as_str().expect("user id").to_string()
}

async fn created_group(app: &axum::Router, token: &str, name: &str) -> String {
    let response = send(app, request("POST", "/api/v3/groups", Some(token), json!({"display_name": name}))).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["group"]["display_name"], name);
    body["group"]["id"].as_str().expect("group id").to_string()
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn user_body(app: &axum::Router, token: &str, id: &str) -> Value {
    let response = send(app, request("GET", &format!("/api/v3/users/{id}"), Some(token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn users_crud_groups_roles_and_credentials() {
    let (state, issuer) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;

    // --- users CRUD ---
    let id = created_user(&app, &token, "Иван Петров").await;
    let body = user_body(&app, &token, &id).await;
    assert_eq!(body["user"]["display_name"], "Иван Петров");
    assert!(body["user"]["roles"].as_array().is_some());

    let response = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/users/{id}"),
            Some(&token),
            json!({"display_name": "Иван Сидоров", "phones": ["+7 900 000-00-00"]}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["user"]["display_name"], "Иван Сидоров");
    assert_eq!(body["user"]["phones"][0], "+7 900 000-00-00");

    let response = send(&app, request("GET", "/api/v3/users?search=Сидоров", Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert_eq!(body["count"], 1, "поиск по display_name: {body}");

    // --- группы ---
    let group_id = created_group(&app, &token, "Группа 1").await;
    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = user_body(&app, &token, &id).await;
    assert_eq!(body["user"]["groups"][0]["id"].as_str(), Some(group_id.as_str()));
    assert_eq!(body["user"]["groups"][0]["size"], 1);

    let response = send(&app, request("GET", &format!("/api/v3/groups/{group_id}"), Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert_eq!(body["group"]["size"], 1);

    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{id}/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, request("GET", &format!("/api/v3/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["group"]["size"], 0);

    // --- роли: выдача/снятие Operator меняет внутреннюю роль ---
    let response = send(&app, request("GET", "/api/v3/roles", Some(&token), json!({}))).await;
    let roles = json_body(response).await;
    assert_eq!(roles["count"], 2);
    assert!(roles["roles"].as_array().unwrap().iter().any(|r| r["id"] == ADMIN_ROLE_ID));
    assert!(roles["roles"].as_array().unwrap().iter().any(|r| r["id"] == OPERATOR_ROLE_ID));

    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/roles/{OPERATOR_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = user_body(&app, &token, &id).await;
    assert!(body["user"]["roles"].as_array().unwrap().iter().any(|r| r["id"] == OPERATOR_ROLE_ID));
    assert_eq!(crate::users::Users::resolve_role(&state, &id), Some(Role::Moderator));
    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{id}/roles/{OPERATOR_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(crate::users::Users::resolve_role(&state, &id), Some(Role::Participant));

    // Administrator назначается, но снять его нельзя (vendor `Undeletable`)
    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/roles/{ADMIN_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(crate::users::Users::resolve_role(&state, &id), Some(Role::Operator));
    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{id}/roles/{ADMIN_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(crate::users::Users::resolve_role(&state, &id), Some(Role::Operator));

    // --- учётные записи ---
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/users/{id}/credentials"),
            Some(&token),
            json!({"login": "ivan", "password": "secret1"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(json_body(response).await["login"], "ivan");

    let response =
        send(&app, request("GET", &format!("/api/v3/users/{id}/credentials"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["login"], "ivan");

    // дубликат логина отклоняется
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/users/{id}/credentials"),
            Some(&token),
            json!({"login": "ivan", "password": "secret2"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let response = send(
        &app,
        request("PATCH", &format!("/api/v3/users/{id}/credentials"), Some(&token), json!({"password": "secret3"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // созданный логин реально аутентифицируется
    let response =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "ivan", "password": "secret3"}))).await;
    assert_eq!(response.status(), StatusCode::OK, "login созданным логином: {:?}", json_body(response).await);
    let ivan_token = {
        let response =
            send(&app, request("POST", "/api/v3/login", None, json!({"login": "ivan", "password": "secret3"}))).await;
        json_body(response).await["token"].as_str().unwrap().to_string()
    };
    // собственная смена пароля
    let response = send(
        &app,
        request(
            "PATCH",
            "/api/v3/profile/credentials",
            Some(&ivan_token),
            json!({"login": null, "confirm_password": "secret3", "new_password": "secret4"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "ivan", "password": "secret4"}))).await;
    assert_eq!(response.status(), StatusCode::OK, "новый пароль должен работать");
    let response =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "ivan", "password": "secret3"}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "старый пароль должен быть отклонён");

    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{id}/credentials"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&app, request("GET", &format!("/api/v3/users/{id}/credentials"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // --- удаление пользователя ---
    let response = send(&app, request("DELETE", &format!("/api/v3/users/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, request("GET", &format!("/api/v3/users/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // --- группа удаляется и исчезает из списка ---
    let response = send(&app, request("DELETE", &format!("/api/v3/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, request("GET", "/api/v3/groups", Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert!(!body["groups"].as_array().unwrap().iter().any(|g| g["id"] == group_id));
    let _ = issuer;
}

#[tokio::test]
async fn user_address_book_attach_create_detach() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Адресный пользователь").await;

    // глобальный адрес
    let response = send(
        &app,
        request(
            "POST",
            "/api/v3/call_strings",
            Some(&token),
            json!({"uri": "sip:user@example.com", "display_name": "Пользователь SIP"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let address_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();

    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/call_strings/{address_id}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&app, request("GET", &format!("/api/v3/users/{id}/call_strings"), Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["call_strings"][0]["uri"], "sip:user@example.com");

    // создание адреса сразу в личной книге
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/ui/users/{id}/call_strings"),
            Some(&token),
            json!({"uri": "h323:192.168.1.50", "display_name": "Терминал"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let personal_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();
    let response =
        send(&app, request("GET", &format!("/api/v3/users/{id}/call_strings"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["count"], 2);

    let response = send(
        &app,
        request("DELETE", &format!("/api/v3/users/{id}/call_strings/{personal_id}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &app,
        request("DELETE", &format!("/api/v3/users/{id}/call_strings/{personal_id}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // глобальная книга не затронута отвязкой
    let response =
        send(&app, request("GET", &format!("/api/v3/call_strings/{address_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(&app, request("DELETE", &format!("/api/v3/users/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn directory_rejects_non_operator_and_guards_builtin_admin() {
    let (state, issuer) = test_state();
    let viewer = Users::create(&state, "viewer", Role::Viewer);
    let viewer_token = issuer.issue(&viewer.id.to_string(), None).expect("issue viewer");
    let app = app(state.clone());
    let admin_token = login_token(&app, "admin", "s3cret").await;

    for (method, uri) in [("GET", "/api/v3/users"), ("POST", "/api/v3/groups"), ("GET", "/api/v3/roles")] {
        let response = send(&app, request(method, uri, Some(&viewer_token), json!({"display_name": "x"}))).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri} must be Operator-only");
    }

    // bootstrap-администратор неудаляем
    let builtin = vks_core::UserId::parse(vks_core::BOOTSTRAP_ADMIN_USER_ID).expect("uuid");
    Users::create_with_id_async(&state, builtin, "root", Role::Operator).await.expect("builtin user");
    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{builtin}"), Some(&admin_token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{builtin}/credentials"), Some(&admin_token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // WS ui:users:search: Operator получает список, Viewer — ошибку
    let admin_user = Users::list(&state).into_iter().find(|u| u.role == Role::Operator).unwrap();
    let result = ws_users_search(&state, &admin_user.id.to_string(), None, &json!({"search": "viewer"}))
        .await
        .expect("operator search");
    assert_eq!(result["count"], 1);
    assert_eq!(result["users"][0]["display_name"], "viewer");
    let denied = ws_users_search(&state, &viewer.id.to_string(), None, &json!({})).await;
    assert_eq!(denied, Err((403, "Forbidden: insufficient permissions")));
}

#[tokio::test]
async fn roles_list_filters_by_user() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Роль-фильтр").await;

    let response = send(&app, request("GET", &format!("/api/v3/roles?user_id={id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["count"], 0, "у нового пользователя нет ролей");

    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/roles/{OPERATOR_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, request("GET", &format!("/api/v3/roles?user_id={id}"), Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert_eq!(body["count"], 1);
    assert_eq!(body["roles"][0]["id"], OPERATOR_ROLE_ID);
}

#[test]
fn directory_persists_in_resource_configuration_document() {
    let mut state = ResourceState::default();
    state.directory.users.insert(
        "u-1".to_string(),
        super::DirectoryUser {
            display_name: "Иван".to_string(),
            roles: vec![ADMIN_ROLE_ID.to_string()],
            ..Default::default()
        },
    );
    state.directory.groups.insert(
        "g-1".to_string(),
        super::DirectoryGroup { display_name: "Группа".to_string(), created_at: 1, creator_id: None },
    );
    state.directory.user_addresses.entry("u-1".to_string()).or_default().insert(
        "a-1".to_string(),
        crate::api_v3::resources::AddressRecord {
            id: "a-1".to_string(),
            uri: "sip:user@example.com".to_string(),
            display_name: "Адрес".to_string(),
            created_at: 1,
            payload: serde_json::json!({}),
        },
    );
    let json = serde_json::to_string(&state).expect("serialize");
    assert!(json.contains("\"directory\""));
    let restored: ResourceState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(restored.directory.users["u-1"].display_name, "Иван");
    assert_eq!(restored.directory.groups["g-1"].display_name, "Группа");
    assert_eq!(restored.directory.user_addresses["u-1"]["a-1"].uri, "sip:user@example.com");
}

#[tokio::test]
async fn delete_user_cascades_credentials_memberships_and_profile() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Удаляемый").await;
    let group_id = created_group(&app, &token, "Группа удаления").await;
    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/users/{id}/credentials"),
            Some(&token),
            json!({"login": "deleteme", "password": "secret1"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = send(&app, request("DELETE", &format!("/api/v3/users/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    // учётные данные удалены: login невозможен
    let response =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "deleteme", "password": "secret1"}))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "deleted user must not authenticate");
    // профиль и членства удалены: размер группы 0
    let response = send(&app, request("GET", &format!("/api/v3/groups/{group_id}"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["group"]["size"], 0);
    // повторное удаление — 404
    let response = send(&app, request("DELETE", &format!("/api/v3/users/{id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deletable_flags_expose_ui_delete_but_not_self_or_bootstrap() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Удаляемы�� через UI").await;
    let body = user_body(&app, &token, &id).await;
    assert_eq!(body["user"]["defaults"]["deletable"], true, "обычного пользователя UI должен давать удалить");
    let admin_id = Users::list(&state).into_iter().find(|u| u.role == Role::Operator).unwrap().id.to_string();
    let admin_body = user_body(&app, &token, &admin_id).await;
    assert_eq!(admin_body["user"]["defaults"]["deletable"], false, "себя удалять нельзя");
    let response = send(&app, request("DELETE", &format!("/api/v3/users/{admin_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let builtin = vks_core::UserId::parse(vks_core::BOOTSTRAP_ADMIN_USER_ID).unwrap();
    Users::create_with_id_async(&state, builtin, "root", Role::Operator).await.unwrap();
    let builtin_body = user_body(&app, &token, &builtin.to_string()).await;
    assert_eq!(builtin_body["user"]["defaults"]["deletable"], false);
}

#[tokio::test]
async fn groups_use_vendor_display_name_filter_excludes_and_sort() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let user = created_user(&app, &token, "Фильтр групп").await;
    let alpha = created_group(&app, &token, "T21G Альфа").await;
    let beta = created_group(&app, &token, "T21G Бета").await;
    let response = send(
        &app,
        request("GET", &format!("/api/v3/groups?display_name={}", urlencoding("T21G Альфа")), Some(&token), json!({})),
    )
    .await;
    let body = json_body(response).await;
    assert_eq!(body["count"], 1, "vendor-параметр display_name: {body}");
    assert_eq!(body["groups"][0]["id"].as_str(), Some(alpha.as_str()));

    // exclude_users: группы, где пользователь уже состоит, не предлагаются
    let response =
        send(&app, request("POST", &format!("/api/v3/users/{user}/groups/{alpha}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&app, request("GET", &format!("/api/v3/groups?exclude_users={user}"), Some(&token), json!({}))).await;
    let body = json_body(response).await;
    assert!(body["groups"].as_array().unwrap().iter().all(|g| g["id"] != alpha));
    assert!(body["groups"].as_array().unwrap().iter().any(|g| g["id"] == beta));

    // сортировка по убыванию
    let response =
        send(&app, request("GET", "/api/v3/groups?sort_field=display_name&sort_order=1", Some(&token), json!({})))
            .await;
    let body = json_body(response).await;
    let names: Vec<&str> =
        body["groups"].as_array().unwrap().iter().filter_map(|g| g["display_name"].as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.to_lowercase()));
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn personal_address_can_be_edited_and_rejects_invalid_uri() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Адрес-редактирование").await;
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/ui/users/{id}/call_strings"),
            Some(&token),
            json!({"uri": "sip:1.2.3.4", "display_name": "Старый"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let address_id = json_body(response).await["call_string"]["id"].as_str().unwrap().to_string();
    let response = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/ui/users/{id}/call_strings/{address_id}"),
            Some(&token),
            json!({"uri": "h323:5.6.7.8", "display_name": "Новый"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["call_string"]["display_name"], "Новый");
    assert_eq!(body["call_string"]["uri"], "h323:5.6.7.8");
    let response = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/ui/users/{id}/call_strings/{address_id}"),
            Some(&token),
            json!({"uri": "sip: "}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/ui/users/{id}/call_strings/unknown"),
            Some(&token),
            json!({"display_name": "x"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn moderator_reads_directory_but_room_scope_is_denied() {
    let (state, issuer) = test_state();
    let moderator = Users::create(&state, "dir-moderator", Role::Moderator);
    let moderator_token = issuer.issue(&moderator.id.to_string(), None).unwrap();
    let room_token = issuer.issue(&moderator.id.to_string(), Some("11111111-1111-1111-1111-111111111111")).unwrap();
    let app = app(state.clone());
    let response = send(&app, request("GET", "/api/v3/users", Some(&moderator_token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK, "Moderator читает справочник");
    let result = ws_users_search(&state, &moderator.id.to_string(), None, &json!({})).await;
    assert!(result.is_ok(), "WS ui:users:search доступен Moderator");
    let response = send(&app, request("GET", "/api/v3/users", Some(&room_token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN, "room-scoped токен не читает глобальный справочник");
    let denied = ws_users_search(&state, &moderator.id.to_string(), Some("room"), &json!({})).await;
    assert_eq!(denied, Err((403, "Forbidden: insufficient permissions")));
}

#[tokio::test]
async fn role_crud_and_implicit_administrator_role_are_preserved() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let admin_id = Users::list(&state).into_iter().find(|u| u.role == Role::Operator).unwrap().id.to_string();

    // PATCH профиля создаёт профиль с пустыми roles; добавление Operator не
    // должно понижать Administrator.
    let response =
        send(&app, request("PATCH", &format!("/api/v3/users/{admin_id}"), Some(&token), json!({"description": "x"})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(
        &app,
        request("POST", &format!("/api/v3/users/{admin_id}/roles/{OPERATOR_ROLE_ID}"), Some(&token), json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(Users::resolve_role(&state, &admin_id), Some(Role::Operator), "Administrator не понижается");

    // создание/редактирование/удаление кастомной роли
    let response = send(
        &app,
        request("POST", "/api/v3/roles", Some(&token), json!({"display_name": "Наблюдатель", "description": "read"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let role_id = json_body(response).await["role"]["id"].as_str().unwrap().to_string();
    let response = send(&app, request("GET", "/api/v3/roles", Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["count"], 3);
    let response = send(&app, request("GET", &format!("/api/v3/roles/{role_id}"), Some(&token), json!({}))).await;
    let role = json_body(response).await;
    assert!(role["role"]["permissions"].as_array().is_some_and(|p| !p.is_empty()));

    let user = created_user(&app, &token, "Носитель роли").await;
    let response =
        send(&app, request("POST", &format!("/api/v3/users/{user}/roles/{role_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, request("GET", &format!("/api/v3/roles?user_id={user}"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["count"], 1);

    let response = send(&app, request("DELETE", &format!("/api/v3/roles/{role_id}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response =
        send(&app, request("DELETE", &format!("/api/v3/roles/{ADMIN_ROLE_ID}"), Some(&token), json!({}))).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = send(&app, request("GET", &format!("/api/v3/roles?user_id={user}"), Some(&token), json!({}))).await;
    assert_eq!(json_body(response).await["count"], 0, "удалённая роль снята с пользователя");
}

#[tokio::test]
async fn bootstrap_rename_keeps_local_login_for_password_change() {
    let (state, _) = test_state();
    let app = app(state.clone());
    let token = login_token(&app, "admin", "s3cret").await;
    let admin_id = Users::list(&state).into_iter().find(|u| u.role == Role::Operator).unwrap().id.to_string();
    let response = send(
        &app,
        request("PATCH", &format!("/api/v3/users/{admin_id}"), Some(&token), json!({"display_name": "Root"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // логин в LocalDb не переименован, смена пароля по-прежнему работает
    let response = send(
        &app,
        request(
            "PATCH",
            "/api/v3/profile/credentials",
            Some(&token),
            json!({"login": null, "old_password": "s3cret", "new_password": "s3cret2"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "self password change via vendor-UI body");
    let response =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "admin", "password": "s3cret2"}))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let fresh_token = json_body(response).await["token"].as_str().unwrap().to_string();
    let response = send(&app, request("GET", "/api/v3/profile", Some(&fresh_token), json!({}))).await;
    assert_eq!(json_body(response).await["user"]["display_name"], "Root");
}

#[tokio::test]
async fn validation_lengths_unicode_duplicates_and_json_envelope() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Длины").await;
    let long_user = "я".repeat(300);
    let response =
        send(&app, request("PATCH", &format!("/api/v3/users/{id}"), Some(&token), json!({"display_name": long_user})))
            .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let long_group = "г".repeat(129);
    let response =
        send(&app, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": long_group}))).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response =
        send(&app, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "Дубль Ёж"}))).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response =
        send(&app, request("POST", "/api/v3/groups", Some(&token), json!({"display_name": "дубль ёж"}))).await;
    assert_eq!(response.status(), StatusCode::CONFLICT, "Unicode-регистр не должен обходить дубликат");

    // malformed JSON → единый конверт ошибки, не plain-text axum
    let raw = axum::http::Request::builder()
        .method("PATCH")
        .uri(format!("/api/v3/users/{id}"))
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"))
        .body(axum::body::Body::from("{not json"))
        .unwrap();
    let response = app.clone().oneshot(raw).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    assert!(body["error"]["code"].as_i64().is_some(), "error envelope: {body}");
}

#[tokio::test]
async fn directory_hydrates_from_configuration_document_on_restart() {
    let storage = std::sync::Arc::new(vks_storage::MemoryStorage::new());
    storage
        .put_configuration_document(
            crate::api_v3::resources::CONFIGURATION_KEY,
            &serde_json::json!({
                "resources": {
                    "layouts": {},
                    "audiochannels": {},
                    "links": {},
                    "schedules": {},
                    "contacts": {},
                    "addresses": {},
                    "directory": {
                        "users": {"u-1": {"display_name": "Гидратация", "roles": [ADMIN_ROLE_ID]}},
                        "groups": {"g-1": {"display_name": "Группа рестарта"}},
                        "roles": {"r-1": {"display_name": "Кастом", "permissions": ["users:read"]}},
                        "user_addresses": {"u-1": {"a-1": {"id": "a-1", "uri": "sip:a@b", "display_name": "Адрес", "created_at": 1}}}
                    }
                }
            })
            .to_string(),
        )
        .await
        .unwrap();
    let mut state = crate::state::AppState::new();
    state.storage = Some(storage);
    state.sync_from_storage().await.expect("hydrate");
    let resources = state.api_v3.resources.lock().await;
    assert_eq!(resources.directory.users["u-1"].display_name, "Гидратация");
    assert_eq!(resources.directory.groups["g-1"].display_name, "Группа рестарта");
    assert_eq!(resources.directory.roles["r-1"].display_name, "Кастом");
    assert_eq!(resources.directory.user_addresses["u-1"]["a-1"].uri, "sip:a@b");
}

#[tokio::test]
async fn credentials_login_can_be_renamed_by_admin_and_self() {
    let (state, _) = test_state();
    let app = app(state);
    let token = login_token(&app, "admin", "s3cret").await;
    let id = created_user(&app, &token, "Переименование логина").await;
    let response = send(
        &app,
        request(
            "POST",
            &format!("/api/v3/users/{id}/credentials"),
            Some(&token),
            json!({"login": "login-a", "password": "secret1"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = send(
        &app,
        request(
            "PATCH",
            &format!("/api/v3/users/{id}/credentials"),
            Some(&token),
            json!({"login": "login-b", "password": "secret2"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{:?}", json_body(response).await);
    let old =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "login-a", "password": "secret2"}))).await;
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    let new =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "login-b", "password": "secret2"}))).await;
    assert_eq!(new.status(), StatusCode::OK);
    let self_token = json_body(new).await["token"].as_str().unwrap().to_string();
    let response = send(
        &app,
        request(
            "PATCH",
            "/api/v3/profile/credentials",
            Some(&self_token),
            json!({"login": "login-c", "old_password": "secret2"}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "{:?}", json_body(response).await);
    let renamed =
        send(&app, request("POST", "/api/v3/login", None, json!({"login": "login-c", "password": "secret2"}))).await;
    assert_eq!(renamed.status(), StatusCode::OK);
}

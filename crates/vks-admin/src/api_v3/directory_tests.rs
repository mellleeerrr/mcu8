//! HTTP/WS тесты справочника PNM (T2.1): users CRUD, группы, роли,
//! учётные записи и личные адресные книги — через полный роутер `/api/v3`.

use axum::http::StatusCode;
use serde_json::{json, Value};

use super::{ws_users_search, ADMIN_ROLE_ID, OPERATOR_ROLE_ID};
use crate::api_v3::resources::ResourceState;
use crate::api_v3::tests::{app, json_body, login_token, request, send, test_state};
use crate::users::Users;
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

    // --- роли: выдача/снятие Administrator меняет и внутреннюю роль ---
    let response = send(&app, request("GET", "/api/v3/roles", Some(&token), json!({}))).await;
    let roles = json_body(response).await;
    assert_eq!(roles["count"], 2);
    assert!(roles["roles"].as_array().unwrap().iter().any(|r| r["id"] == ADMIN_ROLE_ID));
    assert!(roles["roles"].as_array().unwrap().iter().any(|r| r["id"] == OPERATOR_ROLE_ID));

    let response =
        send(&app, request("POST", &format!("/api/v3/users/{id}/roles/{ADMIN_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = user_body(&app, &token, &id).await;
    assert!(body["user"]["roles"].as_array().unwrap().iter().any(|r| r["id"] == ADMIN_ROLE_ID));
    let role = crate::users::Users::resolve_role(&state, &id);
    assert_eq!(role, Some(Role::Operator));

    let response =
        send(&app, request("DELETE", &format!("/api/v3/users/{id}/roles/{ADMIN_ROLE_ID}"), Some(&token), json!({})))
            .await;
    assert_eq!(response.status(), StatusCode::OK);
    let role = crate::users::Users::resolve_role(&state, &id);
    assert_eq!(role, Some(Role::Participant));

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
            json!({"confirm_password": "secret3", "new_password": "secret4"}),
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
    let result = ws_users_search(&state, &admin_user.id.to_string(), &json!({"search": "viewer"}))
        .await
        .expect("operator search");
    assert_eq!(result["count"], 1);
    assert_eq!(result["users"][0]["display_name"], "viewer");
    let denied = ws_users_search(&state, &viewer.id.to_string(), &json!({})).await;
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

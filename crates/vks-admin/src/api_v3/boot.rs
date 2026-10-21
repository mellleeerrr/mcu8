//! Boot-эндпоинты (§4.5 плана): вызываются UI до/во время логина, **без токена**
//! (exempt-list §4.3: `login|login/status|system/about|about|references`).
//!
//! `GET /api/v3/system/about` обязателен: без него UI рисует полноэкранный оверлей.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};

use super::track;
use crate::settings::SettingsService;
use crate::state::SharedState;

/// `GET /api/v3/login/status` → `{"status":"enabled"}`.
pub async fn login_status() -> Response {
    track("login/status");
    Json(serde_json::json!({ "status": "enabled" })).into_response()
}

/// `GET /api/v3/system/about` → форма маппера `PW`
/// (`{node_id, server_name, versions:{server}, state:{status, payload:{package,backup,export}}}`).
pub async fn system_about(State(state): State<SharedState>) -> Response {
    track("system/about");
    let name = SettingsService::get(&state).name;
    Json(serde_json::json!({
        "node_id": name,
        "server_name": name,
        "versions": {
            "server": env!("CARGO_PKG_VERSION"),
            "installer": env!("CARGO_PKG_VERSION"),
            "ui": "2.2.1.66",
            "api": "2.2.1.66",
            "engine": "1.0.0",
            "sip/h323": "1.0.0",
            "vile": "1.0.0"
        },
        "state": { "status": "READY", "payload": { "package": "", "backup": "", "export": "" } }
    }))
    .into_response()
}

/// `GET /api/v3/about` — совместимость с exempt-списком интерцептора UI
/// (тот же конверт + `domain`/`serial` под маппер `Sk`).
pub async fn about(State(state): State<SharedState>) -> Response {
    track("about");
    let name = SettingsService::get(&state).name;
    Json(serde_json::json!({
        "node_id": name,
        "server_name": name,
        "domain": name,
        "serial": "",
        "versions": { "server": env!("CARGO_PKG_VERSION") },
        "state": { "status": "READY", "payload": { "package": "", "backup": "", "export": "" } }
    }))
    .into_response()
}

/// `GET /api/v3/references?language=…` → пустые строки (маппер `AW` мягкий).
pub async fn references() -> Response {
    track("references");
    // Эталон отдаёт реальные ссылки trueconf.com. Пустые строки ломали
    // «Помощь», «Купить» и «Продлить лицензию» (UI делал href="").
    Json(serde_json::json!({
        "key": "https://trueconf.com/products/mcu.html",
        "index": "https://trueconf.com/",
        "buy": "https://trueconf.com/buy.html",
        "contacts": "https://trueconf.com/contacts.html",
        "trial_full_version": "https://trueconf.com/download/mcu.html",
        "license_renewal": "https://trueconf.com/buy.html",
        "free_license_restrictions": "https://trueconf.com/products/mcu/free-license.html"
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn login_status_shape() {
        let resp = login_status().await;
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "enabled");
    }

    #[tokio::test]
    async fn references_shape() {
        let resp = references().await;
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        for key in
            ["key", "index", "buy", "contacts", "trial_full_version", "license_renewal", "free_license_restrictions"]
        {
            assert!(json.get(key).is_some(), "missing key {key}");
        }
    }
}

use axum::{
    extract::Path,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use uuid::Uuid;

fn decode_percent(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// `POST /api/v3/sessions/{id}/previews/{abonent_id}`
pub async fn start_preview(Path((session_id, target)): Path<(String, String)>) -> Response {
    crate::api_v3::track("sessions/{id}/previews");
    let unescaped_target = decode_percent(&target);
    let clean_target = crate::api_v3::sessions::sanitize_id(&unescaped_target).to_string();
    let unescaped_session = decode_percent(&session_id);
    let clean_session = crate::api_v3::sessions::sanitize_id(&unescaped_session).to_string();
    let stream_id = format!("preview-{}", Uuid::new_v4());

    (
        StatusCode::OK,
        Json(json!({
            "status": "started",
            "id": stream_id,
            "streamId": stream_id,
            "stream_id": stream_id,
            "preview_stream_id": stream_id,
            "previewStreamId": stream_id,
            "sessionId": clean_session,
            "session_id": clean_session,
            "abonentId": clean_target,
            "abonent_id": clean_target,
            "callStringId": clean_target,
            "call_string_id": clean_target,
            "addressId": clean_target,
            "address_id": clean_target,
            "preview": {
                "status": "started",
                "id": stream_id,
                "streamId": stream_id,
                "stream_id": stream_id,
                "preview_stream_id": stream_id,
                "previewStreamId": stream_id,
                "sessionId": clean_session,
                "session_id": clean_session,
                "abonentId": clean_target,
                "abonent_id": clean_target,
                "callStringId": clean_target,
                "call_string_id": clean_target,
                "addressId": clean_target,
                "address_id": clean_target,
                "resolution": { "width": 480, "height": 270, "fps_limit": 1 }
            }
        })),
    )
        .into_response()
}

/// `GET /api/v3/sessions/{id}/previews/{stream_id}`
pub async fn get_preview_frame(Path((_session_id, _stream_id)): Path<(String, String)>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=1"));
    (StatusCode::NO_CONTENT, headers).into_response()
}

/// `DELETE /api/v3/sessions/{id}/previews/{call_string_id}`
pub async fn stop_preview(Path((_session_id, _call_string_id)): Path<(String, String)>) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

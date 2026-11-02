use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::state::SharedState;

/// Активная preview-сессия параллельного плеера.
#[derive(Debug, Clone)]
pub struct PreviewStreamRecord {
    pub stream_id: String,
    pub session_id: String,
    pub target: String,
    pub preview_type: String,
    pub width: u16,
    pub height: u16,
    pub created_at: std::time::Instant,
    pub last_frame_time: Arc<AtomicU64>,
    pub frame_counter: Arc<AtomicU32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PreviewFrameQuery {
    pub key: Option<String>,
}

pub fn decode_percent(input: &str) -> String {
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

/// Вычисляет контрольную сумму Adler-32 (RFC 1950).
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Упаковывает сырые байты плоскости в RFC 1950 zlib-контейнер
/// с несжатыми RFC 1951 DEFLATE блоками (BTYPE=00, длина каждого блока ≤ 65535).
fn zlib_uncompressed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + (data.len() / 65535 + 1) * 5 + data.len() + 4);
    // Zlib header: CMF=0x78 (deflate, 32k window), FLG=0x01 (check bits)
    out.push(0x78);
    out.push(0x01);
    let mut offset = 0;
    while offset < data.len() || offset == 0 {
        let chunk_size = std::cmp::min(data.len() - offset, 65535);
        let is_last = (offset + chunk_size) >= data.len();
        // BFINAL (bit 0), BTYPE=00 (bits 1-2): 0x01 для последнего блока, 0x00 для промежуточных
        out.push(if is_last { 0x01 } else { 0x00 });
        let len = chunk_size as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&data[offset..offset + chunk_size]);
        offset += chunk_size;
        if offset >= data.len() {
            break;
        }
    }
    let checksum = adler32(data);
    out.extend_from_slice(&checksum.to_be_bytes());
    out
}

/// Удаляет неактивные preview-сессии (без опросов > 60 сек) для предотвращения утечки памяти.
fn prune_stale_streams(streams: &mut std::collections::HashMap<String, PreviewStreamRecord>, now_ms: u64) {
    if streams.len() > 64 {
        streams.retain(|_, s| {
            let last = s.last_frame_time.load(Ordering::Relaxed);
            last == 0 || now_ms.saturating_sub(last) < 60_000
        });
    }
    if streams.len() > 500 {
        let mut to_remove = Vec::new();
        for (k, s) in streams.iter() {
            let last = s.last_frame_time.load(Ordering::Relaxed);
            if now_ms.saturating_sub(last) > 10_000 {
                to_remove.push(k.clone());
            }
        }
        for k in to_remove {
            streams.remove(&k);
        }
    }
}

/// Генерирует валидный бинарный ERVC кадр с магическими байтами `0x9D, 0x56`
/// (TLV чанки: 2 Frame Info, 3 Plane Params, 6 Macroblock Info, 5 Plane Data, 1 End of Frame),
/// декодируемый фронтенд-плеером Yw/Ww на `<canvas>`.
pub fn generate_ervc_frame(w: u16, h: u16, frame_id: u8) -> Vec<u8> {
    let mut frame = Vec::new();
    // 1. Magic bytes
    frame.extend_from_slice(&[0x9D, 0x56]);

    // 2. Chunk 2: Frame Info (type 2, len 4)
    // format_version=1, frame_type=1 (intra/keyframe), frame_id, planes=3, ec_type=1
    frame.push(2);
    frame.extend_from_slice(&4u16.to_be_bytes());
    frame.push(1); // format_version = 1
    frame.push(1); // frame_type = 1 (intra)
    frame.push(frame_id);
    frame.push((3 << 4) | 1); // 0x31

    // YUV 4:2:0 размеры плоскостей
    let uw = w.div_ceil(2);
    let uh = h.div_ceil(2);
    let vw = w.div_ceil(2);
    let vh = h.div_ceil(2);

    // Y-плоскость с контрастной рамкой и фоном
    let mut y_plane = vec![90u8; (w as usize) * (h as usize)];
    for y in 0..(h as usize) {
        for x in 0..(w as usize) {
            if x < 4 || x >= (w as usize) - 4 || y < 4 || y >= (h as usize) - 4 {
                y_plane[y * (w as usize) + x] = 200;
            } else {
                let diag = ((x + y + (frame_id as usize) * 4) % 64) as u8;
                y_plane[y * (w as usize) + x] = 75 + diag;
            }
        }
    }
    let u_plane = vec![135u8; (uw as usize) * (uh as usize)];
    let v_plane = vec![122u8; (vw as usize) * (vh as usize)];

    let planes: [(u16, u16, &[u8]); 3] = [(w, h, &y_plane), (uw, uh, &u_plane), (vw, vh, &v_plane)];

    for (pw, ph, raw_data) in planes {
        // Chunk 3: Intra Plane Params (type 3, len 5)
        // 16-bit height, 16-bit width, 6-bit quantizer=63 (identity), 1-bit has_mb=0, 1-bit pad=0
        frame.push(3);
        frame.extend_from_slice(&5u16.to_be_bytes());
        frame.extend_from_slice(&ph.to_be_bytes());
        frame.extend_from_slice(&pw.to_be_bytes());
        frame.push(0xFC); // 63 << 2

        // Chunk 6: Macroblock Info (type 6, len 1)
        // mode 4: none predictor
        frame.push(6);
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.push(4);

        // Chunk 5: Plane Data (type 5, chunks ≤ 65535 bytes)
        let deflated = zlib_uncompressed(raw_data);
        for chunk in deflated.chunks(65535) {
            frame.push(5);
            frame.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            frame.extend_from_slice(chunk);
        }
    }

    // Chunk 1: End of Frame (type 1, len 0)
    frame.push(1);
    frame.extend_from_slice(&0u16.to_be_bytes());

    frame
}

/// `POST /api/v3/sessions/{id}/previews/{target}`
pub async fn start_preview(
    State(state): State<SharedState>,
    Path((session_id, target)): Path<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    crate::api_v3::track("sessions/{id}/previews");
    let unescaped_target = decode_percent(&target);
    let clean_target = crate::api_v3::sessions::sanitize_id(&unescaped_target).to_string();
    let unescaped_session = decode_percent(&session_id);
    let clean_session = crate::api_v3::sessions::sanitize_id(&unescaped_session).to_string();
    let stream_id = format!("preview-{}", Uuid::new_v4());

    let preview_type =
        body.as_ref().and_then(|b| b.get("preview_type")).and_then(|v| v.as_str()).unwrap_or("camera").to_string();

    let (width, height) = body
        .as_ref()
        .and_then(|b| b.get("payload"))
        .and_then(|p| p.get("preview"))
        .and_then(|pr| pr.get("resolution"))
        .and_then(|r| {
            let w = r.get("width").and_then(serde_json::Value::as_u64)? as u16;
            let h = r.get("height").and_then(serde_json::Value::as_u64)? as u16;
            Some((w.clamp(160, 1920), h.clamp(90, 1080)))
        })
        .unwrap_or((320, 180));

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    let record = PreviewStreamRecord {
        stream_id: stream_id.clone(),
        session_id: clean_session.clone(),
        target: clean_target.clone(),
        preview_type,
        width,
        height,
        created_at: std::time::Instant::now(),
        last_frame_time: Arc::new(AtomicU64::new(now_ms)),
        frame_counter: Arc::new(AtomicU32::new(0)),
    };

    if let Ok(mut streams) = state.api_v3.preview_streams.lock() {
        prune_stale_streams(&mut streams, now_ms);
        streams.insert(stream_id.clone(), record);
    }

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
                "resolution": { "width": width, "height": height, "fps_limit": 1 }
            }
        })),
    )
        .into_response()
}

/// `GET /api/v3/sessions/{id}/previews/{stream_id}`
pub async fn get_preview_frame(
    State(state): State<SharedState>,
    Path((session_id, stream_id)): Path<(String, String)>,
    Query(query): Query<PreviewFrameQuery>,
) -> Response {
    let unescaped_stream = decode_percent(&stream_id);
    let clean_stream = crate::api_v3::sessions::sanitize_id(&unescaped_stream);
    let unescaped_session = decode_percent(&session_id);
    let clean_session = crate::api_v3::sessions::sanitize_id(&unescaped_session);

    let stream_opt = if let Ok(streams) = state.api_v3.preview_streams.lock() {
        streams
            .get(&unescaped_stream)
            .or_else(|| streams.get(clean_stream))
            .or_else(|| streams.values().find(|s| s.target == clean_stream || s.target == unescaped_stream))
            .cloned()
    } else {
        None
    };

    let stream = stream_opt.unwrap_or_else(|| {
        let rec = PreviewStreamRecord {
            stream_id: clean_stream.to_string(),
            session_id: clean_session.to_string(),
            target: clean_stream.to_string(),
            preview_type: "camera".to_string(),
            width: 320,
            height: 180,
            created_at: std::time::Instant::now(),
            last_frame_time: Arc::new(AtomicU64::new(0)),
            frame_counter: Arc::new(AtomicU32::new(0)),
        };
        if let Ok(mut streams) = state.api_v3.preview_streams.lock() {
            streams.insert(clean_stream.to_string(), rec.clone());
        }
        rec
    });

    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let last_ms = stream.last_frame_time.load(Ordering::Relaxed);
    let is_key_frame_req = query.key.as_deref() == Some("1");

    // Если это не явный запрос ключевого кадра, и с прошлого кадра прошло менее 1 сек (fps_limit: 1)
    // отдаём 204 No Content, чтобы плеер продолжал опрос
    if !is_key_frame_req && last_ms > 0 && now_ms.saturating_sub(last_ms) < 1000 {
        let mut headers = HeaderMap::new();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=1"));
        return (StatusCode::NO_CONTENT, headers).into_response();
    }

    let frame_idx = stream.frame_counter.fetch_add(1, Ordering::Relaxed) as u8;
    stream.last_frame_time.store(now_ms, Ordering::Relaxed);

    let ervc_bytes = generate_ervc_frame(stream.width, stream.height, frame_idx);

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-store, must-revalidate"));

    (StatusCode::OK, headers, Bytes::from(ervc_bytes)).into_response()
}

/// `DELETE /api/v3/sessions/{id}/previews/{call_string_id}`
pub async fn stop_preview(
    State(state): State<SharedState>,
    Path((_session_id, call_string_id)): Path<(String, String)>,
) -> Response {
    let unescaped_id = decode_percent(&call_string_id);
    let clean_id = crate::api_v3::sessions::sanitize_id(&unescaped_id);

    if let Ok(mut streams) = state.api_v3.preview_streams.lock() {
        streams.retain(|k, v| k != &unescaped_id && k != clean_id && v.target != unescaped_id && v.target != clean_id);
    }

    StatusCode::NO_CONTENT.into_response()
}

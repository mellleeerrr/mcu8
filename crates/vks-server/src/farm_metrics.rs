//! `P4a`: метрики фермы энкодеров (`encoder_group_*`) и per-viewer RTP
//! (`viewer_rtp_*`) в `/metrics` и JSON-снимок для `/api/v3/mcu/farm`.
//!
//! Источник — [`McuEngine::video_farm_stats`](vks_media_mcu::McuEngine::video_farm_stats)
//! (снимок групп: refcount, кадры, keyframes, дропы, рестарты, PLI-запросы,
//! агрегат RTP зрителей). Метрики — **гейджи текущего состояния**, а не
//! кумулятивные счётчики процесса: при ретире группы её серии исчезают, при
//! пересоздании начинаются с нуля. Это осознанно: `frames`/`keyframes` — это
//! счётчики *группы* (её жизнь = группа), а `encoder_group_active`/`refcount`
//! показывают, что ферма реально склеила зрителей.
//!
//! Кардинальность ограничена: метки `room`, `codec`, `width`, `height` — по
//! одной серии на группу (групп на комнату ≤ числа профилей, лимит
//! `VKS_VIDEO_FARM_MAX_GROUPS`). Серии обновляются на каждом сборе `/metrics`
//! (`reset()` + `set()` текущих групп), устаревшие не накапливаются.

use std::sync::{Arc, LazyLock, RwLock, Weak};

use prometheus::{register_int_gauge_vec, IntGaugeVec};
use vks_media_mcu::{GroupStat, McuEngine};

/// Метки всех серий фермы.
const LABELS: &[&str] = &["room", "codec", "width", "height"];

fn gauge(name: &'static str, help: &'static str) -> IntGaugeVec {
    register_int_gauge_vec!(name, help, LABELS).expect("farm metric must register once")
}

static ENCODER_GROUP_ACTIVE: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_active", "Encoder groups currently alive (1 per group)"));
static ENCODER_GROUP_REFCOUNT: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_refcount", "Group refcount: viewers + the encoder itself"));
static ENCODER_GROUP_VIEWERS: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_viewers", "Viewers attached to the group"));
static ENCODER_GROUP_VISIBLE_WINDOWS: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_visible_windows", "Occupied composite cells (visible windows) of the room"));
static ENCODER_GROUP_FRAMES: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_frames", "Access units encoded by the group since its start"));
static ENCODER_GROUP_KEYFRAMES: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_keyframes", "Keyframes encoded by the group since its start"));
static ENCODER_GROUP_DROPPED_FRAMES: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge("encoder_group_dropped_frames", "Composite publishes dropped by the group (dedup/queue full)")
});
static ENCODER_GROUP_RESTARTS: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("encoder_group_restarts", "Encoder child restarts in the group"));
static ENCODER_GROUP_KEYFRAME_REQUESTS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge("encoder_group_keyframe_requests", "Keyframe requests (RTCP PLI/FIR) received by the group")
});
static VIEWER_RTP_PACKETS: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("viewer_rtp_packets", "RTP packets sent to group viewers"));
static VIEWER_RTP_BYTES: LazyLock<IntGaugeVec> =
    LazyLock::new(|| gauge("viewer_rtp_bytes", "RTP payload bytes sent to group viewers"));
static VIEWER_RTP_DROPPED: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge("viewer_rtp_dropped_frames", "Frames dropped on the viewer side (broadcast lag or IDR gate)")
});

/// Все метрики фермы — для `reset()` перед обновлением.
static ALL: LazyLock<[&'static IntGaugeVec; 12]> = LazyLock::new(|| {
    [
        &ENCODER_GROUP_ACTIVE,
        &ENCODER_GROUP_REFCOUNT,
        &ENCODER_GROUP_VIEWERS,
        &ENCODER_GROUP_VISIBLE_WINDOWS,
        &ENCODER_GROUP_FRAMES,
        &ENCODER_GROUP_KEYFRAMES,
        &ENCODER_GROUP_DROPPED_FRAMES,
        &ENCODER_GROUP_RESTARTS,
        &ENCODER_GROUP_KEYFRAME_REQUESTS,
        &VIEWER_RTP_PACKETS,
        &VIEWER_RTP_BYTES,
        &VIEWER_RTP_DROPPED,
    ]
});

/// Движки, чьи фермы попадают в метрики. `Weak`: приложение-владелец может
/// умереть (тесты), серии такого движка просто исчезают. Регистрация идемпотентна
/// по адресу движка; в продакшене движок один.
static ENGINES: LazyLock<RwLock<Vec<Weak<McuEngine>>>> = LazyLock::new(|| RwLock::new(Vec::new()));

/// Регистрирует движок для сбора метрик (вызывается при сборке приложения).
pub fn install(engine: &Arc<McuEngine>) {
    let mut engines = ENGINES.write().unwrap();
    if engines.iter().any(|weak| weak.ptr_eq(&Arc::downgrade(engine))) {
        return;
    }
    engines.push(Arc::downgrade(engine));
}

/// Живые движки (мёртвые `Weak` вычищаются по ходу).
fn live_engines() -> Vec<Arc<McuEngine>> {
    let mut engines = ENGINES.write().unwrap();
    engines.retain(|weak| weak.strong_count() > 0);
    engines.iter().filter_map(Weak::upgrade).collect()
}

/// Снимок всех живых групп всех известных движков.
fn collect_stats() -> Vec<GroupStat> {
    live_engines().iter().flat_map(|engine| engine.video_farm_stats()).collect()
}

fn labels(stat: &GroupStat) -> [String; 4] {
    [
        stat.key.room.to_string(),
        stat.key.spec.codec.name().to_string(),
        stat.key.spec.width.to_string(),
        stat.key.spec.height.to_string(),
    ]
}

fn set(vec: &IntGaugeVec, label_values: [&str; 4], value: u64) {
    vec.with_label_values(&label_values).set(i64::try_from(value).unwrap_or(i64::MAX));
}

/// Обновляет серии метрик по снимку групп: `reset()` + значения текущих групп.
pub fn update_from_stats(stats: &[GroupStat]) {
    for vec in ALL.iter() {
        vec.reset();
    }
    for stat in stats {
        let values = labels(stat);
        let refs: [&str; 4] = [&values[0], &values[1], &values[2], &values[3]];
        set(&ENCODER_GROUP_ACTIVE, refs, 1);
        set(&ENCODER_GROUP_REFCOUNT, refs, stat.refcount as u64);
        set(&ENCODER_GROUP_VIEWERS, refs, stat.viewers as u64);
        set(&ENCODER_GROUP_VISIBLE_WINDOWS, refs, stat.visible_windows as u64);
        set(&ENCODER_GROUP_FRAMES, refs, stat.frames);
        set(&ENCODER_GROUP_KEYFRAMES, refs, stat.keyframes);
        set(&ENCODER_GROUP_DROPPED_FRAMES, refs, stat.dropped_frames);
        set(&ENCODER_GROUP_RESTARTS, refs, stat.restarts);
        set(&ENCODER_GROUP_KEYFRAME_REQUESTS, refs, stat.keyframe_requests);
        set(&VIEWER_RTP_PACKETS, refs, stat.viewer_packets);
        set(&VIEWER_RTP_BYTES, refs, stat.viewer_bytes);
        set(&VIEWER_RTP_DROPPED, refs, stat.viewer_dropped);
    }
}

/// Обновляет метрики фермы из всех известных движков (вызывается из `/metrics`).
pub fn update() {
    update_from_stats(&collect_stats());
}

/// JSON-снимок фермы для `/api/v3/mcu/farm` (одна форма на `GroupStat`).
pub fn json_from_stats(stats: &[GroupStat]) -> serde_json::Value {
    let groups: Vec<serde_json::Value> = stats
        .iter()
        .map(|stat| {
            serde_json::json!({
                "room": stat.key.room.to_string(),
                "canvas_version": stat.key.canvas_version,
                "codec": stat.key.spec.codec.name(),
                "width": stat.key.spec.width,
                "height": stat.key.spec.height,
                "fps": stat.key.spec.fps,
                "bitrate_kbps": stat.key.spec.bitrate_kbps,
                "profile": stat.key.spec.profile,
                "level": stat.key.spec.level,
                "refcount": stat.refcount,
                "viewers": stat.viewers,
                "visible_windows": stat.visible_windows,
                "frames": stat.frames,
                "keyframes": stat.keyframes,
                "dropped_frames": stat.dropped_frames,
                "restarts": stat.restarts,
                "keyframe_requests": stat.keyframe_requests,
                "viewer_rtp": {
                    "packets": stat.viewer_packets,
                    "bytes": stat.viewer_bytes,
                    "dropped_frames": stat.viewer_dropped,
                },
            })
        })
        .collect();
    let total = stats.iter().fold(
        serde_json::json!({
            "groups": 0u64, "viewers": 0u64, "frames": 0u64, "viewer_rtp_packets": 0u64,
            "viewer_rtp_bytes": 0u64, "keyframe_requests": 0u64,
        }),
        |mut total, stat| {
            total["groups"] = serde_json::json!(total["groups"].as_u64().unwrap_or(0) + 1);
            total["viewers"] = serde_json::json!(total["viewers"].as_u64().unwrap_or(0) + stat.viewers as u64);
            total["frames"] = serde_json::json!(total["frames"].as_u64().unwrap_or(0) + stat.frames);
            total["viewer_rtp_packets"] =
                serde_json::json!(total["viewer_rtp_packets"].as_u64().unwrap_or(0) + stat.viewer_packets);
            total["viewer_rtp_bytes"] =
                serde_json::json!(total["viewer_rtp_bytes"].as_u64().unwrap_or(0) + stat.viewer_bytes);
            total["keyframe_requests"] =
                serde_json::json!(total["keyframe_requests"].as_u64().unwrap_or(0) + stat.keyframe_requests);
            total
        },
    );
    serde_json::json!({ "groups": groups, "totals": total })
}

/// JSON-снимок фермы всех живых движков.
pub fn snapshot_json() -> serde_json::Value {
    json_from_stats(&collect_stats())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_core::RoomId;
    use vks_media_mcu::{EncoderSpec, FarmCodec};

    /// Тесты модуля обновляют глобальный prometheus-реестр; cargo запускает их
    /// параллельно в одном процессе — сериализуем, иначе один тест видит серии
    /// другого (`reset()` в `update_from_stats`).
    static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn spec(codec: FarmCodec, width: u32, height: u32) -> EncoderSpec {
        EncoderSpec::canonical(codec, width, height, 30, 2048, "baseline", "3.1")
    }

    fn stat(room: RoomId, codec: FarmCodec, width: u32, height: u32) -> GroupStat {
        GroupStat {
            key: spec(codec, width, height).key(room, 1),
            refcount: 4,
            frames: 300,
            keyframes: 10,
            dropped_frames: 2,
            restarts: 0,
            keyframe_requests: 3,
            viewers: 3,
            visible_windows: 3,
            viewer_packets: 1234,
            viewer_bytes: 567_890,
            viewer_dropped: 7,
        }
    }

    #[test]
    fn prometheus_renders_encoder_group_and_viewer_rtp_series() {
        let _guard = METRICS_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let room = RoomId::new();
        update_from_stats(&[stat(room, FarmCodec::H264, 1280, 720)]);
        let text = vks_common::metrics::encode();
        // Text-формат Prometheus сортирует метки по имени.
        let labels = format!("codec=\"h264\",height=\"720\",room=\"{room}\",width=\"1280\"");
        for family in [
            "encoder_group_active",
            "encoder_group_refcount",
            "encoder_group_viewers",
            "encoder_group_visible_windows",
            "encoder_group_frames",
            "encoder_group_keyframes",
            "encoder_group_dropped_frames",
            "encoder_group_restarts",
            "encoder_group_keyframe_requests",
            "viewer_rtp_packets",
            "viewer_rtp_bytes",
            "viewer_rtp_dropped_frames",
        ] {
            assert!(text.contains(family), "метрика {family} отсутствует в /metrics:\n{text}");
        }
        assert!(text.contains(&labels), "метки группы не совпали: {labels}");
        assert!(text.contains("encoder_group_active{") && text.contains("} 1"), "active=1");
        assert!(text.contains("encoder_group_viewers{"), "viewers series");
        assert!(
            text.contains(&format!("encoder_group_visible_windows{{{labels}}} 3")),
            "P1: число видимых окон должно быть видно в /metrics"
        );
        assert!(
            text.contains(&format!("viewer_rtp_packets{{{labels}}} 1234")),
            "viewer_rtp_packets должен быть 1234;\n{text}"
        );
        assert!(
            text.contains(&format!("encoder_group_keyframe_requests{{{labels}}} 3")),
            "PLI-счётчик группы должен быть виден"
        );
    }

    #[test]
    fn retired_group_series_disappear_after_retire() {
        let _guard = METRICS_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let room = RoomId::new();
        update_from_stats(&[stat(room, FarmCodec::H264, 640, 360)]);
        assert!(vks_common::metrics::encode().contains(&room.to_string()), "группа должна быть видна");
        update_from_stats(&[]);
        let text = vks_common::metrics::encode();
        assert!(
            !text.contains(&room.to_string()),
            "после ретира серии группы не должны оставаться в /metrics:\n{text}"
        );
    }

    #[test]
    fn json_snapshot_has_groups_and_totals() {
        let room = RoomId::new();
        let json = json_from_stats(&[stat(room, FarmCodec::H265, 1920, 1080)]);
        let group = &json["groups"][0];
        assert_eq!(group["room"], serde_json::json!(room.to_string()));
        assert_eq!(group["codec"], serde_json::json!("h265"));
        assert_eq!(group["width"], serde_json::json!(1920));
        assert_eq!(group["visible_windows"], serde_json::json!(3), "P1: окна в JSON группы");
        assert_eq!(group["viewers"], serde_json::json!(3));
        assert_eq!(group["viewer_rtp"]["packets"], serde_json::json!(1234));
        assert_eq!(json["totals"]["groups"], serde_json::json!(1));
        assert_eq!(json["totals"]["viewers"], serde_json::json!(3));
        assert_eq!(json["totals"]["viewer_rtp_bytes"], serde_json::json!(567_890));
        assert_eq!(json["totals"]["keyframe_requests"], serde_json::json!(3));
    }
}

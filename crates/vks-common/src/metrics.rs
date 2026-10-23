//! Prometheus-метрики (скаффолдинг для фазы 0).
//!
//! Основание: `docs/rules/BACKEND_RULES.md` §4 (reliability),
//! `docs/rules/BACKEND_RULES.md` §6.4 (error codes 400/401/403/404/409/422/429/500),
//! `docs/architecture/CLUSTERING.md` §12 (monitoring).
//! В фазе 0 — регистр + дефолтные метрики; метрики сервисов добавляются в их фазах.
//!
//! Каждая метрика — публичная статическая `LazyLock<*>Vec`, зарегистрированная
//! один раз (в глобальном default-реестре prometheus). `encode()` выводит их
//! в текстовом формате Prometheus для `/metrics`.

use prometheus::{
    register_histogram_vec, register_int_counter_vec, register_int_gauge_vec, Histogram, HistogramVec, IntCounterVec,
    IntGaugeVec, Registry,
};
use std::sync::LazyLock;

/// Однократная регистрация базовых метрик.
static INIT: std::sync::Once = std::sync::Once::new();

/// Счётчик сигнальных сообщений (label: direction = uplink|downlink).
pub static SIGNALING_MESSAGES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_signaling_messages_total", "Total signaling messages processed", &["direction"])
        .expect("vks_signaling_messages_total must register once")
});

/// Gauge: активные комнаты (label: room_id).
pub static ACTIVE_ROOMS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("vks_active_rooms", "Number of active rooms", &["room_id"])
        .expect("vks_active_rooms must register once")
});

/// Build-info (label: version).
pub static BUILD_INFO: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("vks_build_info", "Build information", &["version"])
        .expect("vks_build_info must register once")
});

/// Счётчик принятых/отправленных байтов медиа (labels: codec, direction = uplink|downlink).
///
/// Основание: `docs/rules/BACKEND_RULES.md` §4 (reliability — наблюдаемость медиа-потоков).
pub static MEDIA_BYTES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_media_bytes_total", "Total media bytes transferred", &["codec", "direction"])
        .expect("vks_media_bytes_total must register once")
});

/// Счётчик дропнутых медиа-пакетов (label: reason — например, "nack_timeout" | "bwe" | "queue_full").
pub static MEDIA_PACKETS_DROPPED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_media_packets_dropped_total", "Total media packets dropped", &["reason"])
        .expect("vks_media_packets_dropped_total must register once")
});

/// Gauge: real WebRTC peer tasks currently registered (process-wide).
pub static WEBRTC_PEERS: LazyLock<prometheus::IntGauge> = LazyLock::new(|| {
    prometheus::register_int_gauge!("vks_webrtc_peers", "Real WebRTC peer tasks")
        .expect("vks_webrtc_peers must register once")
});

/// Counter: browser offers by result (`accepted|rejected|room_full`).
pub static WEBRTC_OFFERS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_webrtc_offers_total", "WebRTC offers by result", &["result"])
        .expect("vks_webrtc_offers_total must register once")
});

/// Counter: ICE failures/evictions by reason.
pub static WEBRTC_ICE_FAILURES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_webrtc_ice_failures_total", "WebRTC ICE failures", &["reason"])
        .expect("vks_webrtc_ice_failures_total must register once")
});

/// Counter: dropped fanout frames by reason (`queue_full`).
pub static WEBRTC_FRAMES_DROPPED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_webrtc_frames_dropped_total", "WebRTC fanout frames dropped", &["reason"])
        .expect("vks_webrtc_frames_dropped_total must register once")
});

/// Counter: offers that fell back to the legacy (non-WebRTC) answer.
pub static WEBRTC_FALLBACK: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_webrtc_fallback_total", "WebRTC fallback events", &["reason"])
        .expect("vks_webrtc_fallback_total must register once")
});

/// Gauge: host codec-worker capability per codec/direction (1 = available).
///
/// Labels: `codec` (`aac`, `aac_latm`, `g729`, `h261`, `h263`, `h263p`, `h265`) and
/// `direction` (`decode`, `encode`). The gateway's SDP policy remains native
/// until the RTP bridge for a codec exists; this gauge only reports the probe.
pub static CODEC_WORKER_SUPPORTED: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "vks_codec_worker_supported",
        "Host codec worker support (1 available, 0 absent)",
        &["codec", "direction"]
    )
    .expect("vks_codec_worker_supported must register once")
});

/// Gauge: host-wide codec worker process budget.
pub static CODEC_WORKER_POOL_SIZE: LazyLock<prometheus::IntGauge> = LazyLock::new(|| {
    prometheus::register_int_gauge!("vks_codec_worker_pool_size", "Configured codec worker process cap")
        .expect("vks_codec_worker_pool_size must register once")
});

/// Gauge: задержка сигнала в комнате в миллисекундах (label: room_id).
/// Обновляется по последнему измерению signal round-trip в комнате.
pub static ROOM_SIGNAL_LATENCY_MS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("vks_room_signal_latency_ms", "Last measured signaling latency in ms", &["room_id"])
        .expect("vks_room_signal_latency_ms must register once")
});

/// Счётчик ошибок по машинному коду (label: code — `ErrorCode::as_str()`,
/// см. `docs/rules/BACKEND_RULES.md` §6.4: 400/401/403/404/409/422/429/500).
pub static ERRORS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_errors_total", "Total errors by error code", &["code"])
        .expect("vks_errors_total must register once")
});

/// HTTP-запросы: `class` — группа маршрутов (`spa|static|api_v1|api_v3|ws|metrics|health|other`),
/// `status` — код ответа. Кардинальность ограничена классом, а не сырым путём.
pub static HTTP_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_http_requests_total", "Total HTTP requests", &["method", "class", "status"])
        .expect("vks_http_requests_total must register once")
});

/// Buckets for the MCU tick histogram: fine below the 2 ms SLO boundary so the
/// exact `le="0.002"` ratio is meaningful, coarse above it for diagnosis.
pub const MCU_TICK_BUCKETS: &[f64] = &[
    0.000_025, 0.000_05, 0.000_1, 0.000_25, 0.000_5, 0.000_75, 0.001, 0.001_5, 0.002, 0.003, 0.005, 0.007_5, 0.01,
    0.02, 0.033, 0.05, 0.1,
];

/// MCU engine tick work: audio mix or video compose, excluding RTP/network,
/// input cloning and composite cloning. It is registered lazily like the other
/// collectors, but only `observe_mcu_tick` (env-gated) and the soak test's
/// `with_label_values` ever materialise children; a default deployment therefore
/// pays nothing and exposes no samples.
static MCU_TICK_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "vks_mcu_tick_duration_seconds",
        "MCU engine tick work duration (audio mix or video compose)",
        &["phase"],
        MCU_TICK_BUCKETS.to_vec()
    )
    .expect("vks_mcu_tick_duration_seconds must register once")
});

static MCU_TICK_AUDIO: LazyLock<Histogram> = LazyLock::new(|| MCU_TICK_DURATION.with_label_values(&["audio"]));
static MCU_TICK_VIDEO: LazyLock<Histogram> = LazyLock::new(|| MCU_TICK_DURATION.with_label_values(&["video"]));

/// Phases of the MCU tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McuTickPhase {
    Audio,
    Video,
}

/// Env-gated tick instrumentation (`VKS_MCU_TICK_METRICS=1`), read once.
pub fn mcu_tick_metrics_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("VKS_MCU_TICK_METRICS")
            .is_ok_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
    })
}

/// Records one successful tick. Call only after the phase actually ran.
pub fn observe_mcu_tick(phase: McuTickPhase, elapsed: std::time::Duration) {
    // Defence in depth for future callers: a recording without the env gate
    // would make the family appear on a supposedly uninstrumented server.
    if !mcu_tick_metrics_enabled() {
        return;
    }
    match phase {
        McuTickPhase::Audio => MCU_TICK_AUDIO.observe(elapsed.as_secs_f64()),
        McuTickPhase::Video => MCU_TICK_VIDEO.observe(elapsed.as_secs_f64()),
    }
}

/// Материализует zero-count series, если инструментация включена. Намеренно
/// не пишет `observe(0.0)`: синтетический sample испортил бы `_count`, `_sum`
/// и точное отношение `le=2ms`, а также мог бы удовлетворить проверку
/// «count > 0» в soak-тесте без единого реального тика.
fn init_mcu_tick_metrics() {
    if mcu_tick_metrics_enabled() {
        let _ = &*MCU_TICK_AUDIO;
        let _ = &*MCU_TICK_VIDEO;
    }
}

/// Гистограмма длительности HTTP-запросов по классу маршрута.
pub static HTTP_REQUEST_DURATION: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!("vks_http_request_duration_seconds", "HTTP request duration in seconds", &["class"])
        .expect("vks_http_request_duration_seconds must register once")
});

/// Байты тела HTTP-ответов по классу маршрута (по Content-Length).
pub static HTTP_RESPONSE_BYTES: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_http_response_bytes_total", "Total HTTP response body bytes", &["class"])
        .expect("vks_http_response_bytes_total must register once")
});

/// Запросы к адаптеру `/api/v3` (label: endpoint — имя роута или `unmatched`).
pub static API_V3_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_api_v3_requests_total", "Total API v3 adapter requests", &["endpoint"])
        .expect("vks_api_v3_requests_total must register once")
});

/// Попытки логина через адаптер (label: result = ok|fail|locked).
pub static LOGIN_ATTEMPTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_login_attempts_total", "Total login attempts", &["result"])
        .expect("vks_login_attempts_total must register once")
});

/// Отказы статике фронтенда (label: reason = not_found|not_configured).
///
/// `not_found` — 404 на запросе к смонтированной статике (`/pnm…`/`/mcu…`): сюда же
/// приходят traversal/невалидные имена — `ServeDir` отказывает мягко и неотличимо
/// от отсутствующего файла (отдельного reason `traversal` нет; см. `with_cache_control`).
/// `not_configured` — рантайм статики не найден на старте (один раз, из `main.rs`).
pub static FRONTEND_DENIED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!("vks_frontend_denied_total", "Frontend requests denied", &["reason"])
        .expect("vks_frontend_denied_total must register once")
});

/// Однократная регистрация + заполнение build-info.
pub fn init_default() {
    INIT.call_once(|| {
        BUILD_INFO.with_label_values(&[env!("CARGO_PKG_VERSION")]).set(1);
        // Materialise zero samples so /metrics shows the WebRTC subsystem even
        // before the first browser offer.
        WEBRTC_PEERS.set(0);
        for result in ["accepted", "rejected", "room_full", "capacity_exceeded", "port_range_exhausted"] {
            WEBRTC_OFFERS.with_label_values(&[result]);
        }
        for reason in ["connect_deadline", "idle_timeout", "Disconnected"] {
            WEBRTC_ICE_FAILURES.with_label_values(&[reason]);
        }
        WEBRTC_FRAMES_DROPPED.with_label_values(&["queue_full"]);
        WEBRTC_FALLBACK.with_label_values(&["disabled"]);
        for codec in ["aac", "aac_latm", "g729", "h261", "h263", "h263p", "h265"] {
            for direction in ["decode", "encode"] {
                CODEC_WORKER_SUPPORTED.with_label_values(&[codec, direction]).set(0);
            }
        }
        CODEC_WORKER_POOL_SIZE.set(0);
        init_mcu_tick_metrics();
    });
}

/// Регистрирует все метрики в собственный `Registry` — *представление* над
/// process-global коллекторами, а не изолированную копию.
///
/// `MetricVec`/`Gauge`/`Histogram` clone — Arc-backed, поэтому здесь
/// регистрируются клоны глобальных коллекторов: значения общие, а `gather()`
/// этого реестра равен глобальному набору семей. Это тестовый/инспекционный
/// helper (отдельный `/metrics` без глобального состояния он не даёт); при
/// добавлении новой глобальной метрики её нужно добавить и сюда, иначе
/// тест `new_registry_is_a_full_view_of_global_metrics` это поймает.
#[must_use]
pub fn new_registry() -> Registry {
    let mut registry = Registry::new();
    // `MetricVec`/`Gauge`/`Histogram` clone are Arc-backed: registering clones of
    // the global collectors shares their state instead of duplicating Opts, which
    // used to drift from the declarations above (and made the test exercise the
    // global registry instead of this function).
    let register = |registry: &mut Registry, m: Box<dyn prometheus::core::Collector>, name: &str| {
        registry.register(m).unwrap_or_else(|e| panic!("register {name} in new_registry: {e:?}"))
    };
    register(&mut registry, Box::new((*SIGNALING_MESSAGES).clone()), "vks_signaling_messages_total");
    register(&mut registry, Box::new((*CODEC_WORKER_SUPPORTED).clone()), "vks_codec_worker_supported");
    register(&mut registry, Box::new((*CODEC_WORKER_POOL_SIZE).clone()), "vks_codec_worker_pool_size");
    register(&mut registry, Box::new((*ACTIVE_ROOMS).clone()), "vks_active_rooms");
    register(&mut registry, Box::new((*BUILD_INFO).clone()), "vks_build_info");
    register(&mut registry, Box::new((*MEDIA_BYTES).clone()), "vks_media_bytes_total");
    register(&mut registry, Box::new((*MEDIA_PACKETS_DROPPED).clone()), "vks_media_packets_dropped_total");
    register(&mut registry, Box::new((*WEBRTC_PEERS).clone()), "vks_webrtc_peers");
    register(&mut registry, Box::new((*WEBRTC_OFFERS).clone()), "vks_webrtc_offers_total");
    register(&mut registry, Box::new((*WEBRTC_ICE_FAILURES).clone()), "vks_webrtc_ice_failures_total");
    register(&mut registry, Box::new((*WEBRTC_FRAMES_DROPPED).clone()), "vks_webrtc_frames_dropped_total");
    register(&mut registry, Box::new((*WEBRTC_FALLBACK).clone()), "vks_webrtc_fallback_total");
    register(&mut registry, Box::new((*ROOM_SIGNAL_LATENCY_MS).clone()), "vks_room_signal_latency_ms");
    register(&mut registry, Box::new((*ERRORS).clone()), "vks_errors_total");
    register(&mut registry, Box::new((*HTTP_REQUESTS).clone()), "vks_http_requests_total");
    register(&mut registry, Box::new((*MCU_TICK_DURATION).clone()), "vks_mcu_tick_duration_seconds");
    register(&mut registry, Box::new((*HTTP_REQUEST_DURATION).clone()), "vks_http_request_duration_seconds");
    register(&mut registry, Box::new((*HTTP_RESPONSE_BYTES).clone()), "vks_http_response_bytes_total");
    register(&mut registry, Box::new((*API_V3_REQUESTS).clone()), "vks_api_v3_requests_total");
    register(&mut registry, Box::new((*LOGIN_ATTEMPTS).clone()), "vks_login_attempts_total");
    register(&mut registry, Box::new((*FRONTEND_DENIED).clone()), "vks_frontend_denied_total");
    registry
}

/// Text-format export of the global default registry (the `/metrics` body).
pub fn encode() -> String {
    prometheus::TextEncoder::new().encode_to_string(&prometheus::gather()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcu_tick_buckets_are_strictly_increasing_and_cover_slo_bounds() {
        assert!(MCU_TICK_BUCKETS.windows(2).all(|pair| pair[0] < pair[1]), "buckets must be strictly increasing");
        assert!(MCU_TICK_BUCKETS.contains(&0.002), "exact 2 ms bucket is the SLO gate");
        assert!(MCU_TICK_BUCKETS.contains(&0.033), "exact 33 ms bucket is the video frame-budget gate");
        assert!(MCU_TICK_BUCKETS.last().is_some_and(|last| *last >= 0.1));
    }

    #[test]
    fn encode_contains_build_info() {
        init_default();
        let text = encode();
        assert!(text.contains("vks_build_info"), "metrics output should include build info");
    }

    #[test]
    fn signaling_counter_increments() {
        init_default();
        SIGNALING_MESSAGES.with_label_values(&["uplink"]).inc();
        let text = encode();
        assert!(text.contains("vks_signaling_messages_total"));
    }

    #[test]
    fn codec_worker_metrics_are_registered() {
        init_default();
        let text = encode();
        for name in ["vks_codec_worker_supported", "vks_codec_worker_pool_size"] {
            assert!(text.contains(name), "missing {name} in /metrics output");
        }
    }

    #[test]
    fn webrtc_metrics_are_registered() {
        init_default();
        let text = encode();
        for name in [
            "vks_webrtc_peers",
            "vks_webrtc_offers_total",
            "vks_webrtc_ice_failures_total",
            "vks_webrtc_frames_dropped_total",
            "vks_webrtc_fallback_total",
        ] {
            assert!(text.contains(name), "missing {name} in /metrics output");
        }
    }

    #[test]
    fn active_rooms_gauge_settable() {
        init_default();
        ACTIVE_ROOMS.with_label_values(&["room-1"]).set(3);
        let text = encode();
        assert!(text.contains("vks_active_rooms"));
        ACTIVE_ROOMS.with_label_values(&["room-1"]).set(0);
    }

    // Тесты новых labeled-метрик: устанавливаем значения и проверяем, что они
    // появляются в `encode()` (глобальный default-реестр).
    #[test]
    fn media_bytes_counter_appears_in_encode() {
        init_default();
        MEDIA_BYTES.with_label_values(&["opus", "uplink"]).inc_by(1234);
        MEDIA_BYTES.with_label_values(&["h264", "downlink"]).inc_by(4321);
        let text = encode();
        assert!(text.contains("vks_media_bytes_total"), "encoded metrics must include vks_media_bytes_total");
        assert!(
            text.contains("vks_media_bytes_total{codec=\"opus\",direction=\"uplink\"} 1234"),
            "expected opus/uplink sample, got: {text}"
        );
        assert!(
            text.contains("vks_media_bytes_total{codec=\"h264\",direction=\"downlink\"} 4321"),
            "expected h264/downlink sample, got: {text}"
        );
    }

    #[test]
    fn media_packets_dropped_counter_appears_in_encode() {
        init_default();
        MEDIA_PACKETS_DROPPED.with_label_values(&["nack_timeout"]).inc();
        MEDIA_PACKETS_DROPPED.with_label_values(&["queue_full"]).inc_by(7);
        let text = encode();
        assert!(text.contains("vks_media_packets_dropped_total"));
        assert!(
            text.contains("vks_media_packets_dropped_total{reason=\"nack_timeout\"} 1"),
            "expected nack_timeout sample, got: {text}"
        );
        assert!(
            text.contains("vks_media_packets_dropped_total{reason=\"queue_full\"} 7"),
            "expected queue_full sample, got: {text}"
        );
    }

    #[test]
    fn room_signal_latency_gauge_settable() {
        init_default();
        ROOM_SIGNAL_LATENCY_MS.with_label_values(&["room-42"]).set(87);
        let text = encode();
        assert!(
            text.contains("vks_room_signal_latency_ms{room_id=\"room-42\"} 87"),
            "expected room-42 latency sample, got: {text}"
        );
        // Gauge — можно пересетить (последний замер).
        ROOM_SIGNAL_LATENCY_MS.with_label_values(&["room-42"]).set(12);
        let text = encode();
        assert!(
            text.contains("vks_room_signal_latency_ms{room_id=\"room-42\"} 12"),
            "gauge must reflect the last set value, got: {text}"
        );
        ROOM_SIGNAL_LATENCY_MS.with_label_values(&["room-42"]).set(0);
    }

    #[test]
    fn errors_counter_by_code_appears_in_encode() {
        init_default();
        use crate::error::ErrorCode;
        for code in [
            ErrorCode::InvalidInput,
            ErrorCode::AuthFailed,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::Conflict,
            ErrorCode::Unprocessable,
            ErrorCode::RateLimited,
            ErrorCode::Internal,
            ErrorCode::Media,
            ErrorCode::Cluster,
        ] {
            ERRORS.with_label_values(&[code.as_str()]).inc();
        }
        let text = encode();
        assert!(text.contains("vks_errors_total"));
        // Каждый из десяти кодов присутствует с label `code`.
        for code in [
            ErrorCode::InvalidInput,
            ErrorCode::AuthFailed,
            ErrorCode::Forbidden,
            ErrorCode::NotFound,
            ErrorCode::Conflict,
            ErrorCode::Unprocessable,
            ErrorCode::RateLimited,
            ErrorCode::Internal,
            ErrorCode::Media,
            ErrorCode::Cluster,
        ] {
            let expected = format!("vks_errors_total{{code=\"{}\"}} 1", code.as_str());
            assert!(text.contains(&expected), "expected {expected} in encoded metrics");
        }
    }

    // `new_registry` is a view over the process-global collectors. The test seeds
    // one sample per family (so both `gather()` calls emit every family), compares
    // the complete family-name sets, and proves the collectors share state rather
    // than being a drifted copy of the declarations.
    #[test]
    fn new_registry_is_a_full_view_of_global_metrics() {
        init_default();
        SIGNALING_MESSAGES.with_label_values(&["uplink"]).inc();
        CODEC_WORKER_SUPPORTED.with_label_values(&["h264", "encode"]).set(1);
        CODEC_WORKER_POOL_SIZE.set(1);
        ACTIVE_ROOMS.with_label_values(&["r1"]).set(1);
        BUILD_INFO.with_label_values(&[env!("CARGO_PKG_VERSION")]).set(1);
        MEDIA_BYTES.with_label_values(&["t14b", "uplink"]).inc_by(1234);
        MEDIA_PACKETS_DROPPED.with_label_values(&["congestion"]).inc();
        WEBRTC_PEERS.set(1);
        WEBRTC_OFFERS.with_label_values(&["ok"]).inc();
        WEBRTC_ICE_FAILURES.with_label_values(&["timeout"]).inc();
        WEBRTC_FRAMES_DROPPED.with_label_values(&["congestion"]).inc();
        WEBRTC_FALLBACK.with_label_values(&["relay"]).inc();
        ROOM_SIGNAL_LATENCY_MS.with_label_values(&["r1"]).set(5);
        ERRORS.with_label_values(&["room_not_found"]).inc();
        HTTP_REQUESTS.with_label_values(&["GET", "spa", "200"]).inc();
        HTTP_REQUEST_DURATION.with_label_values(&["spa"]).observe(0.01);
        HTTP_RESPONSE_BYTES.with_label_values(&["spa"]).inc_by(1024);
        API_V3_REQUESTS.with_label_values(&["login"]).inc();
        LOGIN_ATTEMPTS.with_label_values(&["ok"]).inc();
        FRONTEND_DENIED.with_label_values(&["not_found"]).inc();
        // Zero-count child, not `observe(0.0)`: a synthetic sample would inflate
        // `_count` and could satisfy a count>0 gate.
        let _ = MCU_TICK_DURATION.with_label_values(&["audio"]);

        let view = new_registry();
        let view_names: std::collections::BTreeSet<_> = view.gather().iter().map(|f| f.name().to_string()).collect();
        let global_names: std::collections::BTreeSet<_> =
            prometheus::gather().iter().map(|f| f.name().to_string()).collect();
        assert_eq!(view_names, global_names, "new_registry must expose exactly the global family set");

        let shared = view
            .gather()
            .into_iter()
            .find(|family| family.name() == "vks_media_bytes_total")
            .expect("view must contain vks_media_bytes_total")
            .get_metric()
            .iter()
            .find(|metric| {
                metric.get_label().iter().any(|label| label.name() == "codec" && label.value() == "t14b")
                    && metric.get_label().iter().any(|label| label.name() == "direction" && label.value() == "uplink")
            })
            .map(|metric| metric.get_counter().value())
            .expect("seeded t14b/uplink media counter");
        assert_eq!(shared, 1234.0, "view collectors must share Arc-backed state with the globals");
    }
}

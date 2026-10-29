//! Настройка буферов UDP-сокетов (SO_RCVBUF/SO_SNDBUF) для медиа-плоскости (T15/L0).
//!
//! Зачем: `net.core.rmem_max` сам по себе ничего не меняет — ядро выделяет
//! буфер конкретному сокету, а `tokio::net::UdpSocket`/`std::net::UdpSocket`
//! создают его с дефолтом (на стенде 212 992 B). Без `setsockopt` рекомендации
//! sysctl — театр. Этот модуль — единственная точка, где медиа-сокеты получают
//! размеры буферов:
//!
//! - общий медиа-луп SFU (`vks-media-sfu::udp_loop`, порт `40000`);
//! - per-peer WebRTC UDP-сокеты (`vks-media-sfu::webrtc_room`, порты 43000–43100);
//! - per-call RTP/RTCP-сокеты SIP-моста (`vks-gateway-sip::rtp_bridge`).
//!
//! Linux удваивает запрошенный размер (`setsockopt(SO_RCVBUF, N)` → фактически
//! `2*min(N, net.core.rmem_max)`), поэтому «эффективное» значение читается
//! обратно через `getsockopt` и попадает в лог и метрики
//! `vks_udp_rcvbuf_bytes{role=...}` / `vks_udp_sndbuf_bytes{role=...}`.
//!
//! Конфигурация — только env (без глобального состояния и без `unsafe`):
//!
//! | переменная | роль | дефолт |
//! |---|---|---|
//! | `VKS_UDP_RCVBUF_BYTES` | `media_loop` | 4 MiB |
//! | `VKS_UDP_SNDBUF_BYTES` | `media_loop` | 1 MiB |
//! | `VKS_WEBRTC_UDP_RCVBUF_BYTES` | `webrtc_peer` | 1 MiB |
//! | `VKS_WEBRTC_UDP_SNDBUF_BYTES` | `webrtc_peer` | 512 KiB |
//! | `VKS_GATEWAY_UDP_RCVBUF_BYTES` | `gateway_rtp` | 512 KiB |
//! | `VKS_GATEWAY_UDP_SNDBUF_BYTES` | `gateway_rtp` | 512 KiB |
//!
//! `0` (или пустое значение после trim) — не трогать буфер, оставить дефолт
//! ядра; невалидное число — WARN и дефолт (это производительный тюнинг, а не
//! fail-closed лимит). Значения выше `net.core.rmem_max`/`wmem_max` ядро
//! урежет — фактический размер всегда виден в логе/метрике.

use std::io;
use std::os::fd::AsFd;
use std::sync::LazyLock;

use prometheus::{register_int_gauge_vec, IntGaugeVec};
use socket2::SockRef;
use tracing::warn;

/// Env: размер приёмного буфера общих медиа-сокетов (SFU media loop).
pub const ENV_MEDIA_RCVBUF: &str = "VKS_UDP_RCVBUF_BYTES";
/// Env: размер отправляющего буфера общих медиа-сокетов.
pub const ENV_MEDIA_SNDBUF: &str = "VKS_UDP_SNDBUF_BYTES";
/// Env: размер приёмного буфера per-peer WebRTC UDP-сокетов.
pub const ENV_WEBRTC_RCVBUF: &str = "VKS_WEBRTC_UDP_RCVBUF_BYTES";
/// Env: размер отправляющего буфера per-peer WebRTC UDP-сокетов.
pub const ENV_WEBRTC_SNDBUF: &str = "VKS_WEBRTC_UDP_SNDBUF_BYTES";
/// Env: размер приёмного буфера per-call RTP/RTCP-сокетов SIP/H.323-мостов.
pub const ENV_GATEWAY_RCVBUF: &str = "VKS_GATEWAY_UDP_RCVBUF_BYTES";
/// Env: размер отправляющего буфера per-call RTP/RTCP-сокетов.
pub const ENV_GATEWAY_SNDBUF: &str = "VKS_GATEWAY_UDP_SNDBUF_BYTES";

/// 4 MiB на единственный общий медиа-луп: в него мультиплексируется весь
/// входной RTP/STUN/DTLS комнаты.
pub const DEFAULT_MEDIA_RCVBUF_BYTES: usize = 4 * 1024 * 1024;
/// 1 MiB на отправку: исходящий поток ограничен pacer'ом, больше не нужно.
pub const DEFAULT_MEDIA_SNDBUF_BYTES: usize = 1024 * 1024;
/// 1 MiB на per-peer WebRTC-сокет: при 256 пирах это ~256 MiB лимита,
/// а не 1 GiB как с медиа-дефолтом 4 MiB. Буфер выделяется по мере надобности.
pub const DEFAULT_WEBRTC_RCVBUF_BYTES: usize = 1024 * 1024;
/// 512 KiB на отправку per-peer: исходящий поток ограничен pacer'ом.
pub const DEFAULT_WEBRTC_SNDBUF_BYTES: usize = 512 * 1024;
/// 512 KiB на per-call RTP-сокет: при 20 звонках это ~20 MiB лимита, а не 160.
pub const DEFAULT_GATEWAY_RCVBUF_BYTES: usize = 512 * 1024;
/// 512 KiB на отправку per-call RTP/RTCP.
pub const DEFAULT_GATEWAY_SNDBUF_BYTES: usize = 512 * 1024;

// Per-call sockets выставляются чаще, чем один media loop: их дефолты обязаны
// быть меньше. Compile-time проверка, чтобы правка констант не проскочила.
const _: () = assert!(DEFAULT_GATEWAY_RCVBUF_BYTES < DEFAULT_MEDIA_RCVBUF_BYTES);
const _: () = assert!(DEFAULT_GATEWAY_SNDBUF_BYTES < DEFAULT_MEDIA_SNDBUF_BYTES);
const _: () = assert!(DEFAULT_WEBRTC_RCVBUF_BYTES < DEFAULT_MEDIA_RCVBUF_BYTES);

/// Gauge: эффективный `SO_RCVBUF` тюнингованных UDP-сокетов (T15/L0).
///
/// Label `role` = `media_loop` | `webrtc_peer` | `gateway_rtp`. Значение читается обратно
/// `getsockopt`, т.е. уже включает удвоение ядра и потолок `rmem_max` — ровно
/// то, что показывает `ss -u -m`.
pub static UDP_RCVBUF_BYTES: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("vks_udp_rcvbuf_bytes", "Effective kernel SO_RCVBUF per UDP socket role", &["role"])
        .expect("vks_udp_rcvbuf_bytes must register once")
});

/// Gauge: эффективный `SO_SNDBUF` тюнингованных UDP-сокетов (T15/L0).
pub static UDP_SNDBUF_BYTES: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("vks_udp_sndbuf_bytes", "Effective kernel SO_SNDBUF per UDP socket role", &["role"])
        .expect("vks_udp_sndbuf_bytes must register once")
});

/// Роль сокета — только для метрик/логов и разных дефолтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpSocketRole {
    /// Общий медиа-луп SFU (один сокет на процесс, порт `40000`).
    MediaLoop,
    /// Per-peer WebRTC UDP-сокет (порты 43000–43100).
    WebRtcPeer,
    /// Per-call RTP/RTCP-сокеты шлюза (SIP/H.323 media bridge).
    GatewayRtp,
}

impl UdpSocketRole {
    /// Стабильный label для Prometheus/логов (не менять без миграции дашбордов).
    pub fn as_label(self) -> &'static str {
        match self {
            Self::MediaLoop => "media_loop",
            Self::WebRtcPeer => "webrtc_peer",
            Self::GatewayRtp => "gateway_rtp",
        }
    }
}

/// Запрошенные размеры буферов; `None` = оставить дефолт ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpBufferConfig {
    /// Роль сокета (метки/дефолты).
    pub role: UdpSocketRole,
    /// Запрошенный `SO_RCVBUF`; `None` — не вызывать `setsockopt`.
    pub rcvbuf: Option<usize>,
    /// Запрошенный `SO_SNDBUF`; `None` — не вызывать `setsockopt`.
    pub sndbuf: Option<usize>,
}

impl UdpBufferConfig {
    /// Конфиг для единственного общего медиа-лупа (`VKS_UDP_*`).
    pub fn for_media_loop() -> Self {
        Self::from_env(UdpSocketRole::MediaLoop)
    }

    /// Конфиг для per-peer WebRTC UDP-сокетов (`VKS_WEBRTC_UDP_*`).
    pub fn for_webrtc_peer() -> Self {
        Self::from_env(UdpSocketRole::WebRtcPeer)
    }

    /// Конфиг для per-call RTP/RTCP-сокетов (`VKS_GATEWAY_UDP_*`).
    pub fn for_gateway_rtp() -> Self {
        Self::from_env(UdpSocketRole::GatewayRtp)
    }

    /// Читает env и подставляет дефолты; невалидные значения — WARN + дефолт.
    pub fn from_env(role: UdpSocketRole) -> Self {
        let (rcv_var, rcv_default, snd_var, snd_default) = match role {
            UdpSocketRole::MediaLoop => {
                (ENV_MEDIA_RCVBUF, DEFAULT_MEDIA_RCVBUF_BYTES, ENV_MEDIA_SNDBUF, DEFAULT_MEDIA_SNDBUF_BYTES)
            }
            UdpSocketRole::WebRtcPeer => {
                (ENV_WEBRTC_RCVBUF, DEFAULT_WEBRTC_RCVBUF_BYTES, ENV_WEBRTC_SNDBUF, DEFAULT_WEBRTC_SNDBUF_BYTES)
            }
            UdpSocketRole::GatewayRtp => {
                (ENV_GATEWAY_RCVBUF, DEFAULT_GATEWAY_RCVBUF_BYTES, ENV_GATEWAY_SNDBUF, DEFAULT_GATEWAY_SNDBUF_BYTES)
            }
        };
        Self {
            role,
            rcvbuf: parse_buffer_env(std::env::var(rcv_var).ok().as_deref(), rcv_default, rcv_var),
            sndbuf: parse_buffer_env(std::env::var(snd_var).ok().as_deref(), snd_default, snd_var),
        }
    }

    /// Явный конфиг без env — для тестов и будущих вызовов из конфига.
    pub fn with_sizes(role: UdpSocketRole, rcvbuf: Option<usize>, sndbuf: Option<usize>) -> Self {
        Self { role, rcvbuf, sndbuf }
    }

    /// Полностью отключить тюнинг (оставить дефолты ядра).
    pub fn disabled(role: UdpSocketRole) -> Self {
        Self { role, rcvbuf: None, sndbuf: None }
    }
}

/// Разбор значения env: unset → дефолт, `0` → `None` (дефолт ядра),
/// мусор → WARN + дефолт. Отдельная функция — чтобы тестировать без env.
pub fn parse_buffer_env(raw: Option<&str>, default: usize, variable: &str) -> Option<usize> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Some(default);
    };
    match raw.parse::<usize>() {
        Ok(0) => None,
        Ok(value) => Some(value),
        Err(_) => {
            warn!(
                variable,
                value = raw,
                default_bytes = default,
                "invalid UDP socket buffer size; using the documented default"
            );
            Some(default)
        }
    }
}

/// Фактические (прочитанные обратно из ядра) размеры буферов сокета.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveUdpBuffers {
    /// Роль сокета.
    pub role: UdpSocketRole,
    /// Что просили для `SO_RCVBUF` (`None` — не трогали).
    pub requested_rcvbuf: Option<usize>,
    /// Что просили для `SO_SNDBUF` (`None` — не трогали).
    pub requested_sndbuf: Option<usize>,
    /// Фактический `SO_RCVBUF` (Linux: `2*min(request, rmem_max)`).
    pub rcvbuf: usize,
    /// Фактический `SO_SNDBUF`.
    pub sndbuf: usize,
}

impl EffectiveUdpBuffers {
    /// Компактная строка для лога: `role=media_loop rcvbuf=8388608/4194304`.
    pub fn summary(&self) -> String {
        format!(
            "role={} rcvbuf={}/{} sndbuf={}/{}",
            self.role.as_label(),
            self.rcvbuf,
            self.requested_rcvbuf.map_or("default".to_string(), |value| value.to_string()),
            self.sndbuf,
            self.requested_sndbuf.map_or("default".to_string(), |value| value.to_string()),
        )
    }
}

/// Применяет `SO_RCVBUF`/`SO_SNDBUF` и читает фактический размер обратно.
///
/// `socket` — любой `AsFd` (std/tokio UDP-сокет). `setsockopt` не требует
/// root: ядро урежет запрос до `net.core.rmem_max`/`wmem_max`, поэтому check
/// эффективных значений — задача `scripts/host-doctor.sh`.
///
/// # Errors
/// Ошибка `setsockopt`/`getsockopt` (например, сокет уже закрыт).
pub fn apply_udp_buffers<S: AsFd>(socket: &S, config: &UdpBufferConfig) -> io::Result<EffectiveUdpBuffers> {
    let handle = SockRef::from(socket);
    if let Some(size) = config.rcvbuf {
        handle.set_recv_buffer_size(size)?;
    }
    if let Some(size) = config.sndbuf {
        handle.set_send_buffer_size(size)?;
    }
    let effective = read_udp_buffers(socket, config)?;
    UDP_RCVBUF_BYTES.with_label_values(&[config.role.as_label()]).set(effective.rcvbuf as i64);
    UDP_SNDBUF_BYTES.with_label_values(&[config.role.as_label()]).set(effective.sndbuf as i64);
    Ok(effective)
}

/// Только чтение фактических буферов (без `setsockopt`).
///
/// # Errors
/// Ошибка `getsockopt` (например, сокет уже закрыт).
pub fn read_udp_buffers<S: AsFd>(socket: &S, config: &UdpBufferConfig) -> io::Result<EffectiveUdpBuffers> {
    let handle = SockRef::from(socket);
    Ok(EffectiveUdpBuffers {
        role: config.role,
        requested_rcvbuf: config.rcvbuf,
        requested_sndbuf: config.sndbuf,
        rcvbuf: handle.recv_buffer_size()?,
        sndbuf: handle.send_buffer_size()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_defaults_overrides_disable_and_garbage() {
        let variable = "VKS_UDP_RCVBUF_BYTES";
        assert_eq!(parse_buffer_env(None, 4096, variable), Some(4096));
        assert_eq!(parse_buffer_env(Some("  "), 4096, variable), Some(4096));
        assert_eq!(parse_buffer_env(Some("1048576"), 4096, variable), Some(1_048_576));
        assert_eq!(parse_buffer_env(Some("0"), 4096, variable), None);
        // Мусор не должен ронять старт медиа-плоскости: дефолт + WARN.
        assert_eq!(parse_buffer_env(Some("4MiB"), 4096, variable), Some(4096));
        assert_eq!(parse_buffer_env(Some("-1"), 4096, variable), Some(4096));
    }

    #[test]
    fn roles_have_sane_distinct_defaults_and_labels() {
        assert_eq!(UdpSocketRole::MediaLoop.as_label(), "media_loop");
        assert_eq!(UdpSocketRole::WebRtcPeer.as_label(), "webrtc_peer");
        assert_eq!(UdpSocketRole::GatewayRtp.as_label(), "gateway_rtp");
        // Per-call/per-peer сокеты не должны резервировать 4 MiB каждый — это
        // проверяют compile-time assert рядом с константами.
        assert_ne!(DEFAULT_GATEWAY_RCVBUF_BYTES, DEFAULT_MEDIA_RCVBUF_BYTES);
        assert_eq!(DEFAULT_WEBRTC_RCVBUF_BYTES, 1024 * 1024);
        assert!(DEFAULT_WEBRTC_RCVBUF_BYTES > DEFAULT_GATEWAY_RCVBUF_BYTES);
    }

    #[test]
    fn apply_sets_and_reads_back_effective_buffers() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP socket");
        // 64 KiB заведомо ниже типичного rmem_max; Linux удваивает запрос,
        // поэтому фактическое значение должно быть >= запрошенного.
        let config = UdpBufferConfig::with_sizes(UdpSocketRole::MediaLoop, Some(65_536), Some(65_536));
        let effective = apply_udp_buffers(&socket, &config).expect("apply buffers");
        assert_eq!(effective.role, UdpSocketRole::MediaLoop);
        assert_eq!(effective.requested_rcvbuf, Some(65_536));
        assert_eq!(effective.requested_sndbuf, Some(65_536));
        assert!(
            effective.rcvbuf >= 65_536,
            "kernel must accept a small SO_RCVBUF request: {} < 65536",
            effective.rcvbuf
        );
        assert!(effective.sndbuf >= 65_536, "kernel must accept a small SO_SNDBUF request");
        let summary = effective.summary();
        assert!(summary.contains("role=media_loop"), "{summary}");
        assert!(summary.contains("rcvbuf="), "{summary}");
    }

    #[test]
    fn disabled_config_keeps_kernel_defaults_but_reports_them() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP socket");
        let before = read_udp_buffers(&socket, &UdpBufferConfig::disabled(UdpSocketRole::GatewayRtp))
            .expect("read default buffers");
        let after =
            apply_udp_buffers(&socket, &UdpBufferConfig::disabled(UdpSocketRole::GatewayRtp)).expect("no-op apply");
        assert_eq!(before, after);
        assert_eq!(after.requested_rcvbuf, None);
        assert_eq!(after.requested_sndbuf, None);
        assert!(after.rcvbuf > 0 && after.sndbuf > 0);
    }

    #[test]
    fn effective_buffer_gauges_are_exported() {
        use crate::metrics;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP socket");
        let config = UdpBufferConfig::with_sizes(UdpSocketRole::GatewayRtp, Some(65_536), None);
        let effective = apply_udp_buffers(&socket, &config).expect("apply buffers");
        let text = metrics::encode();
        // Метрики — глобальный реестр процесса, а тесты идут параллельно:
        // другой тест может перезаписать gauge дефолтом ядра (212 992 B).
        // Проверяем, что family/label экспортируется и значение не меньше
        // запрошенного минимума, а не точное число.
        let prefix = "vks_udp_rcvbuf_bytes{role=\"gateway_rtp\"} ";
        let exported = text
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("missing {prefix} in /metrics: {text}"));
        let value: usize = exported.trim().parse().expect("gauge value is an integer");
        assert!(value >= 65_536, "effective rcvbuf gauge {value} < requested 65536");
        assert!(effective.rcvbuf > 0);
    }
}

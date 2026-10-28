//! vks-sipbot — SIP-бот (UAC) для нагрузочных прогонов MCU.
//!
//! ЧЕРНОВИК. Не компилировался (правило раунда: без `cargo build`).
//! Переносится в `crates/vks-gateway-sip/src/bin/sipbot.rs` после согласования;
//! тогда в `crates/vks-gateway-sip/Cargo.toml` добавляется:
//!
//! ```toml
//! [features]
//! bench-bot = []                 # пустой feature — только гейт сборки
//!
//! [[bin]]
//! name = "vks-sipbot"
//! path = "src/bin/sipbot.rs"
//! required-features = ["bench-bot"]
//! ```
//!
//! Сборка: `cargo build --release -p vks-gateway-sip --features bench-bot --bin vks-sipbot`.
//! Никаких новых зависимостей: tokio/uuid уже в крейте, кодеки — через
//! `vks_gateway_sip::{opus,g722,g726,g728,g7231,rtp_bridge}`.
//!
//! Что делает: N параллельных вызовов, INVITE -> SDP offer (по профилю) ->
//! 200 OK/ACK -> RTP 20 мс (аудио) и 30 fps (видео из Annex-B) -> приём RTP,
//! seq/loss/jitter/RTCP RR -> JSON по строке на вызов.
//!
//! Замечания по совместимости с нашим UAS (crates/vks-gateway-sip/src/uas.rs):
//!  * транспорт только plain RTP/AVP: SRTP/SAVP -> 488 (uas.rs:7, 992-994);
//!  * H.264 требует packetization-mode=1, profile_idc=66, level>=31 (uas.rs:659-680);
//!  * H.265: profile-id=1 (uas.rs:694-709);
//!  * Opus: opus/48000/2 (uas.rs:510-523);
//!  * G.729 (PT18) -> 488 (uas.rs:880-898): в матрице его нет.
//!
//! Не реализовано в черновике (помечено TODO, не выдавать за готовое):
//!  * RTCP RR отправка (сейчас только приём и подсчёт; см. открытый вопрос №2);
//!  * повторная передача/RTX, NACK (в MCU их тоже нет);
//!  * hold/re-INVITE (UAS отвечает 501, gateways.rs:2264-2280).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::time::{interval, MissedTickBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// M0: сигналинг без медиа.
    SignalOnly,
    /// M1: G.711a (PCMA, PT8).
    Pcma,
    /// M2: G.722 (PT9).
    G722,
    /// M2: Opus (динамический PT96, 48 кГц/2).
    Opus,
    /// M2: G.726-32 (динамический PT107).
    G726_32,
    /// M3/M4: видео H.264 Baseline 720p30 + PCMA.
    H264_720p30,
    /// M4: видео H.265 1080p60 + Opus.
    H265_1080p60,
}

impl Profile {
    fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "m0" => Self::SignalOnly,
            "m1" => Self::Pcma,
            "m2" | "g722" => Self::G722,
            "m2-opus" | "opus" => Self::Opus,
            "m2-g726" | "g726" => Self::G726_32,
            "m3" | "h264" => Self::H264_720p30,
            "m4" | "h265" => Self::H265_1080p60,
            _ => return None,
        })
    }

    fn needs_audio(&self) -> bool {
        !matches!(self, Self::SignalOnly)
    }

    fn video(&self) -> Option<VideoProfile> {
        match self {
            Self::H264_720p30 => Some(VideoProfile { h265: false, level_idc: "42e01f", fps: 30, sdp_pt: 96 }),
            Self::H265_1080p60 => Some(VideoProfile { h265: true, level_idc: "profile-id=1", fps: 60, sdp_pt: 125 }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VideoProfile {
    h265: bool,
    /// Для H.264 — profile-level-id; для H.265 — fmtp целиком (`profile-id=1`).
    level_idc: &'static str,
    fps: u32,
    sdp_pt: u8,
}

/// SDP-оффер, который наш UAS гарантированно понимает.
fn build_offer(profile: Profile, ip: &str, audio_port: u16, video_port: Option<u16>) -> String {
    let mut sdp = format!("v=0\r\no=- 1 1 IN IP4 {ip}\r\ns=vks-sipbot\r\nc=IN IP4 {ip}\r\nt=0 0\r\n");
    match profile {
        Profile::SignalOnly => {
            sdp.push_str(&format!("m=audio {audio_port} RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=ptime:20\r\n"));
        }
        Profile::Pcma | Profile::H264_720p30 | Profile::H265_1080p60 => {
            sdp.push_str(&format!("m=audio {audio_port} RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\na=ptime:20\r\n"));
        }
        Profile::G722 => {
            sdp.push_str(&format!("m=audio {audio_port} RTP/AVP 9\r\na=rtpmap:9 G722/8000\r\na=ptime:20\r\n"));
        }
        Profile::Opus => {
            sdp.push_str(&format!(
                "m=audio {audio_port} RTP/AVP 96\r\na=rtpmap:96 opus/48000/2\r\n\
                 a=fmtp:96 useinbandfec=1;stereo=0;sprop-stereo=0\r\na=ptime:20\r\n"
            ));
        }
        Profile::G726_32 => {
            sdp.push_str(&format!(
                "m=audio {audio_port} RTP/AVP 107\r\na=rtpmap:107 G726-32/8000\r\na=ptime:20\r\n"
            ));
        }
    }
    if let (Some(vp), Some(port)) = (profile.video(), video_port) {
        if vp.h265 {
            sdp.push_str(&format!(
                "m=video {port} RTP/AVP {}\r\na=rtpmap:{} H265/90000\r\na=fmtp:{} {}\r\n",
                vp.sdp_pt, vp.sdp_pt, vp.sdp_pt, vp.level_idc
            ));
        } else {
            sdp.push_str(&format!(
                "m=video {port} RTP/AVP {}\r\na=rtpmap:{} H264/90000\r\n\
                 a=fmtp:{} packetization-mode=1;profile-level-id={};max-mbps=108000;max-fs=3600\r\n",
                vp.sdp_pt, vp.sdp_pt, vp.sdp_pt, vp.level_idc
            ));
        }
    }
    sdp
}

/// Ответ UAS: 200 OK + SDP, где MCU объявляет свои порты и `c=` (candidate_addr).
#[derive(Debug, Clone)]
struct Answer {
    remote_audio: Option<SocketAddr>,
    remote_video: Option<SocketAddr>,
    /// PT, которым MCU будет присылать аудио (наш answer-совместимый PT).
    audio_pt_rx: u8,
    video_pt_rx: Option<u8>,
    has_video: bool,
}

fn parse_answer(sdp: &str, mcu_ip: &str) -> Option<Answer> {
    let mut c_ip = mcu_ip.to_string();
    let mut audio: Option<(u16, u8)> = None;
    let mut video: Option<(u16, u8)> = None;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("c=IN IP4 ") {
            c_ip = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("m=audio ") {
            let mut parts = rest.split_whitespace();
            let port = parts.next()?.parse::<u16>().ok()?;
            let port = if port == 0 { None } else { Some(port) };
            let pt = parts.find_map(|p| p.parse::<u8>().ok()).unwrap_or(8);
            audio = port.map(|p| (p, pt));
        } else if let Some(rest) = line.strip_prefix("m=video ") {
            let mut parts = rest.split_whitespace();
            let port = parts.next()?.parse::<u16>().ok()?;
            if port == 0 {
                video = None;
            } else {
                let pt = parts.find_map(|p| p.parse::<u8>().ok()).unwrap_or(96);
                video = Some((port, pt));
            }
        }
    }
    let ip: std::net::IpAddr = c_ip.parse().ok()?;
    Some(Answer {
        remote_audio: audio.map(|(port, pt)| (SocketAddr::new(ip, port), pt)).map(|(a, _)| a),
        remote_video: video.map(|(port, _)| SocketAddr::new(ip, port)),
        audio_pt_rx: audio.map(|(_, pt)| pt).unwrap_or(8),
        video_pt_rx: video.map(|(_, pt)| pt),
        has_video: video.is_some(),
    })
}

#[derive(Debug, Default)]
struct Stats {
    rx_packets: AtomicU64,
    rx_bytes: AtomicU64,
    lost: AtomicU64,
    expected: AtomicU64,
    tx_packets: AtomicU64,
    max_seq: AtomicU64,
}

struct CallCfg {
    target: SocketAddr,
    local_ip: String,
    room: String,
    index: usize,
    signal_port: u16,
    rtp_port: u16,
    profile: Profile,
    duration: Duration,
    video_file: Option<Arc<Vec<u8>>>,
}

/// Один вызов: сигналинг + два медиа-цикла.
async fn run_call(cfg: CallCfg) -> serde_json::Value {
    let stats = Arc::new(Stats::default());
    let mut out = serde_json::json!({
        "call": cfg.index,
        "room": cfg.room,
        "profile": format!("{:?}", cfg.profile),
        "disposition": "pending",
    });

    let bind = format!("{}:{}", cfg.local_ip, cfg.signal_port);
    let socket = match UdpSocket::bind(&bind).await {
        Ok(s) => s,
        Err(e) => {
            out["disposition"] = "bind_error".into();
            out["reason"] = e.to_string().into();
            return out;
        }
    };
    let video_port = if cfg.profile.video().is_some() { Some(cfg.rtp_port + 2) } else { None };
    let offer = build_offer(cfg.profile, &cfg.local_ip, cfg.rtp_port, video_port);

    let call_id = format!("bot-{}-{}", cfg.index, uuid::Uuid::new_v4().simple());
    let from_tag = format!("b{}", cfg.index);
    let branch = format!("z9hG4bK{}", uuid::Uuid::new_v4().simple());
    let target_uri = format!("{}:{}", cfg.target.ip(), cfg.target.port());
    let invite = vks_gateway_sip::sip_message::build_invite(
        &target_uri,
        &cfg.local_ip,
        cfg.signal_port,
        &call_id,
        &from_tag,
        &branch,
        &offer,
    );
    if let Err(e) = socket.send_to(invite.as_bytes(), cfg.target).await {
        out["disposition"] = "send_error".into();
        out["reason"] = e.to_string().into();
        return out;
    }

    // --- ожидание 200 OK -----------------------------------------------------
    let mut buf = vec![0u8; 65535];
    let answer = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut to_tag = String::new();
        loop {
            let recv = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await;
            let Ok(Ok((n, _peer))) = recv else {
                out["disposition"] = "timeout".into();
                return out;
            };
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let Some(resp) = vks_gateway_sip::sip_message::SipResponse::parse(&text) else { continue };
            if resp.status_code == 200 {
                to_tag = resp.to_tag.unwrap_or("").to_string();
                match parse_answer(resp.body, &cfg.target.ip().to_string()) {
                    Some(answer) => break answer,
                    None => {
                        out["disposition"] = "bad_sdp".into();
                        out["sdp"] = resp.body.into();
                        return out;
                    }
                }
            } else if (300..700).contains(&resp.status_code) {
                out["disposition"] = format!("{}", resp.status_code).into();
                out["reason"] = resp.reason.into();
                return out;
            }
            // 100/180 — ждём дальше
        }
        // ACK
        let ack = vks_gateway_sip::sip_message::build_ack(
            &target_uri,
            &cfg.local_ip,
            cfg.signal_port,
            &call_id,
            &from_tag,
            Some(&to_tag),
            &branch,
            true,
        );
        let _ = socket.send_to(ack.as_bytes(), cfg.target).await;
        out["to_tag"] = to_tag.into();
        out["has_video"] = serde_json::Value::Bool(false);
        out["sdp_answer"] = String::new().into();
        out["_answer_marker"] = serde_json::Value::Bool(true);
        out["disposition"] = "connected".into();
        // Заглушка-ветка: реальный код вернёт Answer из блока выше (см. TODO в шапке).
        return finish(out, stats, cfg).await;
    };
    let _ = answer;
}

async fn finish(mut out: serde_json::Value, stats: Arc<Stats>, cfg: CallCfg) -> serde_json::Value {
    tokio::time::sleep(cfg.duration).await;
    let expected = stats.expected.load(Ordering::Relaxed);
    let lost = stats.lost.load(Ordering::Relaxed);
    let total = expected + lost;
    out["rx_packets"] = stats.rx_packets.load(Ordering::Relaxed).into();
    out["rx_bytes"] = stats.rx_bytes.load(Ordering::Relaxed).into();
    out["tx_packets"] = stats.tx_packets.load(Ordering::Relaxed).into();
    out["lost"] = lost.into();
    out["loss_pct"] = if total == 0 { 0.0 } else { lost as f64 / total as f64 * 100.0 }.into();
    out
}

/// Аудио-цикл 20 мс: 160 байт PCMA/G.722/G.726 или 960 сэмплов Opus.
async fn audio_loop(sock: Arc<UdpSocket>, dest: SocketAddr, profile: Profile, stats: Arc<Stats>, stop: Arc<std::sync::atomic::AtomicBool>) {
    let mut tick = interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let ssrc: u32 = 0x1234_5678;
    let mut seq: u16 = 0;
    let mut ts: u32 = 0;
    let mut phase: f64 = 0.0;
    // 1 кГц тон, чтобы VAD/MOS видели сигнал, а не тишину.
    while !stop.load(Ordering::Relaxed) {
        tick.tick().await;
        let payload: Vec<u8> = match profile {
            Profile::Pcma => (0..160)
                .map(|_| {
                    let sample = (6000.0 * (phase).sin()) as i16;
                    phase += 2.0 * std::f64::consts::PI * 1000.0 / 8000.0;
                    vks_gateway_sip::rtp_bridge::linear_to_alaw(sample) // TODO: обёртка есть для ulaw; alaw добавить рядом
                })
                .collect(),
            Profile::G722 => {
                // 20 мс G.722 = 160 октетов; кодируем 320 сэмплов 16 кГц.
                let samples: Vec<i32> = (0..320)
                    .map(|n| {
                        let t = phase + n as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 16000.0;
                        (6000.0 * t.sin()) as i32
                    })
                    .collect();
                phase += 2.0 * std::f64::consts::PI * 1000.0 * 0.02;
                let mut codec = vks_gateway_sip::g722::G722Codec::new();
                codec.encode(&samples)
            }
            Profile::G726_32 => {
                let mut codec = vks_gateway_sip::g726::G726Codec::new(32);
                let samples: Vec<i16> = (0..160)
                    .map(|n| (6000.0 * (phase + n as f64 * 0.125).sin()) as i16)
                    .collect();
                phase += 2.0 * std::f64::consts::PI * 1000.0 * 0.02;
                codec.encode(&samples)
            }
            Profile::Opus => {
                let samples: Vec<i16> = (0..960)
                    .map(|n| {
                        let t = phase + n as f64 * 2.0 * std::f64::consts::PI * 1000.0 / 48000.0;
                        (6000.0 * t.sin()) as i16
                    })
                    .collect();
                phase += 2.0 * std::f64::consts::PI * 1000.0 * 0.02;
                match vks_gateway_sip::opus::OpusCodec::new() {
                    Some(mut codec) => codec.encode(&samples),
                    None => break,
                }
            }
            _ => break,
        };
        let mut packet = vec![0u8; 12 + payload.len()];
        vks_gateway_sip::rtp_bridge::write_rtp_header(&mut packet, 8, seq, ts, ssrc, false);
        packet[12..].copy_from_slice(&payload);
        if sock.send_to(&packet, dest).await.is_ok() {
            stats.tx_packets.fetch_add(1, Ordering::Relaxed);
        }
        seq = seq.wrapping_add(1);
        ts = ts.wrapping_add(match profile { Profile::Opus => 960, Profile::G722 => 160, _ => 160 });
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // TODO(после согласования): полный CLI через std::env::args, как в
    // scripts/bench/README.md; здесь — минимальный каркас, чтобы был виден
    // контракт и точки интеграции с обёртками кодеков крейта.
    let target: SocketAddr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:5060".into()).parse().expect("target");
    let profile = Profile::parse(&std::env::args().nth(2).unwrap_or_else(|| "m1".into())).expect("profile");
    println!("{{\"event\":\"start\",\"target\":\"{target}\",\"profile\":\"{profile:?}\"}}");
    let _ = build_offer(profile, "192.168.1.93", 20000, Some(20002));
    let _ = audio_loop;
    Ok(())
}

// ---------------------------------------------------------------------------
// TODO до первого прогона (в порядке приоритета):
// 1. Дочитать Answer и запустить audio_loop/video_loop (сейчас run_call
//    возвращает connected после ACK — каркас, не рабочий бот).
// 2. video_loop: Annex-B -> NAL (H.264: свой сплиттер ~20 строк; H.265:
//    vks_gateway_sip::h265::split_annexb уже есть), FU-A/STAP-A, MTU 1200,
//    marker на последнем пакете кадра, ts += 90000/fps.
// 3. Приём: parse_rtp_header + seq/jitter/loss (RFC 3550 A.8), RTCP RR
//    (RFC 3550 §6.4.1) с LSR/DLSR для RTT.
// 4. Отчёт: JSON-строка на вызов (как h323bot.py) + send_lag p99.
// 5. BYE при завершении; проверка ответа 200.
// 6. `linear_to_alaw` в rtp_bridge.rs отсутствует (есть только µ-law:
//    rtp_bridge.rs:73-121) — либо добавить рядом, либо использовать PCMU.
// ---------------------------------------------------------------------------

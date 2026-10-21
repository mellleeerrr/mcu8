//! H.264 RTP payload processing and isolated FFmpeg codec workers.

use super::audio::RtpPayload;
use super::{GatewayMediaControls, GatewayMediaStats};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

/// Профиль кодирования шлюзового видео. Заменяет прежние жёсткие
/// `640x360/15fps/256k/baseline/3.0`: оператор задаёт размер/битрейт/fps
/// через env (`VKS_GATEWAY_VIDEO_*`), а `max_frame_rate` из
/// `/api/v3/settings/video` подхватывается в `GatewayManager`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub profile: String,
    pub level: String,
}

impl Default for VideoProfile {
    fn default() -> Self {
        Self { width: 1280, height: 720, fps: 30, bitrate_kbps: 2048, profile: "baseline".into(), level: "3.1".into() }
    }
}

impl VideoProfile {
    /// Читает переопределения из env. Невалидные/нулевые значения игнорируются,
    /// чтобы опечатка в unit-файле не отключила видео.
    pub(super) fn from_env() -> Self {
        let mut profile = Self::default();
        profile.width = env_u32("VKS_GATEWAY_VIDEO_WIDTH").unwrap_or(profile.width);
        profile.height = env_u32("VKS_GATEWAY_VIDEO_HEIGHT").unwrap_or(profile.height);
        profile.fps = env_u32("VKS_GATEWAY_VIDEO_FPS").unwrap_or(profile.fps);
        profile.bitrate_kbps = env_u32("VKS_GATEWAY_VIDEO_BITRATE_KBPS").unwrap_or(profile.bitrate_kbps);
        if let Ok(value) = std::env::var("VKS_GATEWAY_VIDEO_PROFILE") {
            if !value.trim().is_empty() {
                profile.profile = value.trim().to_string();
            }
        }
        profile.width = (profile.width.clamp(160, 3840) / 2) * 2;
        profile.height = (profile.height.clamp(120, 2160) / 2) * 2;
        profile.fps = profile.fps.clamp(1, 60);
        profile.bitrate_kbps = profile.bitrate_kbps.clamp(64, 20_000);
        profile.level = std::env::var("VKS_GATEWAY_VIDEO_LEVEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| h264_level(profile.width, profile.height, profile.fps).to_string());
        profile
    }

    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.trim().parse().ok().filter(|value| *value > 0)
}

/// Минимальный H.264 level, покрывающий `width×height@fps` (уровень нужен
/// peer'у для декодера; занижать его нельзя).
fn h264_level(width: u32, height: u32, fps: u32) -> &'static str {
    let pixels_per_second = u64::from(width) * u64::from(height) * u64::from(fps);
    if width <= 1280 && height <= 720 && pixels_per_second <= 1280 * 720 * 30 {
        "3.1"
    } else if width <= 1920 && height <= 1088 && pixels_per_second <= 1920 * 1088 * 30 {
        "4.0"
    } else {
        "4.2"
    }
}

pub(super) struct VideoBridgeSession {
    pub room_id: vks_core::RoomId,
    pub participant_id: vks_core::ParticipantId,
    pub rtp_socket: tokio::net::UdpSocket,
    pub rtcp_socket: Option<tokio::net::UdpSocket>,
    pub remote_target: std::net::SocketAddr,
    pub payload_type: u8,
    pub receive_payload_type: u8,
    pub port_pool: Arc<vks_gateway_sip::RtpPortPool>,
    pub engine: Arc<vks_media_mcu::McuEngine>,
    pub controls: GatewayMediaControls,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub stats: Arc<GatewayMediaStats>,
    pub profile: VideoProfile,
}

pub(super) async fn codec_available() -> bool {
    static AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
    *AVAILABLE
        .get_or_init(|| async {
            match tokio::time::timeout(
                Duration::from_secs(3),
                Command::new("ffmpeg").args(["-hide_banner", "-encoders"]).output(),
            )
            .await
            {
                Ok(Ok(output)) => {
                    output.status.success() && String::from_utf8_lossy(&output.stdout).contains("libx264")
                }
                _ => false,
            }
        })
        .await
}

fn spawn_codec(encoder: bool, profile: &VideoProfile) -> Result<Child, String> {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "1", "-filter_threads", "1"]);
    if encoder {
        let size = format!("{}x{}", profile.width, profile.height);
        let fps = profile.fps.to_string();
        let bitrate = format!("{}k", profile.bitrate_kbps);
        let buffer = format!("{}k", profile.bitrate_kbps.saturating_mul(2));
        let keyint = profile.fps.to_string();
        command.args([
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv420p",
            "-video_size",
            size.as_str(),
            "-framerate",
            fps.as_str(),
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
            "-profile:v",
            profile.profile.as_str(),
            "-level:v",
            profile.level.as_str(),
            "-b:v",
            bitrate.as_str(),
            "-maxrate",
            bitrate.as_str(),
            "-bufsize",
            buffer.as_str(),
            "-g",
            keyint.as_str(),
            "-keyint_min",
            keyint.as_str(),
            "-x264-params",
            "repeat-headers=1:aud=1:scenecut=0",
            "-flush_packets",
            "1",
            "-f",
            "h264",
            "pipe:1",
        ]);
    } else {
        let scale = format!("scale={}:{}", profile.width, profile.height);
        command.args([
            "-flags",
            "low_delay",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-max_pixels",
            "2073600",
            "-f",
            "h264",
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            scale.as_str(),
            "-threads",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "passthrough",
            "-flush_packets",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command
        .spawn()
        .map_err(|error| format!("Cannot start FFmpeg H264 {}: {error}", if encoder { "encoder" } else { "decoder" }))
}

fn drain_errors(child: &mut Child, kind: &'static str) -> tokio::task::JoinHandle<()> {
    let mut stderr = child.stderr.take().expect("piped codec stderr");
    tokio::spawn(async move {
        let mut buffer = [0u8; 1024];
        let mut reported = 0;
        while let Ok(count) = stderr.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            if reported < 4 {
                tracing::warn!(codec = kind, message = %String::from_utf8_lossy(&buffer[..count]), "FFmpeg codec diagnostic");
                reported += 1;
            }
        }
    })
}

pub(super) async fn spawn_video_media_bridge(
    session: VideoBridgeSession,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let local_port = session.rtp_socket.local_addr().map_err(|error| error.to_string())?.port();
    let lease = super::RtpPortLease::new(session.port_pool.clone(), local_port);
    if !codec_available().await {
        return Err("FFmpeg with libx264 is unavailable; video was not started".into());
    }
    let mut encoder = spawn_codec(true, &session.profile)?;
    let mut decoder = match spawn_codec(false, &session.profile) {
        Ok(child) => child,
        Err(error) => {
            let _ = encoder.kill().await;
            return Err(error);
        }
    };
    let encoder_errors = drain_errors(&mut encoder, "h264-encoder");
    let decoder_errors = drain_errors(&mut decoder, "h264-decoder");
    let mut encoder_stdin = encoder.stdin.take().expect("encoder stdin");
    let mut encoder_stdout = encoder.stdout.take().expect("encoder stdout");
    let mut decoder_stdin = decoder.stdin.take().expect("decoder stdin");
    let mut decoder_stdout = decoder.stdout.take().expect("decoder stdout");
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (access_tx, mut access_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
    let rtp_socket = Arc::new(session.rtp_socket);
    let worker_failed = Arc::new(AtomicBool::new(false));
    let ssrc = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
    let last_timestamp = Arc::new(AtomicU32::new(0));

    let encoder_input_failed = worker_failed.clone();
    let encoder_input = tokio::spawn(async move {
        while let Some(frame) = frames_rx.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(1), encoder_stdin.write_all(&frame)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        encoder_input_failed.store(true, Relaxed);
    });
    let decoder_input_failed = worker_failed.clone();
    let decoder_input = tokio::spawn(async move {
        while let Some(access_unit) = access_rx.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(1), decoder_stdin.write_all(&access_unit)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        decoder_input_failed.store(true, Relaxed);
    });
    let decoded_engine = session.engine.clone();
    let decoded_stats = session.stats.clone();
    let decoded_controls = session.controls.clone();
    let room = session.room_id;
    let participant = session.participant_id;
    let video_width = session.profile.width;
    let video_height = session.profile.height;
    let frame_bytes = session.profile.frame_bytes();
    let decoder_output_failed = worker_failed.clone();
    let decoder_output = tokio::spawn(async move {
        loop {
            let mut bytes = vec![0; frame_bytes];
            if decoder_stdout.read_exact(&mut bytes).await.is_err() {
                break;
            }
            if !decoded_controls.video_tx_muted.load(Relaxed)
                && decoded_engine.set_participant_frame(
                    room,
                    participant,
                    vks_media_mcu::frame::Frame::new(video_width, video_height, bytes),
                )
            {
                decoded_stats.decoded_video_frames.fetch_add(1, Relaxed);
            }
        }
        decoder_output_failed.store(true, Relaxed);
    });
    let outgoing_socket = rtp_socket.clone();
    let outgoing_stats = session.stats.clone();
    let outgoing_controls = session.controls.clone();
    let outgoing_timestamp = last_timestamp.clone();
    let target = session.remote_target;
    let payload_type = session.payload_type;
    let encoder_output_failed = worker_failed.clone();
    let encoder_output = tokio::spawn(async move {
        let epoch = std::time::Instant::now();
        let mut splitter = AnnexB::default();
        let mut pending = Vec::new();
        let mut sequence = 1u16;
        let mut buffer = [0; 16384];
        while let Ok(count) = encoder_stdout.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            for nal in splitter.push(&buffer[..count]) {
                if nal[0] & 31 == 9 && !pending.is_empty() {
                    let timestamp = (epoch.elapsed().as_micros() * 90 / 1000) as u32;
                    if !outgoing_controls.video_rx_muted.load(Relaxed) {
                        let packets = packetize_h264(&pending, payload_type, ssrc, &mut sequence, timestamp);
                        let mut successful = false;
                        for packet in packets {
                            if let Ok(sent) = outgoing_socket.send_to(&packet, target).await {
                                outgoing_stats.video_tx_packets.fetch_add(1, Relaxed);
                                outgoing_stats.video_tx_bytes.fetch_add(sent as u64, Relaxed);
                                successful = true;
                            }
                        }
                        if successful {
                            outgoing_stats.encoded_video_frames.fetch_add(1, Relaxed);
                        }
                    }
                    outgoing_timestamp.store(timestamp, Relaxed);
                    pending.clear();
                }
                if pending.len() >= 256 || pending.iter().map(Vec::len).sum::<usize>() + nal.len() > MAX_NAL_BYTES {
                    pending.clear();
                    outgoing_stats.dropped_video_frames.fetch_add(1, Relaxed);
                }
                pending.push(nal);
            }
        }
        encoder_output_failed.store(true, Relaxed);
    });
    let render_engine = session.engine.clone();
    let render_cancelled = session.cancelled.clone();
    let render_stats = session.stats.clone();
    let render_profile = session.profile.clone();
    let render = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_micros(66_667));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !render_cancelled.load(Relaxed) {
            interval.tick().await;
            let engine = render_engine.clone();
            let render_profile = render_profile.clone();
            let rendered = tokio::task::spawn_blocking(move || {
                let frame = engine
                    .tick_video_at(room, super::audio::media_clock_ms())
                    .or_else(|| engine.composite_frame(room))?;
                Some(resize_i420(&frame, &render_profile))
            })
            .await;
            if let Ok(Some(bytes)) = rendered {
                if frames_tx.try_send(bytes).is_err() {
                    render_stats.dropped_video_frames.fetch_add(1, Relaxed);
                }
            }
        }
    });
    session.stats.video_codec_running.store(true, Relaxed);
    Ok(tokio::spawn(async move {
        let _lease = lease;
        let mut source = super::audio::RtpSource::new(session.remote_target, session.receive_payload_type);
        let mut depacketizer = H264Depacketizer::default();
        let mut pending_access = Vec::new();
        let mut last_access_ts = None;
        let mut receive = [0u8; 65536];
        let mut interval = tokio::time::interval(Duration::from_millis(20));
        let mut last_rtcp = std::time::Instant::now() - Duration::from_secs(5);
        while !session.cancelled.load(Relaxed) && !worker_failed.load(Relaxed) {
            tokio::select! {
                _ = interval.tick() => {
                    if last_rtcp.elapsed() >= Duration::from_secs(1) {
                        if let Some(socket) = &session.rtcp_socket {
                            if let Some(port) = session.remote_target.port().checked_add(1) {
                                let destination = std::net::SocketAddr::new(session.remote_target.ip(), port);
                                let report = sender_report(ssrc, last_timestamp.load(Relaxed), &session.stats);
                                let _ = socket.send_to(&report, destination).await;
                                if let Some(remote_ssrc) = source.ssrc() {
                                    let mut pli = vec![0x81, 206, 0, 2];
                                    pli.extend_from_slice(&ssrc.to_be_bytes());
                                    pli.extend_from_slice(&remote_ssrc.to_be_bytes());
                                    let _ = socket.send_to(&pli, destination).await;
                                }
                            }
                        }
                        last_rtcp = std::time::Instant::now();
                    }
                    if encoder.try_wait().ok().flatten().is_some() || decoder.try_wait().ok().flatten().is_some() { break; }
                }
                received = rtp_socket.recv_from(&mut receive) => {
                    let Ok((count, address)) = received else { break; };
                    let Some(packet) = source.accept(address, &receive[..count]) else {
                        session.stats.rejected_packets.fetch_add(1, Relaxed); continue;
                    };
                    session.stats.video_rx_packets.fetch_add(1, Relaxed);
                    session.stats.video_rx_bytes.fetch_add(count as u64, Relaxed);
                    if session.controls.video_tx_muted.load(Relaxed) { continue; }
                    if last_access_ts.is_some_and(|timestamp| timestamp != packet.timestamp) { pending_access.clear(); }
                    last_access_ts = Some(packet.timestamp);
                    let marker = packet.marker;
                    match depacketizer.push(packet) {
                        Ok(units) => {
                            for nal in units {
                                if pending_access.len() + nal.len() + 4 > MAX_NAL_BYTES { pending_access.clear(); break; }
                                pending_access.extend_from_slice(&[0, 0, 0, 1]); pending_access.extend(nal);
                            }
                            if marker && !pending_access.is_empty() {
                                pending_access.extend_from_slice(&[0, 0, 0, 1, 9, 0xf0]);
                                if access_tx.try_send(std::mem::take(&mut pending_access)).is_err() {
                                    session.stats.dropped_video_frames.fetch_add(1, Relaxed);
                                }
                            }
                        }
                        Err(()) => { pending_access.clear(); session.stats.rejected_packets.fetch_add(1, Relaxed); }
                    }
                }
            }
        }
        for task in
            [encoder_input, decoder_input, decoder_output, encoder_output, render, encoder_errors, decoder_errors]
        {
            task.abort();
            let _ = task.await;
        }
        let _ = encoder.kill().await;
        let _ = encoder.wait().await;
        let _ = decoder.kill().await;
        let _ = decoder.wait().await;
        session.stats.video_codec_running.store(false, Relaxed);
        tracing::info!(participant = %session.participant_id, "H264 media bridge and codec workers stopped");
    }))
}

fn resize_i420(frame: &vks_media_mcu::frame::Frame, profile: &VideoProfile) -> Vec<u8> {
    let width = profile.width as usize;
    let height = profile.height as usize;
    let mut output = vec![128; profile.frame_bytes()];
    output[..width * height].fill(16);
    if frame.width == 0 || frame.height == 0 {
        return output;
    }
    let source_width = frame.width as usize;
    let source_height = frame.height as usize;
    for plane in 0..if frame.is_yuv420p() { 3 } else { 1 } {
        let divisor = if plane == 0 { 1 } else { 2 };
        let plane_width = width / divisor;
        let plane_height = height / divisor;
        let source_stride = source_width / divisor;
        let source_rows = source_height / divisor;
        let source_offset =
            if plane == 0 { 0 } else { source_width * source_height + (plane - 1) * source_width * source_height / 4 };
        let output_offset = if plane == 0 { 0 } else { width * height + (plane - 1) * width * height / 4 };
        for y in 0..plane_height {
            let source_y = y * source_rows / plane_height;
            for x in 0..plane_width {
                let source_x = x * source_stride / plane_width;
                output[output_offset + y * plane_width + x] =
                    frame.data.get(source_offset + source_y * source_stride + source_x).copied().unwrap_or(16);
            }
        }
    }
    output
}

#[derive(Default)]
struct AnnexB {
    buffer: Vec<u8>,
}

impl AnnexB {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_NAL_BYTES {
            self.buffer.clear();
            return Vec::new();
        }
        let mut units = Vec::new();
        loop {
            let Some((first, prefix)) = find_start_code(&self.buffer, 0) else {
                break;
            };
            let Some((next, _)) = find_start_code(&self.buffer, first + prefix) else {
                if first > 0 {
                    self.buffer.drain(..first);
                }
                break;
            };
            if next > first + prefix {
                units.push(self.buffer[first + prefix..next].to_vec());
            }
            self.buffer.drain(..next);
        }
        units
    }
}

fn find_start_code(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    (start..bytes.len().saturating_sub(2)).find_map(|position| {
        if bytes.get(position..position + 4) == Some(&[0, 0, 0, 1][..]) {
            Some((position, 4))
        } else if bytes.get(position..position + 3) == Some(&[0, 0, 1][..]) {
            Some((position, 3))
        } else {
            None
        }
    })
}

fn packetize_h264(units: &[Vec<u8>], payload_type: u8, ssrc: u32, sequence: &mut u16, timestamp: u32) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for (index, nal) in units.iter().enumerate() {
        if nal.is_empty() {
            continue;
        }
        if nal.len() <= 1188 {
            let mut packet = vec![0; 12];
            vks_gateway_sip::write_rtp_header(
                &mut packet,
                payload_type,
                *sequence,
                timestamp,
                ssrc,
                index + 1 == units.len(),
            );
            *sequence = sequence.wrapping_add(1);
            packet.extend_from_slice(nal);
            packets.push(packet);
        } else {
            let chunks: Vec<&[u8]> = nal[1..].chunks(1186).collect();
            for (part, chunk) in chunks.iter().enumerate() {
                let last = part + 1 == chunks.len();
                let mut packet = vec![0; 12];
                vks_gateway_sip::write_rtp_header(
                    &mut packet,
                    payload_type,
                    *sequence,
                    timestamp,
                    ssrc,
                    last && index + 1 == units.len(),
                );
                *sequence = sequence.wrapping_add(1);
                packet.push((nal[0] & 0xe0) | 28);
                packet.push(
                    (nal[0] & 31)
                        | if part == 0 {
                            0x80
                        } else if last {
                            0x40
                        } else {
                            0
                        },
                );
                packet.extend_from_slice(chunk);
                packets.push(packet);
            }
        }
    }
    packets
}

fn sender_report(ssrc: u32, timestamp: u32, stats: &GatewayMediaStats) -> Vec<u8> {
    super::audio::rtcp_sender_report(
        ssrc,
        timestamp,
        stats.video_tx_packets.load(Relaxed),
        stats.video_tx_bytes.load(Relaxed),
    )
}

const MAX_NAL_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct H264Depacketizer {
    fragment: Vec<u8>,
    last_sequence: u16,
    timestamp: u32,
}

impl H264Depacketizer {
    fn push(&mut self, packet: RtpPayload<'_>) -> Result<Vec<Vec<u8>>, ()> {
        let data = packet.payload;
        let Some(&header) = data.first() else {
            return Err(());
        };
        if header & 0x80 != 0 || data.len() > MAX_NAL_BYTES {
            self.fragment.clear();
            return Err(());
        }
        match header & 31 {
            1..=23 => {
                self.fragment.clear();
                Ok(vec![data.to_vec()])
            }
            24 => {
                self.fragment.clear();
                let mut position = 1;
                let mut units = Vec::new();
                while position < data.len() {
                    if position + 2 > data.len() {
                        return Err(());
                    }
                    let count = usize::from(u16::from_be_bytes([data[position], data[position + 1]]));
                    position += 2;
                    if count == 0 || position + count > data.len() {
                        return Err(());
                    }
                    let unit = &data[position..position + count];
                    if unit[0] & 0x80 != 0 || !(1..=23).contains(&(unit[0] & 31)) {
                        return Err(());
                    }
                    units.push(unit.to_vec());
                    position += count;
                }
                if units.is_empty() {
                    Err(())
                } else {
                    Ok(units)
                }
            }
            28 => {
                if data.len() < 3 {
                    self.fragment.clear();
                    return Err(());
                }
                let fu = data[1];
                let start = fu & 0x80 != 0;
                let end = fu & 0x40 != 0;
                let reconstructed = (header & 0xe0) | (fu & 31);
                if fu & 0x20 != 0 || !(1..=23).contains(&(fu & 31)) || start && end {
                    self.fragment.clear();
                    return Err(());
                }
                if start {
                    self.fragment.clear();
                    self.fragment.push(reconstructed);
                    self.timestamp = packet.timestamp;
                } else if self.fragment.is_empty()
                    || packet.sequence != self.last_sequence.wrapping_add(1)
                    || packet.timestamp != self.timestamp
                    || self.fragment[0] != reconstructed
                {
                    self.fragment.clear();
                    return Err(());
                }
                self.last_sequence = packet.sequence;
                if self.fragment.len() + data.len() - 2 > MAX_NAL_BYTES {
                    self.fragment.clear();
                    return Err(());
                }
                self.fragment.extend_from_slice(&data[2..]);
                if end {
                    Ok(vec![std::mem::take(&mut self.fragment)])
                } else {
                    Ok(Vec::new())
                }
            }
            _ => {
                self.fragment.clear();
                Err(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires ffmpeg with libx264"]
    async fn ffmpeg_encoder_sps_respects_negotiated_level_30() {
        let profile = VideoProfile { level: "3.0".into(), ..VideoProfile::default() };
        let mut encoder = spawn_codec(true, &profile).unwrap();
        let mut stdin = encoder.stdin.take().unwrap();
        let mut stdout = encoder.stdout.take().unwrap();
        let mut frame = vec![128; profile.frame_bytes()];
        frame[..(profile.width * profile.height) as usize].fill(76);
        stdin.write_all(&frame).await.unwrap();
        stdin.write_all(&frame).await.unwrap();
        drop(stdin);
        let mut encoded = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), stdout.read_to_end(&mut encoded)).await.unwrap().unwrap();
        assert!(encoder.wait().await.unwrap().success());
        let mut splitter = AnnexB::default();
        let units = splitter.push(&encoded);
        let sps = units.iter().find(|unit| unit[0] & 31 == 7).expect("real encoder SPS");
        assert_eq!(sps[1], 66, "encoder must use Baseline");
        assert_eq!(sps[3], 30, "SPS must not advertise3.1 when peer accepted3.0");
    }

    fn payload(data: &[u8], sequence: u16, timestamp: u32) -> RtpPayload<'_> {
        RtpPayload { payload: data, sequence, timestamp, marker: false }
    }

    #[test]
    fn h264_fu_a_reassembles_only_consecutive_fragments_of_the_same_frame() {
        let mut decoder = H264Depacketizer::default();
        assert!(decoder.push(payload(&[0x7c, 0x85, 1, 2], 10, 9000)).unwrap().is_empty());
        assert_eq!(decoder.push(payload(&[0x7c, 0x45, 3, 4], 11, 9000)).unwrap(), vec![vec![0x65, 1, 2, 3, 4]]);
        assert!(decoder.push(payload(&[0x7c, 0x85, 1], 12, 18000)).unwrap().is_empty());
        assert!(
            decoder.push(payload(&[0x7c, 0x45, 2], 14, 18000)).is_err(),
            "lost fragments must never feed a corrupt NAL to decoder"
        );
    }

    #[test]
    fn h264_stap_a_extracts_sps_and_pps_with_strict_lengths() {
        let mut decoder = H264Depacketizer::default();
        assert_eq!(
            decoder.push(payload(&[0x78, 0, 2, 0x67, 1, 0, 2, 0x68, 2], 1, 0)).unwrap(),
            vec![vec![0x67, 1], vec![0x68, 2]]
        );
        assert!(decoder.push(payload(&[0x78, 0, 10, 0x67], 2, 0)).is_err());
    }

    #[tokio::test]
    #[ignore = "requires installed ffmpeg with libx264; sends only loopback RTP"]
    async fn ffmpeg_h264_rtp_roundtrip_preserves_color_and_stops_workers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        let sender_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let receiver_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let room = vks_core::RoomId::new();
        let sender = vks_core::ParticipantId::new();
        let receiver = vks_core::ParticipantId::new();
        for (engine, pid) in [(&sender_engine, sender), (&receiver_engine, receiver)] {
            engine.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        let pixels = (VideoProfile::default().width * VideoProfile::default().height) as usize;
        let mut red = vec![76; pixels];
        red.extend(vec![85; pixels / 4]);
        red.extend(vec![255; pixels / 4]);
        assert!(sender_engine.set_participant_frame(
            room,
            sender,
            vks_media_mcu::frame::Frame::new(VideoProfile::default().width, VideoProfile::default().height, red)
        ));
        let sender_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_address = sender_socket.local_addr().unwrap();
        let receiver_address = receiver_socket.local_addr().unwrap();
        let sender_stats = Arc::new(GatewayMediaStats::default());
        let receiver_stats = Arc::new(GatewayMediaStats::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let sender_controls = GatewayMediaControls::default();
        sender_controls.video_tx_muted.store(true, Ordering::Relaxed);
        let receiver_controls = GatewayMediaControls::default();
        receiver_controls.video_rx_muted.store(true, Ordering::Relaxed);
        let sender_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: sender,
            rtp_socket: sender_socket,
            rtcp_socket: None,
            remote_target: receiver_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: sender_engine,
            controls: sender_controls,
            cancelled: cancelled.clone(),
            stats: sender_stats.clone(),
            profile: VideoProfile::default(),
        })
        .await
        .expect("real H264 encoder/decoder must start");
        let receiver_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: receiver,
            rtp_socket: receiver_socket,
            rtcp_socket: None,
            remote_target: sender_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: receiver_engine.clone(),
            controls: receiver_controls,
            cancelled: cancelled.clone(),
            stats: receiver_stats.clone(),
            profile: VideoProfile::default(),
        })
        .await
        .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(7), async {
            while receiver_stats.decoded_video_frames.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        cancelled.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(3), sender_task).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), receiver_task).await.unwrap().unwrap();
        assert!(received.is_ok(), "no actual H264 video was decoded from loopback RTP");
        assert!(sender_stats.encoded_video_frames.load(Ordering::Relaxed) > 0);
        let frame = receiver_engine.tick_video_at(room, u64::MAX / 2).unwrap();
        assert!(frame.is_yuv420p());
        let pixels = frame.width as usize * frame.height as usize;
        assert!((frame.data[pixels + pixels / 8] as i16 - 85).abs() < 8, "U color plane lost");
        assert!(frame.data[pixels * 5 / 4 + pixels / 8] > 230, "V color plane lost");
    }
}

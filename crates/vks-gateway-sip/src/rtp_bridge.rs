//! Plain RTP bridge, port allocation, G.711 codec and SDP parsing for hardware terminals (RFC 3550 / RFC 4566).

use std::collections::BTreeSet;
use std::sync::Mutex;
use tokio::net::UdpSocket;

/// Выделенная пара UDP-сокетов (RTP четный, RTCP нечетный).
pub struct PortPair {
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub rtp_socket: UdpSocket,
    pub rtcp_socket: UdpSocket,
}

/// Пул портов RTP/RTCP (RFC 3550 §11: четные порты для RTP).
pub struct RtpPortPool {
    free_ports: Mutex<BTreeSet<u16>>,
}

impl Default for RtpPortPool {
    fn default() -> Self {
        Self::new(41000, 41200)
    }
}

impl RtpPortPool {
    pub fn new(start: u16, end: u16) -> Self {
        let mut set = BTreeSet::new();
        let mut p = if start.is_multiple_of(2) { start } else { start + 1 };
        while p < end {
            set.insert(p);
            p += 2;
        }
        Self { free_ports: Mutex::new(set) }
    }

    /// Выделяет пару свободных сокетов (RTP + RTCP) с привязкой к адресу.
    pub async fn allocate_pair(&self, bind_ip: &str) -> Option<PortPair> {
        let ports: Vec<u16> = {
            let lock = self.free_ports.lock().ok()?;
            lock.iter().copied().collect()
        };

        for port in ports {
            let rtp_addr = format!("{bind_ip}:{port}");
            let rtcp_addr = format!("{bind_ip}:{}", port + 1);
            if let Ok(rtp) = UdpSocket::bind(&rtp_addr).await {
                if let Ok(rtcp) = UdpSocket::bind(&rtcp_addr).await {
                    if let Ok(mut lock) = self.free_ports.lock() {
                        lock.remove(&port);
                    }
                    return Some(PortPair { rtp_port: port, rtcp_port: port + 1, rtp_socket: rtp, rtcp_socket: rtcp });
                }
            }
        }
        None
    }

    /// Возвращает порт в пул.
    pub fn release_port(&self, port: u16) {
        if let Ok(mut lock) = self.free_ports.lock() {
            let even = if port.is_multiple_of(2) { port } else { port - 1 };
            lock.insert(even);
        }
    }
}

// ---------------------------------------------------------------------------
// G.711 µ-law & A-law Codecs + Resampling
// ---------------------------------------------------------------------------

/// Кодирует 16-битный PCM сэмпл в G.711 µ-law байт.
pub fn linear_to_ulaw(sample: i16) -> u8 {
    let sign = (sample >> 8) & 0x80;
    let mut abs = if sample < 0 { -sample } else { sample };
    if abs > 32635 {
        abs = 32635;
    }
    abs += 0x84;

    let mut exponent = 7;
    let mut mask = 0x4000;
    while (abs & mask) == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (abs >> (exponent + 3)) & 0x0F;
    let byte = (sign | (exponent << 4) | mantissa) as u8;
    !byte
}

/// Декодирует G.711 µ-law байт в linear PCM сэмпл (f32 в диапазоне [-1.0, 1.0]).
pub fn ulaw_to_f32(byte: u8) -> f32 {
    let byte = !byte;
    let sign = (byte & 0x80) != 0;
    let exponent = ((byte >> 4) & 0x07) as i32;
    let mantissa = (byte & 0x0F) as i32;

    let mut sample = ((mantissa << 3) + 0x84) << exponent;
    sample -= 0x84;
    let i_sample = if sign { -sample } else { sample };
    (i_sample as f32) / 32768.0
}

/// Декодирует массив G.711 µ-law байтов в сэмплы f32.
pub fn decode_ulaw(src: &[u8], dst: &mut [f32]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = ulaw_to_f32(*s);
    }
}

/// Кодирует массив f32 [-1.0, 1.0] в G.711 µ-law байты.
pub fn encode_ulaw(src: &[f32], dst: &mut [u8]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let clamped = s.clamp(-1.0, 1.0);
        let sample = (clamped * 32767.0) as i16;
        *d = linear_to_ulaw(sample);
    }
}

/// Понижает частоту дискретизации с 48 кГц до 8 кГц (6:1 усреднение).
pub fn downsample_48k_to_8k(src: &[f32], dst: &mut [f32; 160]) {
    dst.fill(0.0);
    let chunks = (src.len() / 6).min(160);
    for i in 0..chunks {
        let chunk = &src[i * 6..(i + 1) * 6];
        let sum: f32 = chunk.iter().sum();
        dst[i] = sum / 6.0;
    }
}

/// Повышает частоту дискретизации с 8 кГц до 48 кГц (линейная интерполяция 1:6).
pub fn upsample_8k_to_48k(src: &[f32; 160], dst: &mut [f32; 960]) {
    for i in 0..159 {
        let s0 = src[i];
        let s1 = src[i + 1];
        let step = (s1 - s0) / 6.0;
        for k in 0..6 {
            dst[i * 6 + k] = s0 + step * (k as f32);
        }
    }
    let last = src[159];
    for k in 0..6 {
        dst[954 + k] = last;
    }
}

// ---------------------------------------------------------------------------
// RFC 3550 RTP Header Serializer / Deserializer
// ---------------------------------------------------------------------------

/// Записывает 12-байтовый RTP заголовок (RFC 3550).
pub fn write_rtp_header(buf: &mut [u8], pt: u8, seq: u16, ts: u32, ssrc: u32, marker: bool) {
    assert!(buf.len() >= 12, "buffer too small for RTP header");
    buf[0] = 0x80; // V=2, P=0, X=0, CC=0
    buf[1] = (if marker { 0x80 } else { 0x00 }) | (pt & 0x7F);
    buf[2..4].copy_from_slice(&seq.to_be_bytes());
    buf[4..8].copy_from_slice(&ts.to_be_bytes());
    buf[8..12].copy_from_slice(&ssrc.to_be_bytes());
}

/// Разбирает базовый RTP заголовок. Возвращает (payload_type, seq, ts, ssrc, payload_offset).
pub fn parse_rtp_header(packet: &[u8]) -> Option<(u8, u16, u32, u32, usize)> {
    if packet.len() < 12 {
        return None;
    }
    let v = (packet[0] >> 6) & 0x03;
    if v != 2 {
        return None;
    }
    let cc = (packet[0] & 0x0F) as usize;
    let pt = packet[1] & 0x7F;
    let seq = u16::from_be_bytes([packet[2], packet[3]]);
    let ts = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);

    let header_len = 12 + cc * 4;
    if packet.len() < header_len {
        return None;
    }
    Some((pt, seq, ts, ssrc, header_len))
}

// ---------------------------------------------------------------------------
// Plain SDP Generation & Parsing
// ---------------------------------------------------------------------------

/// Формирует стандартный plain RTP SDP offer для аппаратного терминала.
pub fn build_plain_sdp(local_ip: &str, audio_port: u16, video_port: Option<u16>) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

    let mut sdp = format!(
        "v=0\r\n\
o=VKS-MCU {now} 1 IN IP4 {local_ip}\r\n\
s=VKS Conference\r\n\
c=IN IP4 {local_ip}\r\n\
t=0 0\r\n\
m=audio {audio_port} RTP/AVP 0 8 101\r\n\
a=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
a=fmtp:101 0-16\r\n\
a=sendrecv\r\n\
a=rtcp:{}\r\n",
        audio_port + 1
    );

    if let Some(vport) = video_port {
        sdp.push_str(&format!(
            "m=video {vport} RTP/AVP 96\r\n\
a=rtpmap:96 H264/90000\r\n\
a=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\n\
a=sendrecv\r\n\
a=rtcp:{}\r\n",
            vport + 1
        ));
    }

    sdp
}

/// Извлекает IP и медиа-порты (audio, video) из SDP answer удаленного терминала.
pub fn parse_remote_media_ports(sdp: &str) -> (Option<String>, Option<u16>, Option<u16>) {
    let mut remote_ip = None;
    let mut audio_port = None;
    let mut video_port = None;

    for line in sdp.lines() {
        let trimmed = line.trim();
        if let Some(c_line) = trimmed.strip_prefix("c=IN IP4 ") {
            if remote_ip.is_none() {
                remote_ip = Some(c_line.trim().to_string());
            }
        } else if let Some(m_line) = trimmed.strip_prefix("m=") {
            let mut parts = m_line.split_whitespace();
            if let (Some(kind), Some(port_str)) = (parts.next(), parts.next()) {
                if let Ok(port) = port_str.parse::<u16>() {
                    if kind == "audio" {
                        audio_port = Some(port);
                    } else if kind == "video" {
                        video_port = Some(port);
                    }
                }
            }
        }
    }

    (remote_ip, audio_port, video_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_ulaw_roundtrip() {
        let samples = [0.0f32, 0.5, -0.5, 0.99, -0.99];
        let mut encoded = [0u8; 5];
        let mut decoded = [0.0f32; 5];
        encode_ulaw(&samples, &mut encoded);
        decode_ulaw(&encoded, &mut decoded);

        for (orig, dec) in samples.iter().zip(decoded.iter()) {
            assert!((orig - dec).abs() < 0.05, "diff too large: {orig} vs {dec}");
        }
    }

    #[test]
    fn resampling_cadence() {
        let mut src_8k = [0.0f32; 160];
        // 200 Hz fundamental tone at 8 kHz
        for (i, sample) in src_8k.iter_mut().enumerate() {
            *sample = ((i as f32) * 2.0 * std::f32::consts::PI * 200.0 / 8000.0).sin();
        }
        let mut dst_48k = [0.0f32; 960];
        upsample_8k_to_48k(&src_8k, &mut dst_48k);
        assert_eq!(dst_48k.len(), 960);

        let mut roundtrip_8k = [0.0f32; 160];
        downsample_48k_to_8k(&dst_48k, &mut roundtrip_8k);
        for i in 1..159 {
            assert!((src_8k[i] - roundtrip_8k[i]).abs() < 0.2);
        }
    }

    #[test]
    fn rtp_header_roundtrip() {
        let mut buf = [0u8; 12];
        write_rtp_header(&mut buf, 0, 100, 16000, 0x12345678, true);
        let (pt, seq, ts, ssrc, offset) = parse_rtp_header(&buf).expect("parse rtp");
        assert_eq!(pt, 0);
        assert_eq!(seq, 100);
        assert_eq!(ts, 16000);
        assert_eq!(ssrc, 0x12345678);
        assert_eq!(offset, 12);
    }

    #[test]
    fn sdp_parse_answer() {
        let sdp = "v=0\r\n\
o=term 1 1 IN IP4 192.168.1.132\r\n\
c=IN IP4 192.168.1.132\r\n\
m=audio 5004 RTP/AVP 0\r\n\
m=video 5006 RTP/AVP 96\r\n";
        let (ip, a_port, v_port) = parse_remote_media_ports(sdp);
        assert_eq!(ip, Some("192.168.1.132".to_string()));
        assert_eq!(a_port, Some(5004));
        assert_eq!(v_port, Some(5006));
    }
}

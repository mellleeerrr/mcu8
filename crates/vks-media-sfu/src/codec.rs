//! Codec feature flags (ADR-0017, CODE_STYLE §5).
//!
//! Features `opus`, `vp8`, `vp9`, `av1` (default: opus, vp8) gates
//! `CodecRegistry` entries. Каждый enabled feature → один entry.
//!
//! SFU не декодирует (forwarding) — registry для SDP offer/answer
//! (фаз3) и RoomCaps.

use std::fmt;

/// Медиа-класс кодека.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    /// Audio.
    Audio,
    /// Video.
    Video,
}

/// Запись codec-registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecEntry {
    /// Codec name (wire, lower-case).
    pub name: &'static str,
    /// Media class.
    pub kind: CodecKind,
}

impl CodecEntry {
    /// Name + kind.
    const OPUS: Self = Self { name: "opus", kind: CodecKind::Audio };
    const VP8: Self = Self { name: "vp8", kind: CodecKind::Video };
    #[cfg(feature = "vp9")]
    const VP9: Self = Self { name: "vp9", kind: CodecKind::Video };
    #[cfg(feature = "av1")]
    const AV1: Self = Self { name: "av1", kind: CodecKind::Video };
}

/// Registry: list of enabled codecs (feature-gated).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodecRegistry {
    /// Enabled codecs (feature-gated).
    entries: Vec<CodecEntry>,
}

impl CodecRegistry {
    /// Создаёт registry со всеми enabled codecs (feature-gated).
    ///
    /// Order: audio first, then video (deterministic).
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut v = Vec::new();
        #[cfg(feature = "opus")]
        {
            v.push(CodecEntry::OPUS);
        }
        #[cfg(feature = "vp8")]
        {
            v.push(CodecEntry::VP8);
        }
        #[cfg(feature = "vp9")]
        {
            v.push(CodecEntry::VP9);
        }
        #[cfg(feature = "av1")]
        {
            v.push(CodecEntry::AV1);
        }
        Self { entries: v }
    }

    /// Все enabled (code.
    pub fn all(&self) -> &[CodecEntry] {
        &self.entries
    }

    /// Enabled audio (codecs).
    pub fn audio(&self) -> Vec<&CodecEntry> {
        self.entries.iter().filter(|e| e.kind == CodecKind::Audio).collect()
    }

    /// Enabled video (codecs).
    pub fn video(&self) -> Vec<&CodecEntry> {
        self.entries.iter().filter(|e| e.kind == CodecKind::Video).collect()
    }

    /// Enabled ли (codec (code).
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name)
    }

    /// Video codec names (для RoomCaps.video_codecs).
    pub fn video_names(&self) -> Vec<&'static str> {
        self.video().iter().map(|e| e.name).collect()
    }

    /// Audio codec names (для RoomCaps.audio_codecs).
    pub fn audio_names(&self) -> Vec<&'static str> {
        self.audio().iter().map(|e| e.name).collect()
    }
}

impl fmt::Display for CodecRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.entries.iter().map(|e| e.name).collect::<Vec<_>>();
        write!(f, "codecs({})", names.join(", "))
    }
}

/// Информация о RTP-пакете H.264 (RFC 6184).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264RtpInfo {
    /// Тип NAL-юнита (1 = non-IDR, 5 = IDR, 7 = SPS, 8 = PPS, etc.)
    pub nal_unit_type: u8,
    /// Является ли пакет ключевым кадром (IDR slice или SPS)
    pub is_keyframe: bool,
    /// Является ли пакет фрагментом FU-A
    pub is_fua: bool,
    /// Первый ли это фрагмент кадра (Start bit)
    pub is_fua_start: bool,
    /// Последний ли это фрагмент кадра (End bit)
    pub is_fua_end: bool,
}

/// Парсит RTP-полезную нагрузку H.264 по RFC 6184.
pub fn parse_h264_rtp(payload: &[u8]) -> Option<H264RtpInfo> {
    if payload.is_empty() {
        return None;
    }
    let nal_type = payload[0] & 0x1F;
    if nal_type == 28 {
        // FU-A
        if payload.len() < 2 {
            return None;
        }
        let fu_header = payload[1];
        let is_fua_start = (fu_header & 0x80) != 0;
        let is_fua_end = (fu_header & 0x40) != 0;
        let inner_nal_type = fu_header & 0x1F;
        let is_keyframe = is_fua_start && (inner_nal_type == 5 || inner_nal_type == 7);
        Some(H264RtpInfo {
            nal_unit_type: inner_nal_type,
            is_keyframe,
            is_fua: true,
            is_fua_start,
            is_fua_end,
        })
    } else if nal_type == 24 {
        // STAP-A
        if payload.len() < 4 {
            return None;
        }
        let inner_nal = payload[3] & 0x1F;
        let is_keyframe = inner_nal == 5 || inner_nal == 7;
        Some(H264RtpInfo {
            nal_unit_type: inner_nal,
            is_keyframe,
            is_fua: false,
            is_fua_start: true,
            is_fua_end: true,
        })
    } else {
        // Single NAL unit
        let is_keyframe = nal_type == 5 || nal_type == 7;
        Some(H264RtpInfo {
            nal_unit_type: nal_type,
            is_keyframe,
            is_fua: false,
            is_fua_start: true,
            is_fua_end: true,
        })
    }
}

/// Информация о RTP-пакете VP8 (RFC 7741).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vp8RtpInfo {
    /// Является ли пакет ключевым кадром (Keyframe)
    pub is_keyframe: bool,
    /// Начало ли это раздела/партиции (Start bit)
    pub is_partition_start: bool,
    /// Идентификатор раздела (Partition ID: 0..7)
    pub partition_id: u8,
    /// Идентификатор кадра (PictureID), если присутствует в расширении
    pub picture_id: Option<u16>,
}

/// Парсит RTP-полезную нагрузку VP8 по RFC 7741.
pub fn parse_vp8_rtp(payload: &[u8]) -> Option<Vp8RtpInfo> {
    if payload.is_empty() {
        return None;
    }
    let first = payload[0];
    let x_bit = (first & 0x80) != 0;
    let s_bit = (first & 0x10) != 0;
    let pid = first & 0x07;
    let mut offset = 1;
    let mut picture_id = None;

    if x_bit {
        if offset >= payload.len() {
            return None;
        }
        let ext = payload[offset];
        let has_i = (ext & 0x80) != 0;
        let has_l = (ext & 0x40) != 0;
        let has_t = (ext & 0x20) != 0;
        let has_k = (ext & 0x10) != 0;
        offset += 1;

        if has_i {
            if offset >= payload.len() {
                return None;
            }
            if (payload[offset] & 0x80) != 0 {
                if offset + 1 >= payload.len() {
                    return None;
                }
                let pid_val = (((payload[offset] & 0x7F) as u16) << 8) | (payload[offset + 1] as u16);
                picture_id = Some(pid_val);
                offset += 2;
            } else {
                picture_id = Some(payload[offset] as u16);
                offset += 1;
            }
        }
        if has_l {
            offset += 1;
        }
        if has_t || has_k {
            offset += 1;
        }
    }

    let mut is_keyframe = false;
    if s_bit && pid == 0 && offset < payload.len() {
        let frame_header = payload[offset];
        // Bit 0 = 0 -> keyframe; 1 -> interframe
        if (frame_header & 0x01) == 0 {
            // Check optional sync code at offset + 3..offset + 6 [0x9d, 0x01, 0x2a]
            if offset + 5 < payload.len() {
                if payload[offset + 3] == 0x9D && payload[offset + 4] == 0x01 && payload[offset + 5] == 0x2A {
                    is_keyframe = true;
                }
            } else {
                is_keyframe = true;
            }
        }
    }

    Some(Vp8RtpInfo {
        is_keyframe,
        is_partition_start: s_bit,
        partition_id: pid,
        picture_id,
    })
}

/// Информация о RTP-пакете Opus (RFC 7587 / RFC 6716).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusRtpInfo {
    /// Конфигурация Opus (0..31)
    pub config: u8,
    /// Стерео ли режим (Stereo flag)
    pub is_stereo: bool,
    /// Код количества кадров (0..3)
    pub frame_count_code: u8,
    /// Длительность пакета в миллисекундах (обычно 20 мс)
    pub duration_ms: u32,
}

/// Парсит RTP-полезную нагрузку Opus по RFC 7587.
pub fn parse_opus_rtp(payload: &[u8]) -> Option<OpusRtpInfo> {
    if payload.is_empty() {
        return None;
    }
    let toc = payload[0];
    let config = toc >> 3;
    let is_stereo = (toc & 0x04) != 0;
    let frame_count_code = toc & 0x03;
    let duration_ms = match config {
        0..=3 | 12..=13 => 10,
        4..=7 | 14..=15 => 20,
        8..=11 | 16..=17 => 40,
        18..=19 => 60,
        20..=23 => 10,
        24..=27 => 20,
        _ => 20,
    };
    Some(OpusRtpInfo {
        config,
        is_stereo,
        frame_count_code,
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_include_opus_and_vp8() {
        let r = CodecRegistry::new();
        assert!(r.contains("opus"));
        assert!(r.contains("vp8"));
        assert!(!r.contains("vp9"));
        assert!(!r.contains("av1"));
    }

    #[test]
    fn audio_and_video_split() {
        let r = CodecRegistry::new();
        let audio: Vec<&str> = r.audio().iter().map(|e| e.name).collect();
        let video: Vec<&str> = r.video().iter().map(|e| e.name).collect();
        assert!(audio.contains(&"opus"));
        assert!(video.contains(&"vp8"));
        assert!(!video.contains(&"opus"));
    }

    #[test]
    fn codec_names_for_caps() {
        let r = CodecRegistry::new();
        assert_eq!(r.audio_names(), vec!["opus"]);
        assert_eq!(r.video_names(), vec!["vp8"]);
    }

    #[test]
    fn display_joins_names() {
        let r = CodecRegistry::new();
        let s = r.to_string();
        assert!(s.contains("opus"));
        assert!(s.contains("vp8"));
    }

    #[test]
    fn test_parse_h264_single_nal_and_fua() {
        assert_eq!(parse_h264_rtp(&[]), None);

        // Single NAL unit: IDR slice (type 5) -> keyframe
        let idr = [0x65, 0x88, 0x84, 0x00];
        let info = parse_h264_rtp(&idr).unwrap();
        assert_eq!(info.nal_unit_type, 5);
        assert!(info.is_keyframe);
        assert!(!info.is_fua);

        // Single NAL unit: Non-IDR slice (type 1) -> not keyframe
        let non_idr = [0x41, 0x9A];
        let info = parse_h264_rtp(&non_idr).unwrap();
        assert_eq!(info.nal_unit_type, 1);
        assert!(!info.is_keyframe);

        // FU-A: Type 28, indicator + header (Start bit = 1, type 5 IDR)
        let fua_start = [0x5C, 0x85, 0x00, 0x01];
        let info = parse_h264_rtp(&fua_start).unwrap();
        assert_eq!(info.nal_unit_type, 5);
        assert!(info.is_keyframe);
        assert!(info.is_fua);
        assert!(info.is_fua_start);
        assert!(!info.is_fua_end);

        // FU-A middle/end: Start bit = 0
        let fua_end = [0x5C, 0x45, 0x00, 0x01];
        let info = parse_h264_rtp(&fua_end).unwrap();
        assert_eq!(info.nal_unit_type, 5);
        assert!(!info.is_keyframe); // Not start, so not keyframe marker
        assert!(!info.is_fua_start);
        assert!(info.is_fua_end);
    }

    #[test]
    fn test_parse_vp8_keyframe_and_interframe() {
        assert_eq!(parse_vp8_rtp(&[]), None);

        // Simple descriptor (X=0, S=1, PID=0), VP8 header keyframe bit = 0, sync code
        let vp8_key = [0x10, 0x00, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        let info = parse_vp8_rtp(&vp8_key).unwrap();
        assert!(info.is_keyframe);
        assert!(info.is_partition_start);
        assert_eq!(info.partition_id, 0);

        // Interframe: header bit 0 = 1
        let vp8_inter = [0x10, 0x01, 0x00, 0x00];
        let info = parse_vp8_rtp(&vp8_inter).unwrap();
        assert!(!info.is_keyframe);
        assert!(info.is_partition_start);

        // With Extension bit X=1 and PictureID
        let vp8_ext = [0x90, 0x80, 0x2A, 0x00, 0x00, 0x00, 0x9D, 0x01, 0x2A];
        let info = parse_vp8_rtp(&vp8_ext).unwrap();
        assert!(info.is_keyframe);
        assert_eq!(info.picture_id, Some(42));
    }

    #[test]
    fn test_parse_opus_toc() {
        assert_eq!(parse_opus_rtp(&[]), None);

        // Config 24 (Silk/Celt 20ms mono), s=0, c=0
        let opus_pkt = [0xC0, 0xFF, 0xEE];
        let info = parse_opus_rtp(&opus_pkt).unwrap();
        assert_eq!(info.config, 24);
        assert!(!info.is_stereo);
        assert_eq!(info.duration_ms, 20);

        // Stereo flag set (bit 2)
        let opus_stereo = [0xC4, 0x11, 0x22];
        let info = parse_opus_rtp(&opus_stereo).unwrap();
        assert!(info.is_stereo);
    }
}

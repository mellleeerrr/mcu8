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
}

//! `vks-gateway-h323` — H.323-шлюз: H.323 → WebRTC (ADR-0007, H323Plus FFI).
//!
//! Фаза 6 (DEVELOPMENT_PLAN.md):
//!
//! - H.323 → WebRTC bridge: вызов «приземляется» в WebRTC-комнату
//!   (participant = gateway, для WebRTC-клиентов прозрачно —
//!   ARCHITECTURE §2);
//! - Codec negotiation (H.323 capability → WebRTC SDP):
//!   H.263/H.264 → VP8/VP9, G.711/G.729 → Opus (транскодирование при
//!   mismatch);
//! - **FFI-граница** (H323Plus, BACKEND_RULES §6.2): одна точка,
//!   panic hook (не роняет процесс), watchdog (FFI call > 5s → restart).
//!
//! Текущая волна: self-contained testable core — [`codec`] (таблица
//! соответствий) и [`call`] (H.323-вызов FSM + in-memory шлюз). Транспорт —
//! Python-сайдкар на pycrate; нативный Q.931/H.225/H.245 — отдельный эпик (см.
//! docs/audit/2026-10-29/02_h323_p6.md); дефолтная сборка dep-light, без `unsafe`.
//!
//! ## Module
//! - [`codec`] — H.323 ↔ WebRTC codec negotiation (deterministic map).
//! - [`call`] — H.323 call FSM (H.225/Q.931) and in-memory gateway.
//!
//! Dependency tree: vks-core, vks-common (ARCHITECTURE §3).

use vks_common::codec_spec::{CodecId, CodecKind, Disposition, CODECS};

/// Static PT of a codec on the H.323 sidecar plane. Payload numbers are
/// transport-local (never in the SSOT) but membership is derived from the
/// `h323: Native` disposition.
const fn h323_static_pt(id: CodecId) -> Option<u8> {
    match id {
        CodecId::Pcmu => Some(0),
        CodecId::G723 => Some(4),
        CodecId::Pcma => Some(8),
        CodecId::G722 => Some(9),
        _ => None,
    }
}

const fn h323_audio_pt_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native)
            && matches!(CODECS[i].kind, CodecKind::Audio)
            && h323_static_pt(CODECS[i].id).is_some()
        {
            count += 1;
        }
        i += 1;
    }
    count
}

const fn build_h323_audio_pts() -> [u8; h323_audio_pt_count()] {
    let mut out = [0; h323_audio_pt_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native) && matches!(CODECS[i].kind, CodecKind::Audio) {
            if let Some(pt) = h323_static_pt(CODECS[i].id) {
                out[j] = pt;
                j += 1;
            }
        }
        i += 1;
    }
    out
}

/// Symmetric static RTP payload types the H.323 single-codec bridge supports,
/// generated from the codec SSOT (`[0, 8, 9, 4]` in matrix order).
pub const H323_NATIVE_AUDIO_PTS: &[u8] = &build_h323_audio_pts();

pub mod call;
pub mod codec;
pub mod q931;
pub mod uac;
pub mod uas;

pub use call::{H323Call, H323CallState, H323Gateway};
pub use codec::{H323Codec, H323CodecMap, NegotiatedCodec, WebRtcCodec};
pub use q931::{
    build_q931_release_complete, encode_tpkt, parse_q931_message, Q931Message, MSG_ALERTING, MSG_CALL_PROCEEDING,
    MSG_CONNECT, MSG_RELEASE_COMPLETE, MSG_SETUP,
};
pub use uac::{ActiveH323Session, H323UacClient, H323VideoProfile};
pub use uas::{read_event, H323EventStream, H323Uas, H323UasHandle};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h323_pts_match_spec_slice() {
        let expected: Vec<u8> = CODECS
            .iter()
            .filter(|spec| spec.h323 == Disposition::Native && spec.kind == CodecKind::Audio)
            .filter_map(|spec| h323_static_pt(spec.id))
            .collect();
        assert_eq!(H323_NATIVE_AUDIO_PTS, expected.as_slice());
        let mut sorted = H323_NATIVE_AUDIO_PTS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 4, 8, 9], "H.323 single-codec PT set changed");
        // G.728 and Opus stay SIP-only (sidecar does not open their OLC).
        assert!(vks_common::codec_spec::is_h323_native_audio("G711"));
        for name in ["G728", "OPUS", "G726-16", "MP4A-LATM"] {
            assert!(!vks_common::codec_spec::is_h323_native_audio(name), "{name}");
        }
        assert_eq!(vks_common::codec_spec::H323_VIDEO_NAMES, &["H264"]);
    }
}

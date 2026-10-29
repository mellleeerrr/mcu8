//! Single source of truth (SSOT) for codec capabilities and dispositions.
//!
//! Every consumer (SIP offer/answer, server preflight, H.323 sidecar, WebRTC
//! answer, admin matrix, docs table) derives its codec names from [`CODECS`]
//! and the generated slices below. A codec must never be added to a second
//! list: the meta-tests in this module and in the consumer crates fail when a
//! literal table disagrees with the spec table.
//!
//! Design constraints (see `docs/audit/2026-10-29/pitfalls/T04b.md`):
//! * RTP payload types are transport-local and deliberately **not** stored in
//!   [`CodecSpec`]; they live in the SIP offer/answer tables.
//! * `G711` is an alias family (PCMU + PCMA), not a codec.
//! * H.264 High is `RxOnly`: terminals may send it, but the gateway always
//!   transmits baseline.
//! * AAC-LATM is a worker codec behind an operator license gate; availability
//!   is process state, not a disposition.
//! * WebRTC availability is split into `webrtc` (implemented at all) and
//!   `webrtc_negotiable` (present in the str0m answer). The registry is
//!   feature-gated, the answer is not.
//!
//! The module is static, allocation-free at startup and safe for wasm: no
//! environment, filesystem or time access.

/// Closed codec identity. Not a protobuf tag: `CodecId` wire values are stable
/// in `vks-proto` and are converted explicitly at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CodecId {
    Pcmu,
    Pcma,
    G722,
    G723,
    G726_16,
    G726_24,
    G726_32,
    G726_40,
    G728,
    Opus,
    AacLatm,
    G729,
    G7221,
    G719,
    Ilbc,
    H264,
    H264High,
    H265,
    Vp8,
    Vp9,
    Av1,
    H261,
    H263,
    H263Plus,
    H264Svc,
    TelephoneEvent,
}

/// Media class of a codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    Audio,
    Video,
    /// RFC 4733 `telephone-event`: carried on an audio m-line but never a
    /// selectable media codec.
    Dtmf,
}

/// Machine-readable reason why a codec is not available on a plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NoImplementation,
    EncoderNotBitExact,
    LicenseNotAccepted,
    TxProfileUnsupported,
    WebRtcOnly,
    PendingPayloader,
    SvcNotForwarded,
    H323NoAlternative,
    ProbeUnavailable,
    UnknownCodec,
}

impl Reason {
    /// Stable id used by the admin API, docs and tests (never prose).
    #[must_use]
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::NoImplementation => "no_implementation",
            Self::EncoderNotBitExact => "encoder_not_bit_exact",
            Self::LicenseNotAccepted => "license_not_accepted",
            Self::TxProfileUnsupported => "tx_profile_unsupported",
            Self::WebRtcOnly => "webrtc_only",
            Self::PendingPayloader => "pending_payloader",
            Self::SvcNotForwarded => "svc_not_forwarded",
            Self::H323NoAlternative => "h323_no_alternative",
            Self::ProbeUnavailable => "probe_unavailable",
            Self::UnknownCodec => "unknown_codec",
        }
    }
}

/// Disposition of one codec on one plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Encoded/decoded by the gateway itself (or bridged bit-exactly).
    Native,
    /// Carried by the isolated FFmpeg worker bridge (AAC-LATM).
    Worker,
    /// Available on the WebRTC leg only (forwarding, no transcode).
    WebRtc,
    /// Accepted for receive, never advertised for transmit (H.264 High).
    RxOnly,
    /// Not available on this plane; [`Reason`] says why.
    Unsupported(Reason),
}

impl Disposition {
    /// Bidirectional bridge today: safe to advertise in an offer.
    #[must_use]
    pub const fn is_bidirectional(self) -> bool {
        matches!(self, Self::Native | Self::Worker)
    }

    /// Short label for the generated public matrix.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Worker => "worker",
            Self::WebRtc => "webrtc",
            Self::RxOnly => "rx-only",
            Self::Unsupported(_) => "—",
        }
    }
}

/// Legal status of a bridge. Patent-encumbered codecs are never enabled by the
/// FFmpeg probe alone; the operator must opt in explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum License {
    Free,
    OperatorOptIn(&'static str),
}

impl License {
    /// Token accepted in `VKS_CODEC_LICENSE_ACCEPTED`.
    #[must_use]
    pub const fn token(self) -> Option<&'static str> {
        match self {
            Self::Free => None,
            Self::OperatorOptIn(token) => Some(token),
        }
    }

    #[must_use]
    pub const fn is_free(self) -> bool {
        matches!(self, Self::Free)
    }
}

/// One canonical codec and its disposition on every plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecSpec {
    pub id: CodecId,
    /// Canonical display/SDP name (case-insensitive on the wire).
    pub name: &'static str,
    /// Additional names that resolve to this spec after [`normalize`].
    pub aliases: &'static [&'static str],
    pub kind: CodecKind,
    /// RTP timestamp clock advertised in `a=rtpmap`.
    pub rtp_clock: u32,
    /// Sample rate the mixer/worker runs at (G.722: 16 kHz, AAC: 48 kHz).
    pub sample_rate: u32,
    /// Bitrate of a variant spec (G.726); `None` when the codec has no
    /// bitrate variants in the matrix.
    pub bitrate: Option<u32>,
    pub sip: Disposition,
    pub h323: Disposition,
    pub webrtc: Disposition,
    /// Whether `WebRtcPeer` includes the codec in its str0m answer. Feature-
    /// gated registry support alone is not enough (VP9/AV1).
    pub webrtc_negotiable: bool,
    pub license: License,
    /// Public reason for the admin matrix when the codec is not offered.
    pub reason: Option<Reason>,
}

/// Every codec the product knows about, in matrix order.
pub const CODECS: &[CodecSpec] = &[
    CodecSpec {
        id: CodecId::Pcmu,
        name: "PCMU",
        aliases: &["G711U"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Native,
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::Pcma,
        name: "PCMA",
        aliases: &["G711A"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Native,
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G722,
        name: "G722",
        aliases: &[],
        kind: CodecKind::Audio,
        // RTP clock is 8 kHz (RFC 3551); the mixer runs at 16 kHz.
        rtp_clock: 8_000,
        sample_rate: 16_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Native,
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G723,
        name: "G723",
        aliases: &["G7231"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Native,
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G726_16,
        name: "G726-16",
        aliases: &["G72616"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: Some(16),
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G726_24,
        name: "G726-24",
        aliases: &["G72624"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: Some(24),
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G726_32,
        name: "G726-32",
        aliases: &["G72632"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: Some(32),
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G726_40,
        name: "G726-40",
        aliases: &["G72640"],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: Some(40),
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::G728,
        name: "G728",
        aliases: &[],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::Opus,
        name: "OPUS",
        aliases: &[],
        kind: CodecKind::Audio,
        rtp_clock: 48_000,
        sample_rate: 48_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::WebRtc,
        webrtc_negotiable: true,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::AacLatm,
        name: "MP4A-LATM",
        aliases: &["AAC", "AACLATM"],
        kind: CodecKind::Audio,
        rtp_clock: 90_000,
        sample_rate: 48_000,
        bitrate: None,
        sip: Disposition::Worker,
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::OperatorOptIn("aac-latm"),
        reason: None,
    },
    CodecSpec {
        id: CodecId::G729,
        name: "G729",
        aliases: &[],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::EncoderNotBitExact),
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::EncoderNotBitExact),
    },
    CodecSpec {
        id: CodecId::G7221,
        name: "G7221",
        aliases: &["G7221C"],
        kind: CodecKind::Audio,
        rtp_clock: 16_000,
        sample_rate: 16_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::G719,
        name: "G719",
        aliases: &[],
        kind: CodecKind::Audio,
        rtp_clock: 48_000,
        sample_rate: 48_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::Ilbc,
        name: "ILBC",
        aliases: &[],
        kind: CodecKind::Audio,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::H264,
        name: "H264",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Native,
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::H264High,
        name: "H264-HIGH",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        // Receive only: UAS accepts profile_idc 66/77/100, TX is always
        // baseline (42c0xx). Offering High produced one-way video (T04a P0).
        sip: Disposition::RxOnly,
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::TxProfileUnsupported),
    },
    CodecSpec {
        id: CodecId::H265,
        name: "H265",
        aliases: &["HEVC"],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::H323NoAlternative),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
    CodecSpec {
        id: CodecId::Vp8,
        name: "VP8",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::WebRtcOnly),
        h323: Disposition::Unsupported(Reason::WebRtcOnly),
        webrtc: Disposition::WebRtc,
        webrtc_negotiable: true,
        license: License::Free,
        reason: Some(Reason::WebRtcOnly),
    },
    CodecSpec {
        id: CodecId::Vp9,
        name: "VP9",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::WebRtcOnly),
        h323: Disposition::Unsupported(Reason::WebRtcOnly),
        webrtc: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::WebRtcOnly),
    },
    CodecSpec {
        id: CodecId::Av1,
        name: "AV1",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::WebRtcOnly),
        h323: Disposition::Unsupported(Reason::WebRtcOnly),
        webrtc: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::WebRtcOnly),
    },
    CodecSpec {
        id: CodecId::H261,
        name: "H261",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::H263,
        name: "H263",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::H263Plus,
        name: "H263+",
        aliases: &["H263P"],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::NoImplementation),
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::NoImplementation),
    },
    CodecSpec {
        id: CodecId::H264Svc,
        name: "H264-SVC",
        aliases: &["H264SVC"],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::SvcNotForwarded),
        h323: Disposition::Unsupported(Reason::SvcNotForwarded),
        webrtc: Disposition::Unsupported(Reason::SvcNotForwarded),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::SvcNotForwarded),
    },
    CodecSpec {
        id: CodecId::TelephoneEvent,
        name: "telephone-event",
        aliases: &[],
        kind: CodecKind::Dtmf,
        rtp_clock: 8_000,
        sample_rate: 8_000,
        bitrate: None,
        sip: Disposition::Native,
        h323: Disposition::Unsupported(Reason::NoImplementation),
        webrtc: Disposition::Unsupported(Reason::NoImplementation),
        webrtc_negotiable: false,
        license: License::Free,
        reason: None,
    },
];

/// `G711` is not one codec: it is the law-agnostic family of PCMU and PCMA.
/// It must never be offered as a name of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AliasFamily {
    pub name: &'static str,
    pub members: &'static [CodecId],
}

/// Alias families that resolve to concrete specs.
pub const ALIAS_FAMILIES: &[AliasFamily] = &[AliasFamily { name: "G711", members: &[CodecId::Pcmu, CodecId::Pcma] }];

/// Single normalisation used by every consumer: trim, upper-case, drop
/// `. - _` and internal spaces (`G.711-A`, `g711u`, `G 711`, `G72616`).
#[must_use]
pub fn normalize(name: &str) -> String {
    // Settings spell variants as `G7221/24` or `G726:16`; the suffix is a
    // variant key handled by [`lookup_variant`], not part of the codec name.
    let base = name.trim().split([':', '/']).next().unwrap_or_default();
    base.to_ascii_uppercase().replace(['.', '-', '_', ' '], "")
}

/// Resolve a codec name or alias (not an alias family).
#[must_use]
pub fn lookup(name: &str) -> Option<&'static CodecSpec> {
    let wanted = normalize(name);
    if wanted.is_empty() {
        return None;
    }
    CODECS
        .iter()
        .find(|spec| normalize(spec.name) == wanted || spec.aliases.iter().any(|alias| normalize(alias) == wanted))
}

/// Resolve an alias family such as `G711`.
#[must_use]
pub fn family(name: &str) -> Option<&'static AliasFamily> {
    let wanted = normalize(name);
    ALIAS_FAMILIES.iter().find(|family| normalize(family.name) == wanted)
}

/// Resolve a settings variant: bare `G726` requires an explicit bitrate and
/// then resolves to the matching `G726-16/24/32/40` spec.
#[must_use]
pub fn lookup_variant(name: &str, bitrate: Option<u32>) -> Option<&'static CodecSpec> {
    if normalize(name) == "G726" {
        return lookup(&format!("G726{}", bitrate?));
    }
    let spec = lookup(name)?;
    match (spec.bitrate, bitrate) {
        (Some(expected), Some(given)) if expected != given => None,
        _ => Some(spec),
    }
}

/// Spec by closed id. The table is small and this is never in a hot loop; a
/// missing id is a programming error.
#[must_use]
pub fn spec(id: CodecId) -> &'static CodecSpec {
    CODECS.iter().find(|spec| spec.id == id).expect("codec id missing from CODECS")
}

/// H.323 audio codec names (native specs plus aliases and the G711 family).
pub const H323_AUDIO_NAMES: &[&str] = &h323_audio_names();
/// H.323 video codec names (H.241/H.264 only today).
pub const H323_VIDEO_NAMES: &[&str] = &h323_video_names();

/// True when the H.323 sidecar opens an OLC for this audio codec.
#[must_use]
pub fn is_h323_native_audio(name: &str) -> bool {
    if let Some(family) = family(name) {
        return family.members.iter().any(|id| spec(*id).h323 == Disposition::Native);
    }
    lookup(name).is_some_and(|spec| spec.kind == CodecKind::Audio && spec.h323 == Disposition::Native)
}

/// True when the H.323 sidecar negotiates this video codec.
#[must_use]
pub fn is_h323_native_video(name: &str) -> bool {
    lookup(name).is_some_and(|spec| spec.kind == CodecKind::Video && spec.h323 == Disposition::Native)
}

/// Specs that participate in the WebRTC str0m answer.
pub fn webrtc_answer_specs() -> impl Iterator<Item = &'static CodecSpec> {
    CODECS.iter().filter(|spec| spec.webrtc == Disposition::WebRtc && spec.webrtc_negotiable)
}

// ---------------------------------------------------------------------------
// Generated slices. `const fn` keeps startup allocation-free and wasm-safe.
// ---------------------------------------------------------------------------

const fn spec_sip_native(spec: &CodecSpec) -> bool {
    matches!(spec.sip, Disposition::Native)
}

const fn native_audio_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Audio) {
            count += 1 + CODECS[i].aliases.len();
        }
        i += 1;
    }
    // G711 family alias.
    count + 1
}

const fn build_native_audio() -> [&'static str; native_audio_count()] {
    let mut out = [""; native_audio_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Audio) {
            out[j] = CODECS[i].name;
            j += 1;
            let mut k = 0;
            while k < CODECS[i].aliases.len() {
                out[j] = CODECS[i].aliases[k];
                j += 1;
                k += 1;
            }
        }
        i += 1;
    }
    out[j] = ALIAS_FAMILIES[0].name;
    out
}

const fn native_video_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Video) {
            count += 1 + CODECS[i].aliases.len();
        }
        i += 1;
    }
    count
}

const fn build_native_video() -> [&'static str; native_video_count()] {
    let mut out = [""; native_video_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Video) {
            out[j] = CODECS[i].name;
            j += 1;
            let mut k = 0;
            while k < CODECS[i].aliases.len() {
                out[j] = CODECS[i].aliases[k];
                j += 1;
                k += 1;
            }
        }
        i += 1;
    }
    out
}

const fn worker_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].sip, Disposition::Worker) {
            count += 1 + CODECS[i].aliases.len();
        }
        i += 1;
    }
    count
}

const fn build_worker() -> [&'static str; worker_count()] {
    let mut out = [""; worker_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].sip, Disposition::Worker) {
            out[j] = CODECS[i].name;
            j += 1;
            let mut k = 0;
            while k < CODECS[i].aliases.len() {
                out[j] = CODECS[i].aliases[k];
                j += 1;
                k += 1;
            }
        }
        i += 1;
    }
    out
}

const fn webrtc_answer_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].webrtc, Disposition::WebRtc) && CODECS[i].webrtc_negotiable {
            count += 1;
        }
        i += 1;
    }
    count
}

const fn build_webrtc_answer() -> [&'static str; webrtc_answer_count()] {
    let mut out = [""; webrtc_answer_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].webrtc, Disposition::WebRtc) && CODECS[i].webrtc_negotiable {
            out[j] = CODECS[i].name;
            j += 1;
        }
        i += 1;
    }
    out
}

const fn h323_audio_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native) && matches!(CODECS[i].kind, CodecKind::Audio) {
            count += 1 + CODECS[i].aliases.len();
        }
        i += 1;
    }
    // G711 family alias.
    count + 1
}

const fn h323_audio_names() -> [&'static str; h323_audio_count()] {
    let mut out = [""; h323_audio_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native) && matches!(CODECS[i].kind, CodecKind::Audio) {
            out[j] = CODECS[i].name;
            j += 1;
            let mut k = 0;
            while k < CODECS[i].aliases.len() {
                out[j] = CODECS[i].aliases[k];
                j += 1;
                k += 1;
            }
        }
        i += 1;
    }
    out[j] = ALIAS_FAMILIES[0].name;
    out
}

const fn h323_video_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native) && matches!(CODECS[i].kind, CodecKind::Video) {
            count += 1;
        }
        i += 1;
    }
    count
}

const fn h323_video_names() -> [&'static str; h323_video_count()] {
    let mut out = [""; h323_video_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if matches!(CODECS[i].h323, Disposition::Native) && matches!(CODECS[i].kind, CodecKind::Video) {
            out[j] = CODECS[i].name;
            j += 1;
        }
        i += 1;
    }
    out
}

/// Native (non-transcoding) audio names, including aliases, in matrix order.
#[must_use]
pub const fn native_audio_names() -> [&'static str; native_audio_count()] {
    build_native_audio()
}

/// Native (non-transcoding) video names, including aliases.
#[must_use]
pub const fn native_video_names() -> [&'static str; native_video_count()] {
    build_native_video()
}

/// Worker/transcode audio names, including aliases.
#[must_use]
pub const fn worker_codec_names() -> [&'static str; worker_count()] {
    build_worker()
}

/// Names of the codecs present in the WebRTC answer (Opus + VP8 today).
#[must_use]
pub const fn webrtc_answer_names() -> [&'static str; webrtc_answer_count()] {
    build_webrtc_answer()
}

// ---------------------------------------------------------------------------
// Public matrix rendering (docs and admin).
// ---------------------------------------------------------------------------

/// Renders the truthful markdown matrix used by
/// `docs/architecture/MEDIA_PIPELINE.md` §5 and checked by a doc test.
#[must_use]
pub fn render_markdown_matrix() -> String {
    let mut out = String::from("| Codec | Kind | SIP | H.323 | WebRTC | Reason |\n|---|---|---|---|---|---|\n");
    for spec in CODECS {
        let kind = match spec.kind {
            CodecKind::Audio => "audio",
            CodecKind::Video => "video",
            CodecKind::Dtmf => "dtmf",
        };
        let reason = match (spec.sip, spec.reason) {
            (Disposition::Native | Disposition::Worker, _) => "—",
            (_, Some(reason)) => reason.as_id(),
            (_, None) => "no_implementation",
        };
        out.push_str(&format!(
            "| {} | {kind} | {} | {} | {} | {reason} |\n",
            spec.name,
            spec.sip.label(),
            spec.h323.label(),
            spec.webrtc.label(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn ssot_specs_have_unique_ids_names_and_aliases() {
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for spec in CODECS {
            assert!(ids.insert(spec.id), "duplicate id {:?}", spec.id);
            assert!(names.insert(normalize(spec.name)), "duplicate name {}", spec.name);
        }
        for spec in CODECS {
            let canonical = normalize(spec.name);
            for alias in spec.aliases {
                let normalized = normalize(alias);
                // Redundant punctuation spellings (G726-16 vs G72616) are kept
                // only to expand NATIVE_AUDIO without changing its public
                // contract; they must not collide with a *different* spec.
                if normalized == canonical {
                    continue;
                }
                assert!(names.insert(normalized.clone()), "alias {alias} collides with another spec");
            }
        }
        for family in ALIAS_FAMILIES {
            assert!(names.insert(normalize(family.name)), "family collides with a spec");
        }
        assert!(!CODECS.is_empty());
    }

    #[test]
    fn every_spec_has_one_disposition_per_plane_and_resolves_by_name() {
        for spec in CODECS {
            // Each plane field is exactly one value by construction; assert the
            // lookup roundtrip and the enum invariants that consumers rely on.
            assert_eq!(lookup(spec.name).map(|s| s.id), Some(spec.id), "lookup {}", spec.name);
            for alias in spec.aliases {
                assert_eq!(lookup(alias).map(|s| s.id), Some(spec.id), "alias {alias}");
            }
            if spec.webrtc_negotiable {
                assert_eq!(spec.webrtc, Disposition::WebRtc, "{} negotiable but not WebRtc", spec.name);
            }
            if matches!(spec.sip, Disposition::Unsupported(_) | Disposition::RxOnly) {
                assert!(spec.reason.is_some(), "{} has no public reason", spec.name);
            }
            assert!(!spec.name.is_empty());
        }
    }

    #[test]
    fn native_slices_are_derived_and_complete() {
        let audio: BTreeSet<&str> = native_audio_names().iter().copied().collect();
        let video: BTreeSet<&str> = native_video_names().iter().copied().collect();
        for spec in CODECS {
            match spec.kind {
                CodecKind::Audio => {
                    assert_eq!(
                        audio.contains(spec.name),
                        spec.sip == Disposition::Native,
                        "{} NATIVE_AUDIO membership",
                        spec.name
                    );
                }
                CodecKind::Video => {
                    assert_eq!(
                        video.contains(spec.name),
                        spec.sip == Disposition::Native,
                        "{} NATIVE_VIDEO membership",
                        spec.name
                    );
                }
                CodecKind::Dtmf => {
                    assert!(!audio.contains(spec.name), "telephone-event is not a media codec");
                }
            }
        }
        assert!(audio.contains("G711"), "G711 family alias must stay in NATIVE_AUDIO");
        assert!(audio.contains("G711U") && audio.contains("G711A"));
        assert!(audio.contains("G7231"));
        assert!(video.contains("HEVC"));
        assert!(!video.contains("H264-HIGH"), "High is RX-only");
        // No duplicates in generated slices.
        assert_eq!(audio.len(), native_audio_names().len());
        assert_eq!(video.len(), native_video_names().len());
    }

    #[test]
    fn worker_and_webrtc_slices_match_dispositions() {
        let workers: BTreeSet<&str> = worker_codec_names().iter().copied().collect();
        assert!(workers.contains("MP4A-LATM"));
        assert!(workers.contains("AAC"));
        for spec in CODECS {
            assert_eq!(workers.contains(spec.name), spec.sip == Disposition::Worker, "{}", spec.name);
        }
        let answer: Vec<CodecId> = webrtc_answer_specs().map(|spec| spec.id).collect();
        assert_eq!(answer, vec![CodecId::Opus, CodecId::Vp8]);
        let names: BTreeSet<&str> = webrtc_answer_names().iter().copied().collect();
        assert_eq!(names, BTreeSet::from(["OPUS", "VP8"]));
        for spec in CODECS {
            assert_eq!(names.contains(spec.name), spec.webrtc_negotiable, "{} answer slice", spec.name);
        }
    }

    #[test]
    fn unsupported_specs_have_a_reason_from_the_dictionary() {
        let known = [
            Reason::NoImplementation,
            Reason::EncoderNotBitExact,
            Reason::LicenseNotAccepted,
            Reason::TxProfileUnsupported,
            Reason::WebRtcOnly,
            Reason::PendingPayloader,
            Reason::SvcNotForwarded,
            Reason::H323NoAlternative,
            Reason::ProbeUnavailable,
            Reason::UnknownCodec,
        ];
        for spec in CODECS {
            if let Some(reason) = spec.reason {
                assert!(known.contains(&reason));
                assert!(!reason.as_id().is_empty());
            }
        }
        assert_eq!(lookup("G729").unwrap().reason, Some(Reason::EncoderNotBitExact));
        assert_eq!(lookup("G7221C").unwrap().reason, Some(Reason::NoImplementation));
        assert_eq!(lookup("G719").unwrap().reason, Some(Reason::NoImplementation));
        assert_eq!(lookup("H264-HIGH").unwrap().reason, Some(Reason::TxProfileUnsupported));
        assert_eq!(lookup("VP8").unwrap().reason, Some(Reason::WebRtcOnly));
    }

    #[test]
    fn g711_family_is_not_a_codec_and_resolves_to_pcmu_pcma() {
        assert!(lookup("G711").is_none(), "G711 must not resolve to a single codec");
        let family = family("g.711").expect("family");
        assert_eq!(family.members, &[CodecId::Pcmu, CodecId::Pcma]);
        assert_eq!(lookup("G711U").unwrap().id, CodecId::Pcmu);
        assert_eq!(lookup("G.711-A").unwrap().id, CodecId::Pcma);
        assert_eq!(normalize("G711 A"), "G711A");
    }

    #[test]
    fn lookup_accepts_all_previously_accepted_spellings() {
        for (spelling, id) in [
            ("PCMU", CodecId::Pcmu),
            ("pcmu", CodecId::Pcmu),
            ("G.711U", CodecId::Pcmu),
            ("G711U", CodecId::Pcmu),
            ("G711-A", CodecId::Pcma),
            ("g711a", CodecId::Pcma),
            ("G 711A", CodecId::Pcma),
            ("G.722", CodecId::G722),
            ("g722", CodecId::G722),
            ("G72616", CodecId::G726_16),
            ("G.726-16", CodecId::G726_16),
            ("G726-40", CodecId::G726_40),
            ("G7221/24", CodecId::G7221),
            ("G7221C", CodecId::G7221),
            ("G7231", CodecId::G723),
            ("MP4A-LATM", CodecId::AacLatm),
            ("mp4a_latm", CodecId::AacLatm),
            ("MP4ALATM", CodecId::AacLatm),
            ("AAC-LATM", CodecId::AacLatm),
            ("h.264", CodecId::H264),
            ("HEVC", CodecId::H265),
            ("h265", CodecId::H265),
            ("H264-HIGH", CodecId::H264High),
            ("telephone-event", CodecId::TelephoneEvent),
        ] {
            assert_eq!(lookup(spelling).map(|spec| spec.id), Some(id), "{spelling}");
        }
        assert!(lookup("").is_none());
        assert!(lookup("not-a-codec").is_none());
    }

    #[test]
    fn g726_variants_require_a_supported_bitrate() {
        assert_eq!(lookup_variant("G726", Some(16)).unwrap().id, CodecId::G726_16);
        assert_eq!(lookup_variant("G726", Some(40)).unwrap().id, CodecId::G726_40);
        assert!(lookup_variant("G726", None).is_none());
        assert!(lookup_variant("G726", Some(48)).is_none());
        assert!(lookup_variant("G72616", Some(48)).is_none());
        assert_eq!(lookup_variant("G72616", None).unwrap().id, CodecId::G726_16);
    }

    #[test]
    fn g7231_alias_is_supported() {
        let spec = lookup("G7231").expect("alias");
        assert_eq!(spec.id, CodecId::G723);
        assert_eq!(spec.sip, Disposition::Native);
        assert_eq!(spec.h323, Disposition::Native);
    }

    #[test]
    fn h264_high_is_rx_only_and_never_offered() {
        let spec = lookup("H264-HIGH").expect("spec");
        assert_eq!(spec.sip, Disposition::RxOnly);
        assert!(!spec.sip.is_bidirectional());
        assert_eq!(spec.reason, Some(Reason::TxProfileUnsupported));
        assert!(!H323_AUDIO_NAMES.contains(&"H264-HIGH"));
    }

    #[test]
    fn h323_slices_match_spec_dispositions() {
        for name in ["PCMU", "PCMA", "G722", "G723", "G7231", "G711", "G711U", "G711A"] {
            assert!(H323_AUDIO_NAMES.contains(&name), "H.323 audio missing {name}");
        }
        for name in ["G728", "G726-16", "OPUS", "MP4A-LATM", "G729"] {
            assert!(!H323_AUDIO_NAMES.contains(&name), "H.323 must not advertise {name}");
            assert!(!is_h323_native_audio(name), "H.323 native must reject {name}");
        }
        assert_eq!(H323_VIDEO_NAMES, &["H264"]);
        assert!(is_h323_native_video("h.264"));
        assert!(!is_h323_native_video("H265"));
        assert!(is_h323_native_audio("G711"));
    }

    #[test]
    fn webrtc_caps_do_not_exceed_answer() {
        for spec in CODECS {
            if spec.webrtc_negotiable {
                assert!(webrtc_answer_names().contains(&spec.name));
                assert_eq!(spec.webrtc, Disposition::WebRtc);
            } else {
                assert!(!webrtc_answer_names().contains(&spec.name), "{} leaked into answer", spec.name);
            }
        }
        // Feature-gated VP9/AV1 never enter the answer slice.
        assert!(!webrtc_answer_names().contains(&"VP9"));
        assert!(!webrtc_answer_names().contains(&"AV1"));
    }

    #[test]
    fn specs_have_no_payload_type_field() {
        // T2: PT is transport-local. Guard the struct definition itself so a
        // future refactor cannot move it into the SSOT unnoticed.
        let source = include_str!("codec_spec.rs");
        let struct_start = source.find("pub struct CodecSpec {").expect("struct");
        let struct_end = source[struct_start..].find("\n}").expect("end") + struct_start;
        let body = &source[struct_start..struct_end];
        assert!(!body.contains("payload"), "CodecSpec must not carry an RTP payload type");
        assert!(!body.contains("packet_type"), "CodecSpec must not carry an RTP payload type");
    }

    #[test]
    fn license_tokens_are_unique_and_stable() {
        let tokens: Vec<&str> = CODECS.iter().filter_map(|spec| spec.license.token()).collect();
        assert_eq!(tokens, vec!["aac-latm"]);
        assert_eq!(spec(CodecId::AacLatm).license.token(), Some("aac-latm"));
        assert!(spec(CodecId::Pcmu).license.is_free());
    }

    #[test]
    fn docs_public_matrix_matches_ssot() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/architecture/MEDIA_PIPELINE.md");
        let doc = std::fs::read_to_string(path).expect("MEDIA_PIPELINE.md must be readable");
        let begin = "<!-- codec-matrix:begin -->";
        let end = "<!-- codec-matrix:end -->";
        let start = doc.find(begin).expect("codec-matrix:begin marker") + begin.len();
        let stop = doc.find(end).expect("codec-matrix:end marker");
        let documented = doc[start..stop].trim_matches(['\r', '\n']);
        let rendered = render_markdown_matrix();
        assert_eq!(
            documented,
            rendered.trim_end(),
            "MEDIA_PIPELINE.md codec matrix is stale; paste the generated table"
        );
    }

    #[test]
    fn no_second_codec_name_table_outside_allowlist() {
        // T1: the SSOT is only useful if no consumer keeps its own literal
        // table. The allowlist is exactly the SIP offer tables (`AUDIO_OFFER`/
        // `VIDEO_OFFER`) and the dead H.323 CodecMap (T04c), which are
        // transport/legacy models, not capability truth.
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let forbidden: &[(&str, &str)] = &[
            ("vks-server/src/gateways.rs", "const H323_AUDIO"),
            ("vks-server/src/gateways.rs", "const H323_VIDEO"),
            ("vks-server/src/gateways.rs", "107 => \"G726-16\""),
            ("vks-server/src/gateway_audio.rs", "Some(\"G72616\")"),
            ("vks-server/src/gateway_aac.rs", "matches!(normalized.as_str(), \"AAC\""),
            ("vks-gateway-sip/src/uas.rs", "\"G72616\" => \"G726-16\""),
            ("vks-gateway-sip/src/rtp_bridge.rs", "pub native: bool"),
            ("vks-gateway-h323/src/lib.rs", "pub const H323_NATIVE_AUDIO_PTS: &[u8] = &[0, 4, 8, 9]"),
            ("vks-signaling/src/sdp.rs", "\"opus\" | \"pcmu\" | \"pcma\" | \"g722\""),
        ];
        for (file, needle) in forbidden {
            let path = format!("{root}/{file}");
            let source = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {path}"));
            assert!(!source.contains(needle), "{file} still contains a second codec table: {needle}");
        }
        for file in [
            "vks-common/src/codec_capabilities.rs",
            "vks-server/src/gateways.rs",
            "vks-server/src/gateway_audio.rs",
            "vks-server/src/gateway_aac.rs",
            "vks-gateway-sip/src/rtp_bridge.rs",
            "vks-gateway-sip/src/uas.rs",
            "vks-gateway-h323/src/lib.rs",
            "vks-signaling/src/sdp.rs",
        ] {
            let path = format!("{root}/{file}");
            let source = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {path}"));
            assert!(source.contains("codec_spec"), "{file} does not consume the codec SSOT");
        }
    }
}

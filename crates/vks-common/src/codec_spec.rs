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

impl CodecId {
    /// `true` for the four G.726 bitrate specs.
    #[must_use]
    pub const fn is_g726(self) -> bool {
        matches!(self, Self::G726_16 | Self::G726_24 | Self::G726_32 | Self::G726_40)
    }
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
    /// The codec variant exists but the requested bitrate is not in the
    /// matrix (`G726:20`).
    UnsupportedBitrate,
    /// The FFmpeg probe was green but the worker pool cannot serve the bridge
    /// (fewer than two workers).
    WorkerPoolUnavailable,
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
            Self::UnsupportedBitrate => "unsupported_bitrate",
            Self::WorkerPoolUnavailable => "worker_pool_unavailable",
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
        // The matrix says "not offered" on every plane: the missing piece is
        // the RTP payloader, not the WebRTC transport. `webrtc_only` would
        // contradict the empty WebRTC column (review A P2-5).
        sip: Disposition::Unsupported(Reason::PendingPayloader),
        h323: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::PendingPayloader),
    },
    CodecSpec {
        id: CodecId::Av1,
        name: "AV1",
        aliases: &[],
        kind: CodecKind::Video,
        rtp_clock: 90_000,
        sample_rate: 90_000,
        bitrate: None,
        sip: Disposition::Unsupported(Reason::PendingPayloader),
        h323: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc: Disposition::Unsupported(Reason::PendingPayloader),
        webrtc_negotiable: false,
        license: License::Free,
        reason: Some(Reason::PendingPayloader),
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

/// One truthful variant of a codec whose identity alone does not determine the
/// wire parameters: G.722.1 is 16 kHz, G.722.1C is 32 kHz, and the same
/// `G7221` SDP name is used for both (RFC 4749 has no separate encoding name).
///
/// The table is keyed by `(id, rtp_clock)`. Payload types stay transport-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecVariant {
    pub id: CodecId,
    /// RTP timestamp clock: 16 kHz for G.722.1, 32 kHz for G.722.1C.
    pub rtp_clock: u32,
    /// Bitrates (bit/s) valid with this clock.
    pub bitrates: &'static [u32],
}

/// Every codec variant that cannot be derived from [`CodecSpec`] alone.
///
/// `G7221C` is an alias of [`CodecId::G7221`] (the wire identity is unchanged);
/// this table is what tells the 16 kHz and 32 kHz forms apart. Consumers that
/// resolve an SDP row must use [`resolve_clock`], not `spec.rtp_clock`.
pub const CODEC_VARIANTS: &[CodecVariant] = &[
    CodecVariant { id: CodecId::G7221, rtp_clock: 16_000, bitrates: &[24_000, 32_000] },
    CodecVariant { id: CodecId::G7221, rtp_clock: 32_000, bitrates: &[24_000, 32_000, 48_000] },
];

/// Variants declared for a codec id (empty when the spec has one fixed form).
pub fn variants(id: CodecId) -> impl Iterator<Item = &'static CodecVariant> {
    CODEC_VARIANTS.iter().filter(move |variant| variant.id == id)
}

/// The exact variant for `(id, clock)`.
#[must_use]
pub fn variant(id: CodecId, rtp_clock: u32) -> Option<&'static CodecVariant> {
    variants(id).find(|variant| variant.rtp_clock == rtp_clock)
}

/// A spec resolved together with its exact wire variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCodec {
    pub spec: &'static CodecSpec,
    /// `Some` for codecs with a [`CODEC_VARIANTS`] entry, `None` otherwise.
    pub variant: Option<&'static CodecVariant>,
}

/// Resolve an SDP `(name, clock)` pair to a spec and, for variant codecs, to
/// the exact variant row. A clock that does not belong to the spec (for
/// example `G7221/8000`) is rejected instead of being silently accepted.
#[must_use]
pub fn resolve_clock(name: &str, rtp_clock: u32) -> Option<ResolvedCodec> {
    let spec = lookup(name)?;
    if variants(spec.id).next().is_some() {
        let variant = variant(spec.id, rtp_clock)?;
        return Some(ResolvedCodec { spec, variant: Some(variant) });
    }
    (spec.rtp_clock == rtp_clock).then_some(ResolvedCodec { spec, variant: None })
}

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

/// `true` for the G.726 family spellings (`G726`, `G72616`, `G726:20`,
/// `G.726-32`), including invalid bitrates that must be reported as
/// `unsupported_bitrate` rather than `unknown_codec`.
#[must_use]
pub fn is_g726_name(name: &str) -> bool {
    normalize(name).starts_with("G726")
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
        // Variants live in the SSOT: look the bitrate up instead of
        // concatenating a name (`G72620` must not accidentally become a codec).
        let bitrate = bitrate?;
        return CODECS.iter().find(|spec| spec.id.is_g726() && spec.bitrate == Some(bitrate));
    }
    let spec = lookup(name)?;
    if let (Some(expected), Some(given)) = (spec.bitrate, bitrate) {
        if expected != given {
            return None;
        }
    }
    // Variant codecs (G.722.1 / G.722.1C) only accept bitrates their clock
    // actually supports; `G7221C:64` is not a real variant. Settings spell the
    // bitrate in kbit/s (`G7221:24`) while SDP `bitrate=` is in bit/s
    // (`bitrate=24000`); the table stores bit/s.
    if variants(spec.id).next().is_some() {
        if let Some(given) = bitrate {
            let wire_bitrate = if given < 1_000 { given.saturating_mul(1_000) } else { given };
            if !variants(spec.id).any(|variant| variant.bitrates.contains(&wire_bitrate)) {
                return None;
            }
        }
    }
    Some(spec)
}

/// `true` when the name resolves to a codec whose clock/bitrate variants live
/// in [`CODEC_VARIANTS`] (G.722.1 / G.722.1C).
#[must_use]
pub fn is_variant_codec(name: &str) -> bool {
    lookup(name).is_some_and(|spec| variants(spec.id).next().is_some())
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
    // Alias families (G711 today) are advertised by name in the native list.
    // Iterate the table so a second family cannot be silently omitted.
    count + ALIAS_FAMILIES.len()
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
    let mut family = 0;
    while family < ALIAS_FAMILIES.len() {
        out[j] = ALIAS_FAMILIES[family].name;
        j += 1;
        family += 1;
    }
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
    // Alias families (G711 today) are advertised by name in the H.323 list.
    count + ALIAS_FAMILIES.len()
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
    let mut family = 0;
    while family < ALIAS_FAMILIES.len() {
        out[j] = ALIAS_FAMILIES[family].name;
        j += 1;
        family += 1;
    }
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

const fn public_native_audio_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Audio) {
            count += 1;
        }
        i += 1;
    }
    // Alias families (G711 today) are part of the public native list.
    count + ALIAS_FAMILIES.len()
}

/// Canonical (alias-free) public list: exactly one entry per native audio
/// codec, plus the `G711` family name. `/api/v3/net/status` uses this so aliases
/// such as `G72616`/`G7231` never show up as duplicate codecs.
const fn build_public_native_audio() -> [&'static str; public_native_audio_count()] {
    let mut out = [""; public_native_audio_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Audio) {
            out[j] = CODECS[i].name;
            j += 1;
        }
        i += 1;
    }
    let mut family = 0;
    while family < ALIAS_FAMILIES.len() {
        out[j] = ALIAS_FAMILIES[family].name;
        j += 1;
        family += 1;
    }
    out
}

const fn public_native_video_count() -> usize {
    let mut i = 0;
    let mut count = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Video) {
            count += 1;
        }
        i += 1;
    }
    count
}

/// Canonical (alias-free) public video list (H264 + H265 today; `HEVC` is an
/// alias and must not look like a separate codec in `/net/status`).
const fn build_public_native_video() -> [&'static str; public_native_video_count()] {
    let mut out = [""; public_native_video_count()];
    let mut i = 0;
    let mut j = 0;
    while i < CODECS.len() {
        if spec_sip_native(&CODECS[i]) && matches!(CODECS[i].kind, CodecKind::Video) {
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

/// Canonical native audio names for public JSON (no alias duplicates).
#[must_use]
pub const fn public_native_audio_names() -> [&'static str; public_native_audio_count()] {
    build_public_native_audio()
}

/// Canonical native video names for public JSON (no alias duplicates).
#[must_use]
pub const fn public_native_video_names() -> [&'static str; public_native_video_count()] {
    build_public_native_video()
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
            Reason::UnsupportedBitrate,
            Reason::WorkerPoolUnavailable,
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
        // VP9/AV1 are not offered anywhere, so their public reason must not
        // claim "WebRTC only" (review A P2-5).
        assert_eq!(lookup("VP9").unwrap().reason, Some(Reason::PendingPayloader));
        assert_eq!(lookup("AV1").unwrap().reason, Some(Reason::PendingPayloader));
    }

    #[test]
    fn g7221_variants_model_both_clocks_truthfully() {
        // G.722.1 is 16 kHz / 24-32 kbit; G.722.1C is the 32 kHz form of the
        // same wire codec (24/32/48 kbit). Five SIP identity rows, five variants.
        assert_eq!(
            CODEC_VARIANTS,
            &[
                CodecVariant { id: CodecId::G7221, rtp_clock: 16_000, bitrates: &[24_000, 32_000] },
                CodecVariant { id: CodecId::G7221, rtp_clock: 32_000, bitrates: &[24_000, 32_000, 48_000] },
            ]
        );
        let sixteen = resolve_clock("G7221", 16_000).expect("G.722.1 row");
        assert_eq!(sixteen.spec.id, CodecId::G7221);
        assert_eq!(sixteen.variant.expect("16k variant").bitrates, &[24_000, 32_000]);
        let thirty_two = resolve_clock("G7221C", 32_000).expect("G.722.1C row");
        assert_eq!(thirty_two.spec.id, CodecId::G7221, "G7221C keeps the G7221 wire identity");
        assert_eq!(thirty_two.variant.expect("32k variant").bitrates, &[24_000, 32_000, 48_000]);
        // The alias still resolves to the same spec: no new wire id or PT.
        assert_eq!(lookup("G7221C").map(|spec| spec.id), Some(CodecId::G7221));
        assert_eq!(variant(CodecId::G7221, 8_000), None);
        assert_eq!(resolve_clock("G7221", 8_000), None);
        assert_eq!(resolve_clock("G722", 8_000).expect("fixed clock").variant, None);
        assert_eq!(resolve_clock("G722", 16_000), None);
        // Bitrates are part of the variant truth, not a free-form suffix.
        assert!(lookup_variant("G7221C", Some(48)).is_some());
        assert!(lookup_variant("G7221", Some(64)).is_none());
        assert!(lookup_variant("G7221C", Some(16)).is_none());
        assert!(is_variant_codec("G7221C"));
        assert!(!is_variant_codec("G729"));
    }

    #[test]
    fn public_native_lists_are_canonical_and_deduplicated() {
        let audio = public_native_audio_names();
        let unique: BTreeSet<&str> = audio.iter().copied().collect();
        assert_eq!(unique.len(), audio.len(), "public native audio has duplicate names: {audio:?}");
        assert_eq!(
            unique,
            BTreeSet::from([
                "PCMU", "PCMA", "G722", "G723", "G726-16", "G726-24", "G726-32", "G726-40", "G728", "OPUS", "G711"
            ])
        );
        // Aliases stay available for settings validation but not in the public
        // list: G72616/G7231/HEVC must not look like extra codecs.
        for alias in ["G72616", "G7231", "G711U", "G711A", "HEVC"] {
            assert!(!audio.contains(&alias), "alias {alias} leaked into public native_audio");
        }
        let video = public_native_video_names();
        assert_eq!(video, ["H264", "H265"]);
        assert!(!video.contains(&"HEVC"));
        // Every canonical public name resolves to exactly one spec.
        for name in audio.iter().chain(video.iter()) {
            assert!(lookup(name).is_some() || family(name).is_some(), "public name {name} is not in the SSOT");
        }
    }

    #[test]
    fn matrix_reason_does_not_contradict_the_unavailable_plane() {
        let matrix = render_markdown_matrix();
        for (name, expected_reason) in
            [("VP8", "webrtc_only"), ("VP9", "pending_payloader"), ("AV1", "pending_payloader")]
        {
            let row = matrix.lines().find(|line| line.starts_with(&format!("| {name} |"))).expect("row");
            assert!(row.ends_with(&format!("| {expected_reason} |")), "{name} row: {row}");
        }
        let vp9 = matrix.lines().find(|line| line.starts_with("| VP9 |")).expect("VP9 row");
        assert!(vp9.contains("| — | — | — |"), "VP9 must not claim WebRTC availability: {vp9}");
        let vp8 = matrix.lines().find(|line| line.starts_with("| VP8 |")).expect("VP8 row");
        assert!(vp8.contains("| webrtc |"), "VP8 is available on the WebRTC plane: {vp8}");
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
    fn alias_families_are_audio_native_and_present_in_every_native_audio_slice() {
        // The generated lists append every alias family by name. Iterating the
        // table (instead of `ALIAS_FAMILIES[0]` + `count + 1`) keeps a second
        // family from silently disappearing (review C P3-1); this test pins the
        // invariant that makes the unconditional append truthful.
        let native: BTreeSet<&str> = native_audio_names().iter().copied().collect();
        let h323: BTreeSet<&str> = H323_AUDIO_NAMES.iter().copied().collect();
        let public: BTreeSet<&str> = public_native_audio_names().iter().copied().collect();
        for family in ALIAS_FAMILIES {
            assert!(!family.members.is_empty(), "family {} has no members", family.name);
            for member in family.members {
                let spec = spec(*member);
                assert_eq!(spec.kind, CodecKind::Audio, "family {} member must be audio", family.name);
                assert_eq!(spec.sip, Disposition::Native, "family {} member is not SIP-native", family.name);
                assert_eq!(spec.h323, Disposition::Native, "family {} member is not H.323-native", family.name);
            }
            assert!(native.contains(family.name), "family {} missing from native audio", family.name);
            assert!(h323.contains(family.name), "family {} missing from H.323 audio", family.name);
            assert!(public.contains(family.name), "family {} missing from public native audio", family.name);
        }
        assert_eq!(native_audio_names().len(), native.len(), "native audio has duplicates");
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

    /// `CodecSpec` struct body with comments removed, so a prose mention of a
    /// payload type cannot hide (or trip) the field-name guard.
    fn codec_spec_struct_code() -> String {
        let source = include_str!("codec_spec.rs");
        let start = source.find("pub struct CodecSpec {").expect("CodecSpec struct");
        let end = source[start..].find("\n}").expect("CodecSpec end") + start;
        let body = &source[start + "pub struct CodecSpec {".len()..end];
        let mut code = String::with_capacity(body.len());
        let mut chars = body.chars().peekable();
        while let Some(character) = chars.next() {
            match character {
                '/' if chars.peek() == Some(&'/') => {
                    for character in chars.by_ref() {
                        if character == '\n' {
                            code.push('\n');
                            break;
                        }
                    }
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    let mut depth = 1usize;
                    let mut previous = '\0';
                    for character in chars.by_ref() {
                        if previous == '/' && character == '*' {
                            depth += 1;
                        } else if previous == '*' && character == '/' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        previous = character;
                    }
                }
                _ => code.push(character),
            }
        }
        code
    }

    /// The first identifier in `code` that names an RTP payload/PT field.
    /// Unlike a plain substring search this also catches `pt`, `ptype` and
    /// `pt_map` (review C P3-5).
    fn payload_type_field(code: &str) -> Option<String> {
        code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|identifier| !identifier.is_empty())
            .find(|identifier| {
                let identifier = identifier.to_ascii_lowercase();
                identifier == "pt"
                    || identifier.starts_with("pt_")
                    || identifier.ends_with("_pt")
                    || identifier.contains("_pt_")
                    || identifier.contains("payload")
                    || identifier.contains("ptype")
                    || identifier.contains("packet_type")
                    || identifier.contains("packettype")
            })
            .map(str::to_owned)
    }

    #[test]
    fn specs_have_no_payload_type_field() {
        // T2: PT is transport-local. Guard the struct definition itself so a
        // future refactor cannot move it into the SSOT unnoticed. The scan is
        // identifier-based, so short field names (`pt`, `ptype`) cannot slip
        // through a "payload"/"packet_type" substring check (review C P3-5).
        for fixture in [
            "pub struct Fixture { pub payload_type: u8 }",
            "pub struct Fixture { pub pt: u8 }",
            "pub struct Fixture { ptype: u16 }",
            "pub struct Fixture { rtp_pt: u8 }",
        ] {
            assert!(payload_type_field(fixture).is_some(), "the PT guard missed {fixture:?}");
        }
        assert_eq!(payload_type_field("pub struct Fixture { rtp_clock: u32, sample_rate: u32 }"), None);
        let code = codec_spec_struct_code();
        assert_eq!(payload_type_field(&code), None, "CodecSpec must not carry an RTP payload type");
        // Keep the prose-level smell checks as well.
        let lower = code.to_ascii_lowercase();
        assert!(!lower.contains("payload"), "CodecSpec must not carry an RTP payload type");
        assert!(!lower.contains("packet_type"), "CodecSpec must not carry an RTP payload type");
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

    /// Every source file that imports the codec SSOT (directly or via the
    /// `codec_capabilities` facade). Kept explicit so a new consumer must be
    /// reviewed; `ssot_consumer_scan_list_is_complete` proves it matches reality.
    const SSOT_CONSUMER_FILES: &[&str] = &[
        "vks-admin/src/api_v3/shell.rs",
        "vks-admin/src/settings.rs",
        "vks-common/src/codec_capabilities.rs",
        "vks-common/src/codec_spec.rs",
        "vks-common/src/lib.rs",
        "vks-gateway-h323/src/lib.rs",
        "vks-gateway-sip/src/rtp_bridge.rs",
        "vks-gateway-sip/src/uas.rs",
        "vks-media-sfu/src/webrtc.rs",
        "vks-server/src/codec_policy.rs",
        "vks-server/src/gateway_aac.rs",
        "vks-server/src/gateway_audio.rs",
        "vks-server/src/gateways.rs",
        "vks-server/src/main.rs",
        "vks-server/src/transcoder_runtime.rs",
        "vks-signaling/src/handler.rs",
    ];

    /// Explicit, reviewed allowlist of files that may spell canonical codec
    /// names outside `codec_spec.rs`. Every entry states why the spelling is a
    /// transport/UI artifact and which test ties it back to the SSOT. Files not
    /// listed here must not grow a second codec table.
    const NAME_SPELLING_ALLOWLIST: &[(&str, &str)] = &[
        ("vks-common/src/codec_spec.rs", "the SSOT itself"),
        (
            "vks-gateway-sip/src/rtp_bridge.rs",
            "AUDIO_OFFER/VIDEO_OFFER are transport rows keyed by CodecId; checked by offer_rows_match_spec",
        ),
        (
            "vks-gateway-sip/src/uas.rs",
            "SIP UAS static PT map; checked against Disposition by uas_static_pt_map_matches_ssot",
        ),
        (
            "vks-server/src/gateways.rs",
            "server SIP PT maps; checked against Disposition by sip_pt_maps_match_ssot_disposition",
        ),
        (
            "vks-admin/src/settings.rs",
            "operator-intent matrix, not capability truth; every entry resolves via default_codec_matrix_entries_map_to_specs",
        ),
        (
            "vks-media-sfu/src/webrtc.rs",
            "str0m builder branches keyed by CodecId; checked by webrtc_answer_slice_matches_peer_builder",
        ),
        (
            "vks-server/src/transcoder_runtime.rs",
            "prometheus metric labels; subset check worker_metric_labels_are_subset_of_specs",
        ),
        (
            "vks-server/src/main.rs",
            "composition root: proto caps in the ServerHello test; validated against webrtc_answer_specs by production_handler_config_caps_match_ssot_webrtc_dispositions",
        ),
        (
            "vks-common/src/codec_capabilities.rs",
            "native video id dispatch (H264/H265 probe atomics); slices are generated and checked by native_consts_equal_generated_slices",
        ),
        (
            "vks-admin/src/api_v3/shell.rs",
            "vendor /license keys mapped to SSOT spellings; every boolean is derived, checked by license_params_are_derived_from_ssot_dispositions",
        ),
    ];

    /// Files that still contain a codec-name list but cannot import the SSOT.
    /// Documented instead of silently skipped.
    const NON_CONSUMER_EXCEPTIONS: &[(&str, &str)] = &[
        (
            "vks-core/src/media_policy.rs",
            "unused legacy defaults; vks-core deliberately has no vks-common dependency (P3/T04c)",
        ),
        ("vks-gateway-sip/src/codec.rs", "dead legacy CodecMap, removed in T04c"),
        ("vks-gateway-h323/src/codec.rs", "dead legacy CodecMap, removed in T04c"),
    ];

    /// Production-only exceptions: these files do not import the SSOT but
    /// legitimately spell more than one canonical codec name (transport rows,
    /// FFmpeg probe ids, dead legacy maps retired in T04c). Every entry names
    /// the test that ties the spelling back to the SSOT.
    const NON_CONSUMER_NAME_CLUSTER_ALLOWLIST: &[(&str, &str)] = &[
        (
            "vks-gateway-sip/src/uac.rs",
            "SDP fmtp helpers for H264/H265 only; offer rows are validated by offer_rows_match_spec",
        ),
        (
            "vks-gateway-sip/src/g726.rs",
            "G.726 bitrate-to-wire-name helpers; variants live in CODEC_VARIANTS and g726_variants_require_a_supported_bitrate",
        ),
        (
            "vks-server/src/gateway_video.rs",
            "FFmpeg probe enum H264/H265; availability is published into the SSOT atomics in main and checked by video tests",
        ),
        (
            "vks-core/src/media_policy.rs",
            "unused legacy defaults; vks-core deliberately has no vks-common dependency (P3/T04c)",
        ),
        ("vks-gateway-sip/src/codec.rs", "dead legacy CodecMap, removed in T04c"),
        ("vks-gateway-h323/src/codec.rs", "dead legacy CodecMap, removed in T04c"),
    ];

    /// Literal capability tables the SSOT already replaced. Scanned in the
    /// production part of every `crates/vks-*/src/**/*.rs` file (not only in
    /// the files above) so a regression cannot hide behind a new module.
    const FORBIDDEN_CODEC_LITERALS: &[(&str, &[&str], &str)] = &[
        (".with_caps(", &[], "production must not overwrite SSOT-derived RoomCaps"),
        (
            "video_codecs: vec![",
            &["vks-proto/src/lib.rs", "vks-media-mcu/src/engine.rs"],
            "proto round-trip / MCU engine test fixtures, not capability truth",
        ),
        ("audio_codecs: vec![", &[], "production must not build an audio codec list"),
        ("const H323_AUDIO:", &[], "H.323 audio list must derive from CODECS"),
        ("const H323_VIDEO:", &[], "H.323 video list must derive from CODECS"),
        ("107 => \"G726-16\"", &[], "server PT table must derive from the SSOT"),
        ("Some(\"G72616\")", &[], "gateway audio codec names must resolve via the SSOT"),
        ("matches!(normalized.as_str(), \"AAC\"", &[], "AAC identity must come from CodecId"),
        ("\"G72616\" => \"G726-16\"", &[], "UAS PT map must derive from the SSOT"),
        ("pub native: bool", &[], "offer rows must be keyed by CodecId, not a native bit"),
        ("H323_NATIVE_AUDIO_PTS: &[u8] = &[", &[], "H.323 PTs must derive from CODECS"),
        ("\"opus\" | \"pcmu\" | \"pcma\" | \"g722\"", &[], "SDP audio classification must come from CodecKind"),
        ("\"g719\": true", &[], "/license params must derive from the SSOT"),
        ("\"h264-svc\": true", &[], "/license params must derive from the SSOT"),
    ];

    fn read_crate_source(file: &str) -> String {
        let path = format!("{}/{file}", concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {path}"))
    }

    fn collect_rust_files(dir: &std::path::Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display())) {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let relative = path
                    .strip_prefix(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
                    .expect("crate-relative path")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(relative);
            }
        }
    }

    /// Production part of a file: every test module is removed, so production
    /// code before **and after** a test module is scanned.
    ///
    /// Rule (why not "cut at the first `#[cfg(test)]`"): a `#[cfg(test)]`
    /// attribute by itself is not a test module. Files legitimately gate a
    /// `use`/`fn`/`static` helper with it in the middle of production code
    /// (`gateways.rs`), and some files declare their test modules at the top and
    /// keep production below (`api_v3/mod.rs`). Cutting at the first attribute
    /// (or at the first module) hides every production tail after it from the
    /// scanners (review C P2-1). Therefore:
    ///
    /// * an item is a test module when its attribute group contains
    ///   `#[cfg(test)]` (or `#[cfg(all(test, ...))]`); the whole module —
    ///   `mod tests { ... }`, `mod tests;` or `#[path = "..."] mod tests;` — is
    ///   removed and scanning continues after it;
    /// * a bare `mod tests` without attributes is removed only when its name
    ///   looks like a test module and its body contains a `#[test]`-style
    ///   attribute;
    /// * test-only `use`/`fn`/`static` items stay in the scanned text: they are
    ///   not modules and cannot hide a production tail.
    ///
    /// Files included from a `#[cfg(test)] mod` via `#[path]` are skipped by
    /// name in [`is_test_source_file`].
    fn production_source(file: &str) -> String {
        strip_test_modules(&read_crate_source(file))
    }

    /// `source` with all test modules removed; production text is otherwise
    /// byte-identical.
    fn strip_test_modules(source: &str) -> String {
        let mut out = String::with_capacity(source.len());
        let mut cursor = 0;
        for range in test_module_ranges(source) {
            out.push_str(&source[cursor..range.start]);
            cursor = range.end;
        }
        out.push_str(&source[cursor..]);
        out
    }

    /// Byte ranges of the test modules in `source` (see [`production_source`]).
    ///
    /// The scanner is a small Rust lexer: comments and string/char literals are
    /// skipped so braces or `mod` words inside them cannot confuse it.
    fn test_module_ranges(source: &str) -> Vec<std::ops::Range<usize>> {
        let bytes = source.as_bytes();
        let mut ranges = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if let Some(end) = skip_comment_or_literal(bytes, i) {
                i = end;
                continue;
            }
            if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'[') {
                if let Some((group_end, cfg_test)) = attribute_group(bytes, i) {
                    let item = skip_visibility(bytes, skip_trivia(bytes, group_end));
                    let module = module_name(bytes, item).and_then(|(name, name_end)| {
                        let body = skip_trivia(bytes, name_end);
                        module_end(bytes, body).filter(|&end| {
                            cfg_test || (is_test_module_name(name) && body_contains_test_attribute(&source[body..end]))
                        })
                    });
                    if let Some(end) = module {
                        ranges.push(i..end);
                        i = end;
                        continue;
                    }
                    i = skip_trivia(bytes, group_end);
                    continue;
                }
            }
            if keyword_mod_at(bytes, i) {
                if let Some((name, name_end)) = module_name(bytes, i) {
                    if is_test_module_name(name) {
                        let body = skip_trivia(bytes, name_end);
                        if let Some(end) = module_end(bytes, body) {
                            if body_contains_test_attribute(&source[body..end]) {
                                ranges.push(i..end);
                                i = end;
                                continue;
                            }
                        }
                    }
                }
            }
            i += 1;
        }
        ranges
    }

    /// End of the attribute group starting at `start` (a `#[`), plus whether
    /// any attribute in it is a positive `cfg(test)` gate.
    fn attribute_group(bytes: &[u8], start: usize) -> Option<(usize, bool)> {
        let mut end = start;
        let mut cfg_test = false;
        loop {
            if !(bytes.get(end) == Some(&b'#') && bytes.get(end + 1) == Some(&b'[')) {
                return None;
            }
            let attribute_end = matching_delimiter(bytes, end + 1, b'[', b']')?;
            let inner = std::str::from_utf8(bytes.get(end + 2..attribute_end - 1)?).ok()?;
            cfg_test |= is_cfg_test_attribute(inner);
            end = attribute_end;
            let next = skip_trivia(bytes, end);
            if bytes.get(next) == Some(&b'#') && bytes.get(next + 1) == Some(&b'[') {
                end = next;
            } else {
                return Some((end, cfg_test));
            }
        }
    }

    /// `true` only for a gate that requires the test configuration.
    /// `cfg(any(test, ...))` also compiles in production and `cfg(not(test))`
    /// is production-only code, so neither may be treated as a test module.
    fn is_cfg_test_attribute(attribute: &str) -> bool {
        let compact: String = attribute.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(predicate) = compact.strip_prefix("cfg(").and_then(|rest| rest.strip_suffix(')')) else {
            return false;
        };
        predicate == "test"
            || (predicate.starts_with("all(") && contains_word(predicate, "test") && !predicate.contains("not("))
    }

    fn contains_word(haystack: &str, wanted: &str) -> bool {
        haystack.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|token| token == wanted)
    }

    fn is_test_module_name(name: &str) -> bool {
        name == "tests"
            || name.starts_with("test_")
            || name.starts_with("tests_")
            || name.ends_with("_tests")
            || name.ends_with("_test")
    }

    fn body_contains_test_attribute(body: &str) -> bool {
        body.contains("#[test]") || body.contains("::test]") || body.contains("#[rstest]")
    }

    /// End of a `mod` item whose name ends at `name_end` (`{ ... }` or `;`).
    fn module_end(bytes: &[u8], body_start: usize) -> Option<usize> {
        let body = skip_trivia(bytes, body_start);
        match bytes.get(body) {
            Some(b'{') => matching_delimiter(bytes, body, b'{', b'}'),
            Some(b';') => Some(body + 1),
            _ => None,
        }
    }

    fn module_name(bytes: &[u8], start: usize) -> Option<(&str, usize)> {
        let mut i = skip_trivia(bytes, start);
        if !keyword_mod_at(bytes, i) {
            return None;
        }
        i = skip_trivia(bytes, i + 3);
        let name_start = i;
        while bytes.get(i).is_some_and(|byte| is_ident_byte(*byte)) {
            i += 1;
        }
        if i == name_start {
            return None;
        }
        let name = std::str::from_utf8(bytes.get(name_start..i)?).ok()?;
        Some((name, i))
    }

    fn keyword_mod_at(bytes: &[u8], i: usize) -> bool {
        bytes.get(i..).is_some_and(|rest| rest.starts_with(b"mod"))
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && bytes.get(i + 3).is_none_or(|next| !is_ident_byte(*next))
    }

    fn skip_visibility(bytes: &[u8], start: usize) -> usize {
        let mut i = skip_trivia(bytes, start);
        if bytes.get(i..).is_some_and(|rest| rest.starts_with(b"pub"))
            && bytes.get(i + 3).is_none_or(|next| !is_ident_byte(*next))
        {
            i = skip_trivia(bytes, i + 3);
            if bytes.get(i) == Some(&b'(') {
                if let Some(end) = matching_delimiter(bytes, i, b'(', b')') {
                    i = skip_trivia(bytes, end);
                }
            }
        }
        i
    }

    fn is_ident_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }

    fn skip_trivia(bytes: &[u8], start: usize) -> usize {
        let mut i = start;
        loop {
            while bytes.get(i).is_some_and(|byte| byte.is_ascii_whitespace()) {
                i += 1;
            }
            if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'/') {
                i = line_end(bytes, i);
            } else if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'*') {
                i = block_comment_end(bytes, i);
            } else {
                return i;
            }
        }
    }

    /// Skips a comment or a string/char literal starting at `i`; `None` when
    /// the byte does not start one (an ordinary token).
    fn skip_comment_or_literal(bytes: &[u8], i: usize) -> Option<usize> {
        match *bytes.get(i)? {
            b'/' if bytes.get(i + 1) == Some(&b'/') => Some(line_end(bytes, i)),
            b'/' if bytes.get(i + 1) == Some(&b'*') => Some(block_comment_end(bytes, i)),
            b'"' => Some(string_end(bytes, i)),
            b'\'' => char_literal_end(bytes, i),
            b'r' | b'b' | b'c' if i == 0 || !is_ident_byte(bytes[i - 1]) => prefixed_literal_end(bytes, i),
            _ => None,
        }
    }

    fn line_end(bytes: &[u8], start: usize) -> usize {
        let mut i = start;
        while bytes.get(i).is_some_and(|byte| *byte != b'\n') {
            i += 1;
        }
        i
    }

    fn block_comment_end(bytes: &[u8], start: usize) -> usize {
        let mut depth = 0usize;
        let mut i = start;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"/*") {
                depth += 1;
                i += 2;
            } else if bytes[i..].starts_with(b"*/") {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            } else {
                i += 1;
            }
        }
        bytes.len()
    }

    fn string_end(bytes: &[u8], start: usize) -> usize {
        let mut i = start + 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => return i + 1,
                _ => i += 1,
            }
        }
        bytes.len()
    }

    fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
        let mut i = start + 1;
        if bytes.get(i) == Some(&b'\\') {
            i += 2;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'\'' => return Some(i + 1),
                    _ => i += 1,
                }
            }
            return None;
        }
        if bytes.get(i + 1) == Some(&b'\'') {
            Some(i + 2)
        } else {
            None
        }
    }

    /// Raw (`r"..."`, `r#"..."#`), byte (`b"..."`, `br"..."`) and C
    /// (`c"..."`) strings. Returns `None` for raw identifiers (`r#type`).
    fn prefixed_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
        let mut i = start;
        match *bytes.get(i)? {
            b'r' => i += 1,
            b'b' | b'c' if bytes.get(i + 1) == Some(&b'r') => i += 2,
            b'b' | b'c' => {
                if bytes.get(i + 1) == Some(&b'"') {
                    return Some(string_end(bytes, i + 1));
                }
                return None;
            }
            _ => return None,
        }
        let mut hashes = 0;
        while bytes.get(i) == Some(&b'#') {
            hashes += 1;
            i += 1;
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        let mut cursor = i + 1;
        while cursor < bytes.len() {
            if bytes[cursor] == b'"' {
                let mut trailing = 0;
                while trailing < hashes && bytes.get(cursor + 1 + trailing) == Some(&b'#') {
                    trailing += 1;
                }
                if trailing == hashes {
                    return Some(cursor + 1 + hashes);
                }
            }
            cursor += 1;
        }
        Some(bytes.len())
    }

    /// End of the balanced `open_byte`/`close_byte` pair starting at `open`.
    fn matching_delimiter(bytes: &[u8], open: usize, open_byte: u8, close_byte: u8) -> Option<usize> {
        let mut depth = 0usize;
        let mut i = open;
        while i < bytes.len() {
            if let Some(end) = skip_comment_or_literal(bytes, i) {
                i = end;
                continue;
            }
            if bytes[i] == open_byte {
                depth += 1;
            } else if bytes[i] == close_byte {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            i += 1;
        }
        None
    }

    /// Files that are tests themselves (`api_v3/tests.rs`, `*_tests.rs`) and
    /// are included from a `#[cfg(test)] mod` elsewhere. They do not carry an
    /// inner `#[cfg(test)]` marker, so skip them by name.
    fn is_test_source_file(file: &str) -> bool {
        let name = file.rsplit('/').next().unwrap_or(file);
        name == "tests.rs" || name.ends_with("_tests.rs")
    }

    /// Canonical SSOT names (spec names, aliases and alias families) that
    /// appear as quoted string literals in `source`.
    fn codec_names_in(source: &str) -> BTreeSet<&'static str> {
        let mut names = BTreeSet::new();
        for spec in CODECS {
            for name in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
                if source.contains(&format!("\"{name}\"")) {
                    names.insert(name);
                }
            }
        }
        for family in ALIAS_FAMILIES {
            if source.contains(&format!("\"{}\"", family.name)) {
                names.insert(family.name);
            }
        }
        names
    }

    #[test]
    fn ssot_consumer_scan_list_is_complete() {
        // T1: the scan list must cover every crate that imports the SSOT. The
        // walk catches a new consumer file even if nobody remembers to add it.
        let crates_root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut files = Vec::new();
        collect_rust_files(std::path::Path::new(crates_root), &mut files);
        let mut actual = BTreeSet::new();
        for file in &files {
            let source = read_crate_source(file);
            if source.contains("codec_spec") || source.contains("codec_capabilities") {
                actual.insert(file.clone());
            }
        }
        let expected: BTreeSet<String> = SSOT_CONSUMER_FILES.iter().map(|file| (*file).to_string()).collect();
        assert_eq!(
            actual, expected,
            "SSOT consumer scan list drifted: update SSOT_CONSUMER_FILES (or document a NON_CONSUMER_EXCEPTIONS entry)"
        );
        // Documented exceptions must really be non-consumers and must exist.
        for (file, reason) in NON_CONSUMER_EXCEPTIONS {
            assert!(!reason.is_empty());
            let source = read_crate_source(file);
            assert!(
                !source.contains("codec_spec") && !source.contains("codec_capabilities"),
                "{file} is listed as a non-consumer exception but imports the SSOT"
            );
        }
        // Every allowlist reason is documented and the file is a real consumer.
        for (file, reason) in NAME_SPELLING_ALLOWLIST {
            assert!(!reason.is_empty(), "{file} allowlist entry needs a reason");
            assert!(expected.contains(*file), "{file} is allowlisted but not in SSOT_CONSUMER_FILES");
        }
    }

    #[test]
    fn no_second_codec_name_table_outside_allowlist() {
        // T1: the SSOT is only useful if no file keeps its own literal
        // capability table. The known historical copies are scanned in the
        // production part of **every** `crates/vks-*/src/**/*.rs`, so a
        // regression cannot hide in a freshly added module (review B P2-7).
        let crates_root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut files = Vec::new();
        collect_rust_files(std::path::Path::new(crates_root), &mut files);
        let src_files: Vec<String> =
            files.into_iter().filter(|file| file.contains("/src/") && !is_test_source_file(file)).collect();
        assert!(src_files.len() > 40, "scan must cover the real src tree, got {}", src_files.len());
        for (needle, allowed_files, reason) in FORBIDDEN_CODEC_LITERALS {
            for file in &src_files {
                let source = production_source(file);
                if source.contains(needle) {
                    assert!(
                        allowed_files.contains(&file.as_str()),
                        "{file} still contains a literal codec table ({needle:?}): {reason}. \
                         Derive it from CODECS or add a documented allowlist entry with a proving test"
                    );
                }
            }
        }
        // Consumers must still import the SSOT (the completeness test above
        // enforces the exact file list).
        for file in SSOT_CONSUMER_FILES {
            let source = read_crate_source(file);
            assert!(
                source.contains("codec_spec") || source.contains("codec_capabilities"),
                "{file} does not consume the codec SSOT"
            );
        }
    }

    #[test]
    fn no_codec_name_cluster_outside_allowlist() {
        // A fresh file such as `const CODECS2: &[&str] = &["OPUS", "VP8"];` is
        // exactly the T1 regression the SSOT exists to prevent. Catch any
        // production file that spells two or more canonical codec names unless
        // it is explicitly allowlisted with a proving test.
        assert_eq!(
            codec_names_in("const T: &[&str] = &[\"OPUS\", \"VP8\"];").len(),
            2,
            "the cluster scanner itself must detect a fresh literal table"
        );
        let crates_root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut files = Vec::new();
        collect_rust_files(std::path::Path::new(crates_root), &mut files);
        let src_files: Vec<String> =
            files.into_iter().filter(|file| file.contains("/src/") && !is_test_source_file(file)).collect();
        let allowlist: BTreeSet<&str> = NAME_SPELLING_ALLOWLIST
            .iter()
            .chain(NON_CONSUMER_NAME_CLUSTER_ALLOWLIST.iter())
            .map(|(file, _)| *file)
            .collect();
        for (file, reason) in NAME_SPELLING_ALLOWLIST.iter().chain(NON_CONSUMER_NAME_CLUSTER_ALLOWLIST.iter()) {
            assert!(!reason.is_empty(), "{file} allowlist entry needs a reason");
            assert!(src_files.iter().any(|candidate| candidate.as_str() == *file), "{file} allowlist entry is stale");
        }
        let mut offenders = Vec::new();
        for file in &src_files {
            let names = codec_names_in(&production_source(file));
            if names.len() >= 2 && !allowlist.contains(file.as_str()) {
                offenders.push(format!("{file}: {names:?}"));
            }
        }
        assert!(
            offenders.is_empty(),
            "codec name tables outside the explicit allowlist: {offenders:#?}. \
             Derive from CODECS or add a documented allowlist entry with a proving test"
        );
    }

    #[test]
    fn production_source_keeps_items_gated_by_cfg_test_without_a_module() {
        // `#[cfg(test)] use/fn/static` helpers are common and must not cut the
        // production tail. This is exactly the old bug: the first cfg(test)
        // attribute appeared before all production code (review C P2-1).
        let source = concat!(
            "#[cfg(test)]\n",
            "use crate::test_support::Storage;\n\n",
            "#[cfg(test)]\n",
            "fn helper() {}\n\n",
            "const PRODUCTION: &str = \"kept\";\n",
        );
        assert_eq!(strip_test_modules(source), source);
        assert!(codec_names_in(source).is_empty());
    }

    #[test]
    fn production_source_strips_test_modules_and_keeps_production_tails() {
        // Shape of `api_v3/mod.rs` (test declarations first, production below)
        // plus the review C P2-1 regression: a second codec table injected late
        // in the production tail must still be seen by the scanners.
        let source = r#"
#[cfg(test)]
#[path = "observability_tests.rs"]
mod observability_tests;
#[cfg(test)]
mod tests;

pub struct Production;

#[cfg(test)]
mod late_tests {
    const TEST_TABLE: &[&str] = &["OPUS", "VP8"];
    #[test]
    fn hidden() {}
}

const CODECS2: &[&str] = &["OPUS", "VP8"];
"#;
        let production = strip_test_modules(source);
        assert!(production.contains("pub struct Production"), "production after early test modules was dropped");
        assert!(production.contains("const CODECS2"), "late production tail was dropped: {production}");
        assert!(!production.contains("observability_tests.rs"), "test module declaration leaked: {production}");
        assert!(!production.contains("TEST_TABLE"), "test module body leaked: {production}");
        assert_eq!(
            codec_names_in(&production),
            BTreeSet::from(["OPUS", "VP8"]),
            "the scanner must see the late codec table now"
        );
    }

    #[test]
    fn production_source_strips_bare_test_modules_and_ignores_literals() {
        let bare = "pub fn before() {}\n\nmod tests {\n    #[test]\n    fn hidden() {}\n}\n\npub fn after() {}\n";
        let production = strip_test_modules(bare);
        assert!(production.contains("pub fn before") && production.contains("pub fn after"));
        assert!(!production.contains("mod tests"));
        // Braces and `mod` words inside literals/comments are not module items.
        let literals = concat!(
            "const DOC: &str = \"mod tests { #[test] }\";\n",
            "// #[cfg(test)] mod tests { }\n",
            "const RAW: &str = r#\"}\\{\"#;\n",
            "fn production() {}\n",
        );
        assert_eq!(strip_test_modules(literals), literals);
    }

    #[test]
    fn production_source_keeps_real_repo_production_tails() {
        // Every file whose production code follows a real test module must keep
        // that tail in the scan (`api_v3/mod.rs` is the repository case).
        let crates_root = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut files = Vec::new();
        collect_rust_files(std::path::Path::new(crates_root), &mut files);
        let mut tails = 0;
        for file in &files {
            let source = read_crate_source(file);
            let ranges = test_module_ranges(&source);
            let Some(last) = ranges.last() else { continue };
            let tail = source[last.end..].trim();
            if tail.is_empty() {
                continue;
            }
            tails += 1;
            assert!(
                production_source(file).contains(tail),
                "{file}: production tail after the last test module was dropped from the scan"
            );
        }
        assert!(tails > 0, "expected at least one file with production after its test modules");
        // Files the old "first cfg(test)" cut blinded the scanners to.
        for (file, marker) in [
            ("vks-admin/src/api_v3/mod.rs", "pub struct ApiV3State"),
            ("vks-admin/src/api_v3/directory_migrate.rs", "LEGACY_DIRECTORY_KEYS"),
            ("vks-gateway-sip/src/rtp_bridge.rs", "VIDEO_OFFER"),
            ("vks-server/src/gateways.rs", "SIP_AUDIO_PTS"),
        ] {
            assert!(
                production_source(file).contains(marker),
                "{file}: production marker {marker:?} is not scanned (review C P2-1)"
            );
        }
    }
}

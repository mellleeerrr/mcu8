//! AAC-LATM / MP4A-LATM RTP framing helpers (RFC 3016, ISO/IEC 14496-3).
//!
//! FFmpeg's `latm` muxer emits LOAS frames: a 3-byte sync/length header
//! followed by one `audioMuxElement`. RTP carries the `audioMuxElement`
//! **without** that header, so outbound frames are stripped and inbound
//! payloads are wrapped before they reach the `loas` demuxer. This bridge sends
//! one complete element per RTP packet; RFC 3016 also permits fragmentation,
//! which callers must detect (marker=0) rather than feed to `add_loas`.
//!
//! One AAC-LC frame is 1024 samples at every sampling rate. The RTP clock is
//! normally 90 kHz (1920 ticks per frame at 48 kHz, 46.875 packets/s versus a
//! 50 Hz PCM tick); peers such as FFmpeg's RTP LATM muxer may answer with a
//! 48 kHz clock (1024 ticks per frame). Callers must use the negotiated clock
//! and must not assume one packet per 20 ms tick.

/// LOAS sync word (11 bits set, `0x2B7`).
pub const LOAS_SYNC: u8 = 0x56;
/// Bytes of the LOAS `AudioSyncStream` header.
pub const LOAS_HEADER_BYTES: usize = 3;
/// 13-bit LOAS length field maximum.
pub const LOAS_MAX_PAYLOAD: usize = 0x1fff;
/// AAC-LC samples per frame at any sampling rate.
pub const AAC_FRAME_SAMPLES: u32 = 1024;
/// RTP clock advertised for MP4A-LATM.
pub const AAC_RTP_CLOCK: u32 = 90_000;

/// Why a LOAS/audioMuxElement payload is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatmError {
    /// Fewer than 3 bytes (LOAS header) or an empty audioMuxElement.
    TooShort,
    /// First byte is not [`LOAS_SYNC`].
    BadSync,
    /// The 13-bit length does not match the buffer.
    BadLength,
    /// A frame larger than the 13-bit LOAS limit cannot be wrapped.
    TooLarge,
}

/// Splits a LOAS frame into its `audioMuxElement` for the RTP payload.
///
/// # Errors
/// [`LatmError`] if the header is short, the sync word is missing, the length
/// overruns the buffer, or the element is empty.
pub fn strip_loas(frame: &[u8]) -> Result<&[u8], LatmError> {
    if frame.len() < LOAS_HEADER_BYTES {
        return Err(LatmError::TooShort);
    }
    if frame[0] != LOAS_SYNC || frame[1] & 0xe0 != 0xe0 {
        return Err(LatmError::BadSync);
    }
    let length = (usize::from(frame[1] & 0x1f) << 8) | usize::from(frame[2]);
    if length == 0 {
        return Err(LatmError::TooShort);
    }
    if frame.len() != LOAS_HEADER_BYTES + length {
        return Err(LatmError::BadLength);
    }
    Ok(&frame[LOAS_HEADER_BYTES..])
}

/// Wraps an RTP `audioMuxElement` back into a LOAS frame for FFmpeg's `loas`
/// demuxer.
///
/// # Errors
/// [`LatmError::TooShort`] for an empty element and [`LatmError::TooLarge`]
/// above the 13-bit LOAS limit.
pub fn add_loas(audio_mux_element: &[u8]) -> Result<Vec<u8>, LatmError> {
    if audio_mux_element.is_empty() {
        return Err(LatmError::TooShort);
    }
    if audio_mux_element.len() > LOAS_MAX_PAYLOAD {
        return Err(LatmError::TooLarge);
    }
    let length = audio_mux_element.len();
    let mut frame = Vec::with_capacity(LOAS_HEADER_BYTES + length);
    frame.push(LOAS_SYNC);
    frame.push(0xe0 | ((length >> 8) as u8 & 0x1f));
    frame.push(length as u8);
    frame.extend_from_slice(audio_mux_element);
    Ok(frame)
}

/// True when the first bit of `audioMuxElement` is `0`, i.e. the element
/// carries a `StreamMuxConfig` (`useSameStreamMux = 0`). FFmpeg emits this on
/// the first frame of a stream and after an encoder restart; a late RTP joiner
/// must therefore receive that cached first element before later frames.
#[must_use]
pub fn carries_stream_mux_config(audio_mux_element: &[u8]) -> bool {
    audio_mux_element.first().is_some_and(|byte| byte & 0x80 == 0)
}

/// RTP timestamp increment for one AAC frame at `sample_rate`.
///
/// The MP4A-LATM RTP clock is 90 kHz, so 48 kHz gives exactly 1920. Rounded
/// toward the nearest tick; callers keep a running sample counter when the
/// ratio is not integral (44.1 kHz gives 2089.79 -> keep a fractional
/// accumulator instead of using this per-frame value indefinitely).
#[must_use]
pub fn frame_ticks(sample_rate: u32) -> u32 {
    if sample_rate == 0 {
        return 0;
    }
    ((u64::from(AAC_FRAME_SAMPLES) * u64::from(AAC_RTP_CLOCK) + u64::from(sample_rate) / 2) / u64::from(sample_rate))
        as u32
}

/// RTP timestamp generator for AAC-LATM that stays exact for non-integral
/// sample-rate/clock ratios by accumulating the real sample count.
#[derive(Debug, Clone, Copy)]
pub struct LatmRtpClock {
    start: u32,
    samples: u64,
    sample_rate: u32,
    rtp_clock: u32,
}

impl LatmRtpClock {
    /// Creates a clock with the standard 90 kHz MP4A-LATM RTP clock.
    #[must_use]
    pub const fn new(sample_rate: u32, start: u32) -> Self {
        Self::with_rtp_clock(sample_rate, AAC_RTP_CLOCK, start)
    }

    /// Creates a clock for an explicitly negotiated RTP clock (90 kHz or
    /// 48 kHz in practice; any positive value is accepted).
    #[must_use]
    pub const fn with_rtp_clock(sample_rate: u32, rtp_clock: u32, start: u32) -> Self {
        Self {
            start,
            samples: 0,
            sample_rate: if sample_rate == 0 { 48_000 } else { sample_rate },
            rtp_clock: if rtp_clock == 0 { AAC_RTP_CLOCK } else { rtp_clock },
        }
    }

    fn offset(&self) -> u32 {
        ((self.samples * u64::from(self.rtp_clock)) / u64::from(self.sample_rate.max(1))) as u32
    }

    /// The timestamp for the next packet, then advances by one AAC frame.
    pub fn next_timestamp(&mut self) -> u32 {
        let current = self.timestamp();
        self.advance(1);
        current
    }

    /// Advances the clock by `frames` encoded (or lost-but-timed) AAC frames.
    pub fn advance(&mut self, frames: u32) {
        self.samples += u64::from(frames) * u64::from(AAC_FRAME_SAMPLES);
    }

    /// Current (not-yet-sent) timestamp.
    #[must_use]
    pub fn timestamp(&self) -> u32 {
        self.start.wrapping_add(self.offset())
    }

    /// Negotiated RTP clock in Hz.
    #[must_use]
    pub const fn rtp_clock(&self) -> u32 {
        self.rtp_clock
    }
}

/// Number of whole AAC frames accumulated by `samples_in` PCM samples that
/// have not been sent yet. At 48 kHz, 16 ticks of 960 samples yield exactly
/// 15 frames (16 x 960 = 15 x 1024); callers must send 0 or more frames per
/// tick and advance the RTP timestamp only for frames actually sent.
#[must_use]
pub const fn frames_due(samples_in: u64, frames_sent: u64) -> u32 {
    let due = samples_in / AAC_FRAME_SAMPLES as u64;
    let pending = due.saturating_sub(frames_sent);
    if pending > u32::MAX as u64 {
        u32::MAX
    } else {
        pending as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![LOAS_SYNC, 0xe0 | ((payload.len() >> 8) as u8 & 0x1f), payload.len() as u8];
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn strips_and_readds_the_three_byte_loas_header() {
        let payload = [0x20, 0x10, 0x05, 0xaa, 0xbb];
        let wrapped = frame(&payload);
        let stripped = strip_loas(&wrapped).expect("strip");
        assert_eq!(stripped, payload);
        assert_eq!(add_loas(stripped).expect("add"), wrapped);
    }

    #[test]
    fn strip_rejects_bad_sync_length_or_short_buffer() {
        assert_eq!(strip_loas(&[]), Err(LatmError::TooShort));
        assert_eq!(strip_loas(&[0x55, 0xe0, 0x02, 1, 2]), Err(LatmError::BadSync));
        assert_eq!(strip_loas(&[0x56, 0x00, 0x02, 1, 2]), Err(LatmError::BadSync));
        assert_eq!(strip_loas(&[0x56, 0xe0, 0x03, 1, 2]), Err(LatmError::BadLength));
        assert_eq!(strip_loas(&[0x56, 0xe0, 0x00]), Err(LatmError::TooShort));
        assert_eq!(strip_loas(&[0x56, 0xe0, 0x02, 1, 2, 3]), Err(LatmError::BadLength));
    }

    #[test]
    fn add_rejects_empty_or_larger_than_13_bit_length() {
        assert_eq!(add_loas(&[]), Err(LatmError::TooShort));
        assert_eq!(add_loas(&vec![0; LOAS_MAX_PAYLOAD + 1]), Err(LatmError::TooLarge));
        let max = add_loas(&vec![7; LOAS_MAX_PAYLOAD]).expect("max length");
        assert_eq!(max.len(), LOAS_HEADER_BYTES + LOAS_MAX_PAYLOAD);
        assert_eq!(strip_loas(&max).expect("strip max"), vec![7; LOAS_MAX_PAYLOAD]);
    }

    #[test]
    fn detects_in_band_stream_mux_config_first_bit() {
        assert!(carries_stream_mux_config(&[0x20]));
        assert!(carries_stream_mux_config(&[0x00]));
        assert!(!carries_stream_mux_config(&[0x80]));
        assert!(!carries_stream_mux_config(&[0xff]));
        assert!(!carries_stream_mux_config(&[]));
    }

    #[test]
    fn sixteen_pcm_ticks_accumulate_fifteen_aac_frames() {
        assert_eq!(frames_due(0, 0), 0);
        assert_eq!(frames_due(960, 0), 0);
        for tick in 1..=15u64 {
            assert_eq!(frames_due(tick * 960, 0), (tick as u32 * 960) / 1024, "tick {tick}");
        }
        assert_eq!(frames_due(16 * 960, 0), 15);
        assert_eq!(frames_due(16 * 960, 15), 0, "no artificial frame on the 16th tick");
        assert_eq!(frames_due(32 * 960, 15), 15);
        assert_eq!(frames_due(32 * 960, 20), 10, "lost/failed frames still advance time");
    }

    #[test]
    fn rtp_clock_supports_90k_and_48k_without_drift() {
        let mut ninety = LatmRtpClock::new(48_000, 100);
        assert_eq!(ninety.rtp_clock(), 90_000);
        let mut timestamps = Vec::new();
        for _ in 0..16 {
            timestamps.push(ninety.next_timestamp());
        }
        assert_eq!(timestamps[0], 100);
        assert_eq!(timestamps[15] - timestamps[0], 15 * 1920);
        assert_eq!(ninety.timestamp(), 100 + 16 * 1920);
        let mut forty_eight = LatmRtpClock::with_rtp_clock(48_000, 48_000, 7);
        let first = forty_eight.next_timestamp();
        let second = forty_eight.next_timestamp();
        assert_eq!(second - first, 1024);
        // 44.1 kHz: 45 frames are 2089.79 ticks each; the accumulator keeps the
        // long-run error below one tick instead of drifting 0.38 s/hour.
        let mut cd = LatmRtpClock::new(44_100, 0);
        let start = cd.next_timestamp();
        cd.advance(44);
        let end = cd.next_timestamp();
        let expected = (45u64 * 1024 * 90_000) / 44_100;
        assert!((u64::from(end - start)).abs_diff(expected) <= 1, "{} vs {expected}", end - start);
    }

    #[test]
    fn frame_ticks_is_exact_at_48k_and_rounded_elsewhere() {
        assert_eq!(frame_ticks(48_000), 1920);
        assert_eq!(frame_ticks(44_100), 2090);
        assert_eq!(frame_ticks(32_000), 2880);
        assert_eq!(frame_ticks(0), 0);
        assert_eq!(LatmRtpClock::new(0, 0).next_timestamp(), 0);
        assert_eq!(LatmRtpClock::new(0, 5).next_timestamp(), 5);
    }

    #[test]
    fn rtp_clock_advances_exactly_at_48k() {
        let mut clock = LatmRtpClock::new(48_000, 7);
        let ticks: Vec<u32> = (0..5).map(|_| clock.next_timestamp()).collect();
        assert_eq!(ticks, vec![7, 1927, 3847, 5767, 7687]);
    }

    #[test]
    fn rtp_clock_keeps_integral_drift_with_sample_accumulator() {
        let mut clock = LatmRtpClock::new(44_100, 0);
        let mut last = 0;
        for _ in 0..1000 {
            let next = clock.next_timestamp();
            assert!(next >= last, "RTP timestamps must not go backwards");
            last = next;
        }
        // The 1000th call returns the offset of 999 already-sent frames.
        let expected = (999u64 * 1024 * 90_000 / 44_100) as u32;
        assert_eq!(last, expected);
    }
}

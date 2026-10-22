//! AAC-LATM / MP4A-LATM RTP framing helpers (RFC 3016, ISO/IEC 14496-3).
//!
//! FFmpeg's `latm` muxer emits LOAS frames: a 3-byte sync/length header
//! followed by one `audioMuxElement`. RTP carries the `audioMuxElement`
//! **without** that header, so outbound frames are stripped and inbound
//! payloads are wrapped before they reach the `loas` demuxer. Every RTP packet
//! holds exactly one complete element; there is no fragmentation layer.
//!
//! One AAC-LC frame is 1024 samples, so at 48 kHz the 90 kHz RTP timestamp
//! advances by exactly 1920 per packet (46.875 packets/s versus a 50 Hz PCM
//! tick — callers must not assume one packet per 20 ms tick).

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
    if frame[0] != LOAS_SYNC {
        return Err(LatmError::BadSync);
    }
    let length = (usize::from(frame[1] & 0x1f) << 8) | usize::from(frame[2]);
    if length == 0 || frame.len() != LOAS_HEADER_BYTES + length {
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

/// Monotonic 90 kHz RTP timestamp generator that stays exact for non-integral
/// sample-rate ratios by accumulating the real sample count.
#[derive(Debug, Clone, Copy)]
pub struct LatmRtpClock {
    start: u32,
    samples: u64,
    sample_rate: u32,
}

impl LatmRtpClock {
    /// Creates a clock for `sample_rate`; timestamp starts at `start`.
    #[must_use]
    pub const fn new(sample_rate: u32, start: u32) -> Self {
        Self { start, samples: 0, sample_rate }
    }

    fn offset(&self) -> u32 {
        ((self.samples * u64::from(AAC_RTP_CLOCK)) / u64::from(self.sample_rate.max(1))) as u32
    }

    /// The timestamp for the next packet, then advances by one AAC frame.
    pub fn next_timestamp(&mut self) -> u32 {
        let current = self.start.wrapping_add(self.offset());
        self.samples += u64::from(AAC_FRAME_SAMPLES);
        current
    }

    /// Current (not-yet-sent) timestamp.
    #[must_use]
    pub fn timestamp(&self) -> u32 {
        self.start.wrapping_add(self.offset())
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
        assert_eq!(strip_loas(&[0x56, 0xe0, 0x03, 1, 2]), Err(LatmError::BadLength));
        assert_eq!(strip_loas(&[0x56, 0xe0, 0x00]), Err(LatmError::BadLength));
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
    fn frame_ticks_is_exact_at_48k_and_rounded_elsewhere() {
        assert_eq!(frame_ticks(48_000), 1920);
        assert_eq!(frame_ticks(44_100), 2090);
        assert_eq!(frame_ticks(32_000), 2880);
        assert_eq!(frame_ticks(0), 0);
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

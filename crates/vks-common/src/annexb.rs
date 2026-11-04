//! Unified Annex-B NAL unit parsing and streaming splitter.
//!
//! Handles 3-byte (`0x00 0x00 0x01`), 4-byte (`0x00 0x00 0x00 0x01`),
//! and arbitrary leading/trailing zero padding according to ITU-T H.264 Annex B
//! and ITU-T H.265 Annex B.

/// Default maximum buffer size for streaming splitter before resetting (4 MiB).
pub const DEFAULT_MAX_ANNEXB_BUFFER: usize = 4 * 1024 * 1024;

/// Finds the next Annex-B start code in `bytes` starting at `from`.
///
/// Returns `Some((prefix_start, body_start))`, where:
/// - `prefix_start` is the start index of the start code prefix (including all leading zeros).
/// - `body_start` is the index of the first byte of the NAL payload (immediately following `0x01`).
#[must_use]
pub fn find_start_code(bytes: &[u8], from: usize) -> Option<(usize, usize)> {
    if bytes.len() < from + 3 {
        return None;
    }
    let rel_pos = bytes[from..].windows(3).position(|w| w == [0, 0, 1])?;
    let p = from + rel_pos;
    let mut start = p;
    while start > from && bytes[start - 1] == 0 {
        start -= 1;
    }
    Some((start, p + 3))
}

/// Strips one leading 3-, 4- or N-byte Annex-B start code, returning a slice of the NAL payload.
#[must_use]
#[inline]
pub fn strip_start_code(nal: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < nal.len() && nal[i] == 0 {
        i += 1;
    }
    if i >= 2 && i < nal.len() && nal[i] == 1 {
        &nal[i + 1..]
    } else {
        nal
    }
}

/// Strips trailing padding zeros (`trailing_zero_8bits`) from a NAL payload slice.
#[must_use]
#[inline]
pub fn trim_trailing_zeros(mut nal: &[u8]) -> &[u8] {
    while let Some((&0, rest)) = nal.split_last() {
        nal = rest;
    }
    nal
}

/// Zero-allocation iterator yielding NAL slices from a contiguous Annex-B byte buffer.
pub fn split_annexb(bytes: &[u8], keep_start_code: bool) -> impl Iterator<Item = &[u8]> {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        while cursor < bytes.len() {
            let (start, body) = find_start_code(bytes, cursor)?;
            let slice_start = if keep_start_code { start } else { body };
            match find_start_code(bytes, body) {
                Some((next, _)) => {
                    cursor = next;
                    if next > slice_start {
                        return Some(&bytes[slice_start..next]);
                    }
                }
                None => {
                    cursor = bytes.len();
                    if bytes.len() > slice_start {
                        return Some(&bytes[slice_start..]);
                    }
                }
            }
        }
        None
    })
}

/// Splits a complete Annex-B buffer into owned NAL units.
#[must_use]
pub fn split_nals(bytes: &[u8], keep_start_code: bool) -> Vec<Vec<u8>> {
    split_annexb(bytes, keep_start_code).map(|s| s.to_vec()).collect()
}

/// Streaming Annex-B NAL unit reassembler with cross-chunk start code preservation.
#[derive(Debug, Clone)]
pub struct AnnexbSplitter {
    buffer: Vec<u8>,
    max_buffer_size: usize,
}

impl Default for AnnexbSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl AnnexbSplitter {
    /// Creates a splitter with the default 4 MiB buffer limit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_size(DEFAULT_MAX_ANNEXB_BUFFER)
    }

    /// Creates a splitter with an explicit buffer limit.
    #[must_use]
    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer_size: max_size,
        }
    }

    /// Feeds new bytes into the buffer and extracts completed NAL units with start codes.
    pub fn push_stream_units(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.push_internal(chunk, true)
    }

    /// Feeds new bytes into the buffer and extracts completed NAL units without start codes.
    pub fn push_rtp_units(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.push_internal(chunk, false)
    }

    /// Flushes the trailing in-progress NAL unit upon reaching EOF.
    pub fn finish(&mut self, keep_start_code: bool) -> Option<Vec<u8>> {
        let (first, body) = find_start_code(&self.buffer, 0)?;
        if first > 0 {
            self.buffer.drain(..first);
        }
        let start = if keep_start_code { 0 } else { body.saturating_sub(first) };
        let unit = self.buffer[start..].to_vec();
        self.buffer.clear();
        if unit.is_empty() {
            None
        } else {
            Some(unit)
        }
    }

    /// Clears internal buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    /// Current buffered byte count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Whether buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn push_internal(&mut self, chunk: &[u8], keep_start_code: bool) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > self.max_buffer_size {
            self.buffer.clear();
            return Vec::new();
        }
        let mut units = Vec::new();
        loop {
            let Some((first, body)) = find_start_code(&self.buffer, 0) else {
                // Keep the last up to 3 bytes: a start code prefix may span across read chunks.
                let keep = self.buffer.len().saturating_sub(3);
                self.buffer.drain(..keep);
                break;
            };
            if first > 0 {
                // Discard garbage before first start code.
                self.buffer.drain(..first);
                continue;
            }
            let Some((next, _)) = find_start_code(&self.buffer, body) else {
                break;
            };
            let start = if keep_start_code { 0 } else { body };
            if next > start {
                units.push(self.buffer[start..next].to_vec());
            }
            self.buffer.drain(..next);
        }
        units
    }
}

// ==================== H.264 NAL Classifiers ====================

/// Extracts H.264 NAL unit type (0..=31) if valid (forbidden_zero_bit == 0).
#[must_use]
#[inline]
pub fn nal_type_h264(nal: &[u8]) -> Option<u8> {
    let &h = strip_start_code(nal).first()?;
    (h & 0x80 == 0).then_some(h & 0x1f)
}

/// Returns true if NAL is an H.264 IDR picture (type 5).
#[must_use]
#[inline]
pub fn is_idr_h264(nal: &[u8]) -> bool {
    nal_type_h264(nal) == Some(5)
}

/// Returns true if NAL is an H.264 parameter set (SPS = 7 or PPS = 8).
#[must_use]
#[inline]
pub fn is_parameter_set_h264(nal: &[u8]) -> bool {
    matches!(nal_type_h264(nal), Some(7 | 8))
}

/// Returns true if NAL is an H.264 Access Unit Delimiter (AUD = 9).
#[must_use]
#[inline]
pub fn is_aud_h264(nal: &[u8]) -> bool {
    nal_type_h264(nal) == Some(9)
}

// ==================== H.265 NAL Classifiers ====================

/// Extracts H.265 NAL unit type (0..=63) if valid (forbidden_zero_bit == 0).
#[must_use]
#[inline]
pub fn nal_type_h265(nal: &[u8]) -> Option<u8> {
    let &h = strip_start_code(nal).first()?;
    (h & 0x80 == 0).then_some((h >> 1) & 0x3f)
}

/// Returns true if NAL is an H.265 IRAP picture (types 16..=23: BLA, IDR, CRA).
#[must_use]
#[inline]
pub fn is_irap_h265(nal: &[u8]) -> bool {
    matches!(nal_type_h265(nal), Some(16..=23))
}

/// Returns true if NAL is an H.265 parameter set (VPS = 32, SPS = 33, PPS = 34).
#[must_use]
#[inline]
pub fn is_parameter_set_h265(nal: &[u8]) -> bool {
    matches!(nal_type_h265(nal), Some(32..=34))
}

/// Returns true if NAL is an H.265 Access Unit Delimiter (AUD = 35).
#[must_use]
#[inline]
pub fn is_aud_h265(nal: &[u8]) -> bool {
    nal_type_h265(nal) == Some(35)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_start_code_handles_3_4_and_multi_zero_prefixes() {
        let data = [0, 0, 1, 0x67, 0, 0, 0, 1, 0x68, 0, 0, 0, 0, 1, 0x65];
        let sc1 = find_start_code(&data, 0).expect("first start code");
        assert_eq!(sc1, (0, 3));

        let sc2 = find_start_code(&data, sc1.1).expect("second start code");
        assert_eq!(sc2, (4, 8));

        let sc3 = find_start_code(&data, sc2.1).expect("third start code");
        assert_eq!(sc3, (9, 14));
    }

    #[test]
    fn strip_start_code_preserves_payload() {
        assert_eq!(strip_start_code(&[0, 0, 1, 0x67, 1, 2]), &[0x67, 1, 2]);
        assert_eq!(strip_start_code(&[0, 0, 0, 1, 0x68, 3]), &[0x68, 3]);
        assert_eq!(strip_start_code(&[0, 0, 0, 0, 1, 0x65]), &[0x65]);
        assert_eq!(strip_start_code(&[0x65, 1, 2]), &[0x65, 1, 2]);
    }

    #[test]
    fn emulation_prevention_byte_is_not_treated_as_start_code() {
        let data = [0, 0, 1, 0x65, 0, 0, 3, 1, 0, 0, 1, 0x67];
        let sc1 = find_start_code(&data, 0).expect("first");
        assert_eq!(sc1, (0, 3));
        let sc2 = find_start_code(&data, sc1.1).expect("second");
        assert_eq!(sc2, (8, 11));
    }

    #[test]
    fn trim_trailing_zeros_works() {
        assert_eq!(trim_trailing_zeros(&[1, 2, 3, 0, 0, 0]), &[1, 2, 3]);
        assert_eq!(trim_trailing_zeros(&[1, 2, 3]), &[1, 2, 3]);
        assert_eq!(trim_trailing_zeros(&[0, 0]), &[] as &[u8]);
    }

    #[test]
    fn split_annexb_iterator() {
        let stream = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2];
        let with_sc: Vec<&[u8]> = split_annexb(&stream, true).collect();
        assert_eq!(with_sc.len(), 2);
        assert_eq!(with_sc[0], &[0, 0, 0, 1, 0x67, 1]);
        assert_eq!(with_sc[1], &[0, 0, 1, 0x68, 2]);

        let without_sc: Vec<&[u8]> = split_annexb(&stream, false).collect();
        assert_eq!(without_sc.len(), 2);
        assert_eq!(without_sc[0], &[0x67, 1]);
        assert_eq!(without_sc[1], &[0x68, 2]);
    }

    #[test]
    fn splitter_handles_split_start_codes_across_chunks() {
        let mut splitter = AnnexbSplitter::new();
        // Chunk 1 ends in partial start code `[0, 0]`
        let chunk1 = [0, 0, 0, 1, 0x67, 0x42, 0, 0];
        let u1 = splitter.push_rtp_units(&chunk1);
        assert!(u1.is_empty(), "waiting for next start code to delimit first NAL");

        // Chunk 2 finishes the start code with `[1, 0x68]`
        let chunk2 = [1, 0x68, 0xCE, 0, 0, 1, 0x65];
        let u2 = splitter.push_rtp_units(&chunk2);
        assert_eq!(u2.len(), 2);
        assert_eq!(u2[0], &[0x67, 0x42]);
        assert_eq!(u2[1], &[0x68, 0xCE]);

        // Finish flushes trailing NAL
        let last = splitter.finish(false);
        assert_eq!(last, Some(vec![0x65]));
    }

    #[test]
    fn classifiers_h264_and_h265() {
        // H.264
        assert!(is_idr_h264(&[0, 0, 1, 0x25])); // type 5
        assert!(is_idr_h264(&[0x65])); // type 5
        assert!(is_parameter_set_h264(&[0x67])); // SPS = 7
        assert!(is_parameter_set_h264(&[0x68])); // PPS = 8
        assert!(is_aud_h264(&[0x09])); // AUD = 9
        assert!(!is_idr_h264(&[0x41])); // Non-IDR = 1

        // H.265
        assert!(is_irap_h265(&[0, 0, 0, 1, 0x26, 1])); // IDR_W_RADL = 19 (header 0x26 >> 1 = 19)
        assert!(is_irap_h265(&[0x28, 1])); // IDR_N_LP = 20
        assert!(is_irap_h265(&[0x2a, 1])); // CRA_NUT = 21
        assert!(is_parameter_set_h265(&[0x40, 1])); // VPS = 32
        assert!(is_parameter_set_h265(&[0x42, 1])); // SPS = 33
        assert!(is_parameter_set_h265(&[0x44, 1])); // PPS = 34
        assert!(is_aud_h265(&[0x46, 1])); // AUD = 35
        assert!(!is_irap_h265(&[0x02, 1])); // TRAIL_R = 1
    }
}

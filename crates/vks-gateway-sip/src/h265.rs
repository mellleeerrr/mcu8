//! H.265/HEVC RTP payload format (RFC 7798).
//!
//! Pure functions only: the gateway owns sockets, SSRC and the RTP clock;
//! this module owns the two-byte HEVC NAL header, FU (type 49) fragmentation,
//! AP (type 48) parsing and the 1200-byte MTU bound. It intentionally does not
//! aggregate small NALs into AP packets: single-NAL packets are always legal
//! and keep parameter-set ordering obvious for a bridge.
//!
//! The RTP timestamp is not touched here; the caller sets one timestamp per
//! access unit (90 kHz) and marks the last packet of that AU.

use crate::rtp_bridge::write_rtp_header;

/// Network MTU budget used by the SIP gateway for RTP (IPv4/UDP 1500 minus
/// 20+8 headers leaves 1472; 1200 keeps room for tunnels and is the H.264
/// gateway's existing target).
pub const H265_RTP_MTU: usize = 1200;
/// RTP header length (no CSRC/extension).
pub const RTP_HEADER_BYTES: usize = 12;
/// Largest single-NAL payload: MTU - RTP header.
pub const H265_MAX_SINGLE_NAL: usize = H265_RTP_MTU - RTP_HEADER_BYTES;
/// Largest FU payload after the 2-byte PayloadHdr and 1-byte FU header.
pub const H265_MAX_FU_CHUNK: usize = H265_MAX_SINGLE_NAL - 3;
/// Hard cap for one reconstructed NAL; matches the worker's Annex-B cap.
pub const H265_MAX_NAL_BYTES: usize = 4 * 1024 * 1024;

/// HEVC NAL unit type: VPS.
pub const NAL_VPS: u8 = 32;
/// HEVC NAL unit type: SPS.
pub const NAL_SPS: u8 = 33;
/// HEVC NAL unit type: PPS.
pub const NAL_PPS: u8 = 34;
/// HEVC NAL unit type: access unit delimiter.
pub const NAL_AUD: u8 = 35;

const NAL_AP: u8 = 48;
const NAL_FU: u8 = 49;

/// Why a H.265 payload could not be produced or parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H265Error {
    /// Payload shorter than the 2-byte NAL header / 3-byte FU header.
    TooShort,
    /// F bit set or TID+1 == 0.
    BadHeader,
    /// NAL type 48/49 nested inside AP, or a NAL-unit-like type 50..=63 that
    /// must never reach a decoder (RFC 7798 §6).
    BadType,
    /// FU start and end bits set together, empty fragment, or a gap in the
    /// fragment sequence/timestamp.
    BadFragment,
    /// AP size field overruns the payload or is smaller than a NAL header.
    BadAggregation,
    /// A NAL or reconstructed frame exceeds [`H265_MAX_NAL_BYTES`].
    TooLarge,
}

/// Returns the six-bit HEVC NAL type, or `None` for an empty payload (only the
/// first header byte is needed).
#[must_use]
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|header| (header >> 1) & 0x3f)
}

/// True for IRAP pictures (BLA 16-18, IDR 19-20, CRA 21) — the decoder can
/// start here without earlier frames.
#[must_use]
pub fn is_irap(nal: &[u8]) -> bool {
    matches!(nal_type(nal), Some(16..=21))
}

/// True for VPS/SPS/PPS — the parameter sets a late joiner needs in SDP.
#[must_use]
pub fn is_parameter_set(nal: &[u8]) -> bool {
    matches!(nal_type(nal), Some(NAL_VPS | NAL_SPS | NAL_PPS))
}

/// Splits an Annex-B byte stream into raw NALs (without start codes).
///
/// x265 emits both 3- and 4-byte start codes; leading bytes before the first
/// start code are ignored. Empty NALs are skipped.
#[must_use]
pub fn split_annexb(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= stream.len() {
        if stream[index..].starts_with(&[0, 0, 1]) {
            starts.push((index, 3));
            index += 3;
        } else if stream[index..].starts_with(&[0, 0, 0, 1]) && index + 4 <= stream.len() {
            starts.push((index, 4));
            index += 4;
        } else {
            index += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (position, (start, prefix)) in starts.iter().enumerate() {
        let body = start + prefix;
        let end = starts.get(position + 1).map_or(stream.len(), |(next, _)| *next);
        if end > body {
            nals.push(&stream[body..end]);
        }
    }
    nals
}

/// Incremental Annex-B splitter for streaming pipe reads.
///
/// [`split_annexb`] needs a complete buffer; a bridge reading 16 KiB pipe
/// chunks must use this instead, otherwise a NAL split across reads is lost.
/// Incomplete trailing bytes stay buffered until the next push or [`Self::finish`].
///
/// PACI (type 50) and DONL/DOND (`sprop-max-don-diff > 0`) are deliberately
/// unsupported: SDP must not advertise a non-zero `sprop-max-don-diff`.
#[derive(Debug, Default)]
pub struct AnnexBSplitter {
    buffer: Vec<u8>,
}

impl AnnexBSplitter {
    /// Consumes one pipe chunk and returns every NAL completed in it.
    ///
    /// # Errors
    /// [`H265Error::TooLarge`] when a single in-progress NAL exceeds
    /// [`H265_MAX_NAL_BYTES`]; the buffer is cleared so the caller can resync.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>, H265Error> {
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > H265_MAX_NAL_BYTES + 4 {
            self.buffer.clear();
            return Err(H265Error::TooLarge);
        }
        let mut units = Vec::new();
        loop {
            let Some((first, prefix)) = find_start_code(&self.buffer, 0) else {
                // Keep the last three bytes: a 3-/4-byte start code may span reads.
                let keep = self.buffer.len().saturating_sub(3);
                self.buffer.drain(..keep);
                return Ok(units);
            };
            if first > 0 {
                self.buffer.drain(..first);
            }
            let body = prefix;
            let Some((next, _)) = find_start_code(&self.buffer, body) else {
                // Current NAL is not complete yet; keep its start code in buffer.
                return Ok(units);
            };
            if next > body {
                units.push(self.buffer[body..next].to_vec());
            }
            self.buffer.drain(..next);
        }
    }

    /// Flushes the buffered trailing NAL at EOF (empty if none).
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let (first, prefix) = find_start_code(&self.buffer, 0)?;
        if first > 0 {
            self.buffer.drain(..first);
        }
        let unit = self.buffer[prefix..].to_vec();
        self.buffer.clear();
        if unit.is_empty() {
            None
        } else {
            Some(unit)
        }
    }
}

fn find_start_code(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut index = start;
    while index + 3 <= bytes.len() {
        if bytes[index..].starts_with(&[0, 0, 1]) {
            return Some((index, 3));
        }
        if index + 4 <= bytes.len() && bytes[index..].starts_with(&[0, 0, 0, 1]) {
            return Some((index, 4));
        }
        index += 1;
    }
    None
}

/// Strips one leading 3- or 4-byte Annex-B start code, if present.
///
/// `FfmpegWorker::read_encoded_video` returns NALs *with* the start code, while
/// [`packetize_h265`] expects the raw
/// NAL beginning at its two-byte header.
#[must_use]
pub fn strip_start_code(nal: &[u8]) -> &[u8] {
    if nal.starts_with(&[0, 0, 0, 1]) {
        &nal[4..]
    } else if nal.starts_with(&[0, 0, 1]) {
        &nal[3..]
    } else {
        nal
    }
}

/// Packetizes one access unit (ordered raw NALs, no start codes).
///
/// `marker` is set only on the final packet of the final NAL. NALs larger than
/// [`H265_MAX_SINGLE_NAL`] are fragmented as RFC 7798 FUs; the FU payload omits
/// the original two-byte NAL header, which the receiver reconstructs from the
/// PayloadHdr and FuType.
///
/// # Errors
/// Returns [`H265Error`] for an empty/malformed NAL, a reserved type, an
/// oversized NAL, or an empty access unit.
pub fn packetize_h265(
    nals: &[&[u8]],
    payload_type: u8,
    ssrc: u32,
    sequence: &mut u16,
    timestamp: u32,
) -> Result<Vec<Vec<u8>>, H265Error> {
    if nals.is_empty() {
        return Err(H265Error::TooShort);
    }
    // Validate the whole AU first: a bad later NAL must not leave `sequence`
    // advanced for packets that were never sent.
    for nal in nals {
        validate_nal(nal)?;
        if nal.len() > H265_MAX_NAL_BYTES {
            return Err(H265Error::TooLarge);
        }
    }
    let mut packets = Vec::new();
    for (index, nal) in nals.iter().enumerate() {
        let is_last_nal = index + 1 == nals.len();
        if nal.len() <= H265_MAX_SINGLE_NAL {
            let mut packet = vec![0; RTP_HEADER_BYTES];
            write_rtp_header(&mut packet, payload_type, *sequence, timestamp, ssrc, is_last_nal);
            *sequence = sequence.wrapping_add(1);
            packet.extend_from_slice(nal);
            packets.push(packet);
            continue;
        }
        let body = &nal[2..];
        let chunks: Vec<&[u8]> = body.chunks(H265_MAX_FU_CHUNK).collect();
        for (part, chunk) in chunks.iter().enumerate() {
            let start = part == 0;
            let end = part + 1 == chunks.len();
            let mut packet = vec![0; RTP_HEADER_BYTES];
            write_rtp_header(&mut packet, payload_type, *sequence, timestamp, ssrc, is_last_nal && end);
            *sequence = sequence.wrapping_add(1);
            // PayloadHdr: F/LayerId/TID from the original header, type 49.
            packet.push((nal[0] & 0x81) | (NAL_FU << 1));
            packet.push(nal[1]);
            packet.push((if start { 0x80 } else { 0 }) | (if end { 0x40 } else { 0 }) | ((nal[0] >> 1) & 0x3f));
            packet.extend_from_slice(chunk);
            debug_assert!(packet.len() <= H265_RTP_MTU);
            packets.push(packet);
        }
    }
    Ok(packets)
}

fn validate_nal(nal: &[u8]) -> Result<(), H265Error> {
    if nal.len() < 2 {
        return Err(H265Error::TooShort);
    }
    if nal[0] & 0x80 != 0 || nal[1] & 0x07 == 0 {
        return Err(H265Error::BadHeader);
    }
    match (nal[0] >> 1) & 0x3f {
        NAL_AP..=63 => Err(H265Error::BadType),
        _ => Ok(()),
    }
}

/// Reassembles one H.265 RTP stream into raw NALs (no start codes).
///
/// A single packet can yield several NALs only for AP (type 48). A completed
/// FU yields exactly one NAL. The caller groups NALs into access units by RTP
/// timestamp/marker, as it does for H.264.
#[derive(Debug, Default)]
pub struct H265Depacketizer {
    fragment: Vec<u8>,
    fragment_timestamp: u32,
    last_sequence: u16,
}

impl H265Depacketizer {
    /// Drops any partially received FU (e.g. on packet loss) without error.
    pub fn reset(&mut self) {
        self.fragment.clear();
    }

    /// Consumes one RTP payload.
    ///
    /// # Errors
    /// [`H265Error`] on malformed headers, nested/illegal types, AP size
    /// overruns, a gap in FU sequence/timestamp, or an oversized frame. The
    /// partially received FU is discarded on every error.
    pub fn push(&mut self, sequence: u16, timestamp: u32, payload: &[u8]) -> Result<Vec<Vec<u8>>, H265Error> {
        if payload.len() < 2 {
            self.fragment.clear();
            return Err(H265Error::TooShort);
        }
        if payload[0] & 0x80 != 0 || payload[1] & 0x07 == 0 {
            self.fragment.clear();
            return Err(H265Error::BadHeader);
        }
        let nal_type = (payload[0] >> 1) & 0x3f;
        match nal_type {
            NAL_AP => self.push_ap(payload),
            NAL_FU => self.push_fu(sequence, timestamp, payload),
            50..=63 => {
                self.fragment.clear();
                Err(H265Error::BadType)
            }
            _ => {
                self.fragment.clear();
                if payload.len() > H265_MAX_NAL_BYTES {
                    return Err(H265Error::TooLarge);
                }
                Ok(vec![payload.to_vec()])
            }
        }
    }

    fn push_ap(&mut self, payload: &[u8]) -> Result<Vec<Vec<u8>>, H265Error> {
        self.fragment.clear();
        let mut position = 2;
        let mut units = Vec::new();
        while position < payload.len() {
            if position + 2 > payload.len() {
                return Err(H265Error::BadAggregation);
            }
            let size = usize::from(u16::from_be_bytes([payload[position], payload[position + 1]]));
            position += 2;
            if size < 2 || position + size > payload.len() {
                return Err(H265Error::BadAggregation);
            }
            let unit = &payload[position..position + size];
            if unit[0] & 0x80 != 0 || unit[1] & 0x07 == 0 {
                return Err(H265Error::BadHeader);
            }
            if ((unit[0] >> 1) & 0x3f) >= NAL_AP {
                return Err(H265Error::BadType);
            }
            units.push(unit.to_vec());
            position += size;
        }
        if units.len() < 2 {
            // RFC 7798 §4.4.2: an AP aggregates at least two NAL units.
            return Err(H265Error::BadAggregation);
        }
        Ok(units)
    }

    fn push_fu(&mut self, sequence: u16, timestamp: u32, payload: &[u8]) -> Result<Vec<Vec<u8>>, H265Error> {
        if payload.len() < 4 {
            // 3 bytes is a FU header with an empty payload, forbidden by
            // RFC 7798 §4.4.3.
            self.fragment.clear();
            return Err(if payload.len() < 3 { H265Error::TooShort } else { H265Error::BadFragment });
        }
        let start = payload[2] & 0x80 != 0;
        let end = payload[2] & 0x40 != 0;
        let fu_type = payload[2] & 0x3f;
        if start && end || fu_type >= NAL_AP {
            self.fragment.clear();
            return Err(H265Error::BadFragment);
        }
        if start {
            if payload.len() - 3 > H265_MAX_NAL_BYTES {
                self.fragment.clear();
                return Err(H265Error::TooLarge);
            }
            self.fragment.clear();
            // Keep the original F/LayerId/TID bits, insert FuType.
            self.fragment.push((payload[0] & 0x81) | (fu_type << 1));
            self.fragment.push(payload[1]);
            self.fragment_timestamp = timestamp;
        } else if self.fragment.is_empty()
            || timestamp != self.fragment_timestamp
            || sequence != self.last_sequence.wrapping_add(1)
            || (payload[0] & 0x81) | (fu_type << 1) != self.fragment[0]
            || payload[1] != self.fragment[1]
        {
            self.fragment.clear();
            return Err(H265Error::BadFragment);
        }
        self.last_sequence = sequence;
        if self.fragment.len() + payload.len() - 3 > H265_MAX_NAL_BYTES {
            self.fragment.clear();
            return Err(H265Error::TooLarge);
        }
        self.fragment.extend_from_slice(&payload[3..]);
        if end {
            Ok(vec![std::mem::take(&mut self.fragment)])
        } else {
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(header: [u8; 2], body: &[u8]) -> Vec<u8> {
        let mut out = header.to_vec();
        out.extend_from_slice(body);
        out
    }

    fn vcl(nal_type: u8, body: &[u8]) -> Vec<u8> {
        nal([(nal_type << 1), 0x01], body)
    }

    fn rtp_payload(packet: &[u8]) -> (u16, u32, bool, &[u8]) {
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let marker = packet[1] & 0x80 != 0;
        (sequence, timestamp, marker, &packet[RTP_HEADER_BYTES..])
    }

    #[test]
    fn splits_three_and_four_byte_start_codes_without_empty_nals() {
        let stream = [0, 0, 0, 1, 0x40, 0x01, 0, 0, 1, 0x42, 0x02, 0, 0, 1];
        let nals = split_annexb(&stream);
        assert_eq!(nals, vec![&[0x40, 0x01][..], &[0x42, 0x02][..]]);
    }

    #[test]
    fn packetizes_small_nal_unchanged_and_marks_last() {
        let nals = [vcl(1, b"slice"), nal([NAL_SPS << 1, 1], b"sps")];
        let refs: Vec<&[u8]> = nals.iter().map(Vec::as_slice).collect();
        let mut sequence = 7;
        let packets = packetize_h265(&refs, 99, 0x1122_3344, &mut sequence, 9000).expect("packets");
        assert_eq!(packets.len(), 2);
        assert_eq!(sequence, 9);
        let (seq, timestamp, marker, payload) = rtp_payload(&packets[0]);
        assert_eq!(seq, 7);
        assert_eq!(timestamp, 9000);
        assert!(!marker);
        assert_eq!(payload, vcl(1, b"slice"));
        let (seq, _, marker, payload) = rtp_payload(&packets[1]);
        assert_eq!(seq, 8);
        assert!(marker);
        assert_eq!(payload[0] >> 1 & 0x3f, NAL_SPS);
    }

    #[test]
    fn fragments_large_nal_with_fu_and_never_exceeds_mtu() {
        let large = vcl(19, &vec![0xa5; 3000]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 42).expect("packets");
        assert!(packets.len() >= 3);
        assert!(packets.iter().all(|packet| packet.len() <= H265_RTP_MTU));
        let mut receiver = H265Depacketizer::default();
        let mut rebuilt = Vec::new();
        for packet in &packets {
            let (seq, ts, marker, payload) = rtp_payload(packet);
            assert_eq!(payload[0] >> 1 & 0x3f, NAL_FU);
            let nals = receiver.push(seq, ts, payload).expect("fragment");
            if marker {
                assert_eq!(nals.len(), 1);
                rebuilt = nals.into_iter().next().unwrap();
            }
        }
        assert_eq!(rebuilt, large);
    }

    #[test]
    fn fu_start_and_end_bits_are_set_once_and_marker_only_on_last_packet() {
        let large = vcl(1, &vec![1; 2500]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 7).expect("packets");
        for (index, packet) in packets.iter().enumerate() {
            let (_, _, marker, payload) = rtp_payload(packet);
            let fu = payload[2];
            assert_eq!(fu & 0x80 != 0, index == 0, "S bit on first fragment only");
            assert_eq!(fu & 0x40 != 0, index + 1 == packets.len(), "E bit on last fragment only");
            assert_eq!(marker, index + 1 == packets.len());
        }
    }

    #[test]
    fn single_access_unit_can_reuse_rtp_timestamp_for_all_nals() {
        let nals = [nal([NAL_VPS << 1, 1], &[1]), nal([NAL_SPS << 1, 1], &[2]), nal([NAL_PPS << 1, 1], &[3])];
        let refs: Vec<&[u8]> = nals.iter().map(Vec::as_slice).collect();
        let mut sequence = 0;
        let packets = packetize_h265(&refs, 99, 1, &mut sequence, 55).expect("packets");
        assert!(packets.iter().all(|packet| rtp_payload(packet).1 == 55));
        assert!(!rtp_payload(&packets[0]).2 && !rtp_payload(&packets[1]).2 && rtp_payload(&packets[2]).2);
    }

    #[test]
    fn depacketizes_ap_with_two_byte_sizes() {
        let first = vcl(1, b"a");
        let second = nal([NAL_SPS << 1, 1], b"bb");
        let mut payload = vec![(NAL_AP << 1), 0x01];
        payload.extend_from_slice(&(first.len() as u16).to_be_bytes());
        payload.extend_from_slice(&first);
        payload.extend_from_slice(&(second.len() as u16).to_be_bytes());
        payload.extend_from_slice(&second);
        let mut receiver = H265Depacketizer::default();
        let units = receiver.push(1, 2, &payload).expect("ap");
        assert_eq!(units, vec![first, second]);
    }

    #[test]
    fn rejects_nested_and_reserved_types() {
        let mut payload = vec![(NAL_AP << 1), 0x01];
        let nested = vec![(NAL_FU << 1), 0x01, 0x80];
        payload.extend_from_slice(&(nested.len() as u16).to_be_bytes());
        payload.extend_from_slice(&nested);
        assert_eq!(H265Depacketizer::default().push(1, 1, &payload), Err(H265Error::BadType));
    }

    #[test]
    fn drops_partial_fu_on_sequence_gap_and_resets_cleanly() {
        let large = vcl(1, &vec![9; 2600]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 3).expect("packets");
        let mut receiver = H265Depacketizer::default();
        let (seq, ts, _, payload) = rtp_payload(&packets[0]);
        assert!(receiver.push(seq, ts, payload).expect("start").is_empty());
        let (_, _, _, payload) = rtp_payload(&packets[2]);
        assert_eq!(receiver.push(seq.wrapping_add(2), ts, payload), Err(H265Error::BadFragment));
        // A fresh start after a gap is accepted.
        let (seq, ts, _, payload) = rtp_payload(&packets[0]);
        assert!(receiver.push(seq, ts, payload).expect("restart").is_empty());
    }

    #[test]
    fn rejects_bad_headers_and_empty_access_unit() {
        assert_eq!(H265Depacketizer::default().push(1, 1, &[]), Err(H265Error::TooShort));
        assert_eq!(H265Depacketizer::default().push(1, 1, &[0x80, 0x01]), Err(H265Error::BadHeader));
        assert_eq!(H265Depacketizer::default().push(1, 1, &[0x02, 0x00]), Err(H265Error::BadHeader));
        assert_eq!(packetize_h265(&[], 99, 1, &mut 0, 1), Err(H265Error::TooShort));
        assert_eq!(packetize_h265(&[&[0x02, 0x00]], 99, 1, &mut 0, 1), Err(H265Error::BadHeader));
    }

    #[test]
    fn detects_irap_and_parameter_sets() {
        assert!(is_irap(&vcl(19, b"idr")));
        assert!(is_irap(&vcl(21, b"cra")));
        assert!(!is_irap(&vcl(1, b"p")));
        assert!(is_parameter_set(&nal([NAL_VPS << 1, 1], b"v")));
        assert!(is_parameter_set(&nal([NAL_PPS << 1, 1], b"p")));
        assert!(!is_parameter_set(&vcl(1, b"slice")));
        assert_eq!(nal_type(&[]), None);
    }

    #[test]
    fn roundtrips_fragmented_trail_n_type_zero() {
        let large = vcl(0, &vec![0xab; 3000]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 11).expect("packets");
        assert!(packets.len() >= 3);
        let mut receiver = H265Depacketizer::default();
        let mut rebuilt = Vec::new();
        for packet in &packets {
            let (seq, ts, _, payload) = rtp_payload(packet);
            if let Some(nal) = receiver.push(seq, ts, payload).expect("trail_n").into_iter().next() {
                rebuilt = nal;
            }
        }
        assert_eq!(rebuilt, large);
    }

    #[test]
    fn rejects_reserved_nal_unit_like_types_everywhere() {
        let reserved = [(52 << 1), 0x01];
        assert_eq!(H265Depacketizer::default().push(1, 1, &reserved), Err(H265Error::BadType));
        assert_eq!(packetize_h265(&[&reserved], 99, 1, &mut 0, 1), Err(H265Error::BadType));
        let mut ap = vec![(NAL_AP << 1), 0x01];
        let unit = [(55 << 1), 0x01];
        ap.extend_from_slice(&(unit.len() as u16).to_be_bytes());
        ap.extend_from_slice(&unit);
        assert_eq!(H265Depacketizer::default().push(1, 1, &ap), Err(H265Error::BadType));
    }

    #[test]
    fn rejects_empty_fu_payload_and_single_unit_ap() {
        // Type 49, S bit set, FuType 1, but no payload bytes.
        assert_eq!(H265Depacketizer::default().push(1, 1, &[0x62, 0x01, 0x80]), Err(H265Error::BadFragment));
        let one = vcl(1, b"a");
        let mut ap = vec![(NAL_AP << 1), 0x01];
        ap.extend_from_slice(&(one.len() as u16).to_be_bytes());
        ap.extend_from_slice(&one);
        assert_eq!(H265Depacketizer::default().push(1, 1, &ap), Err(H265Error::BadAggregation));
    }

    #[test]
    fn exact_mtu_boundary_and_large_cap() {
        let edge = vcl(1, &vec![0; H265_MAX_SINGLE_NAL - 2]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&edge], 99, 1, &mut sequence, 1).expect("edge");
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), H265_RTP_MTU);
        let over = vcl(1, &vec![0; H265_MAX_SINGLE_NAL - 1]);
        let packets = packetize_h265(&[&over], 99, 1, &mut sequence, 1).expect("over");
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].len(), H265_RTP_MTU);
        let too_large = vcl(1, &vec![0; H265_MAX_NAL_BYTES]);
        assert_eq!(packetize_h265(&[&too_large], 99, 1, &mut 0, 1), Err(H265Error::TooLarge));
    }

    #[test]
    fn sequence_wraps_and_layer_id_tid_are_preserved() {
        let mut sequence = u16::MAX;
        let mut header = [0u8, 0u8];
        header[0] = 19 << 1; // type 19, LayerId high bit 0
        header[1] = (1 << 3) | 2; // LayerId low 5 bits = 1, TID+1 = 2
        let large = nal(header, &vec![0x5a; 2400]);
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 5).expect("packets");
        let sequences: Vec<u16> = packets.iter().map(|packet| rtp_payload(packet).0).collect();
        assert_eq!(sequences.first(), Some(&u16::MAX));
        assert_eq!(sequences.last(), Some(&1));
        let mut receiver = H265Depacketizer::default();
        let mut rebuilt = Vec::new();
        for packet in &packets {
            let (seq, ts, _, payload) = rtp_payload(packet);
            if let Some(nal) = receiver.push(seq, ts, payload).expect("layer").into_iter().next() {
                rebuilt = nal;
            }
        }
        assert_eq!(rebuilt, large);
        assert_eq!(rebuilt[1] & 0x07, 2);
    }

    #[test]
    fn streaming_splitter_survives_read_boundaries_and_flushes_at_eof() {
        let first = vcl(1, b"first-slice");
        let second = nal([NAL_SPS << 1, 1], b"second");
        let mut stream = vec![0, 0, 0, 1];
        stream.extend_from_slice(&first);
        stream.extend_from_slice(&[0, 0, 1]);
        stream.extend_from_slice(&second);
        let mut splitter = AnnexBSplitter::default();
        let mut units = Vec::new();
        for chunk in stream.chunks(4) {
            units.extend(splitter.push(chunk).expect("splitter"));
        }
        units.extend(splitter.finish());
        assert_eq!(units, vec![first, second]);
    }

    #[test]
    fn start_code_and_reset_helpers() {
        assert_eq!(strip_start_code(&[0, 0, 0, 1, 0x26, 1]), &[0x26, 1]);
        assert_eq!(strip_start_code(&[0, 0, 1, 0x26, 1]), &[0x26, 1]);
        assert_eq!(strip_start_code(&[0x26, 1]), &[0x26, 1]);
        let mut receiver = H265Depacketizer::default();
        let large = vcl(1, &vec![4; 2600]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 2).expect("packets");
        let (seq, ts, _, payload) = rtp_payload(&packets[0]);
        assert!(receiver.push(seq, ts, payload).expect("start").is_empty());
        receiver.reset();
        let (seq, ts, _, payload) = rtp_payload(&packets[1]);
        assert_eq!(receiver.push(seq, ts, payload), Err(H265Error::BadFragment));
    }

    #[test]
    fn splitter_handles_four_byte_start_code_split_across_reads_and_garbage() {
        let nals = [vcl(1, b"one"), nal([NAL_VPS << 1, 1], b"two")];
        let mut stream = vec![9, 9, 0, 0, 0, 1];
        stream.extend_from_slice(&nals[0]);
        stream.extend_from_slice(&[0, 0, 0, 1]);
        stream.extend_from_slice(&nals[1]);
        let mut splitter = AnnexBSplitter::default();
        let mut units = Vec::new();
        for byte in &stream {
            units.extend(splitter.push(std::slice::from_ref(byte)).expect("byte push"));
        }
        units.extend(splitter.finish());
        assert_eq!(units, vec![nals[0].clone(), nals[1].clone()]);
    }

    #[test]
    fn splitter_rejects_oversized_in_progress_nal() {
        let mut splitter = AnnexBSplitter::default();
        assert_eq!(splitter.push(&vec![0; H265_MAX_NAL_BYTES + 5]), Err(H265Error::TooLarge));
        // The buffer was cleared: a fresh NAL parses normally.
        let unit = vcl(1, b"ok");
        let mut stream = vec![0, 0, 1];
        stream.extend_from_slice(&unit);
        assert!(splitter.push(&stream).expect("recovery").is_empty());
        assert_eq!(splitter.finish(), Some(unit));
    }

    #[test]
    fn fu_middle_fragment_type_or_timestamp_mismatch_is_rejected() {
        let large = vcl(7, &vec![3; 2600]);
        let mut sequence = 0;
        let packets = packetize_h265(&[&large], 99, 1, &mut sequence, 9).expect("packets");
        let (seq0, ts, _, payload0) = rtp_payload(&packets[0]);
        let (seq1, _, _, payload1) = rtp_payload(&packets[1]);
        let (seq2, _, _, payload2) = rtp_payload(&packets[2]);
        let mut receiver = H265Depacketizer::default();
        assert!(receiver.push(seq0, ts, payload0).expect("start").is_empty());
        assert!(receiver.push(seq1, ts, payload1).expect("middle").is_empty());
        // Changed FuType in the middle fragment.
        let mut wrong_type = payload2.to_vec();
        wrong_type[2] = (wrong_type[2] & 0xc0) | 8;
        assert_eq!(receiver.push(seq2, ts, &wrong_type), Err(H265Error::BadFragment));
        // Restart, then a timestamp change in the middle fragment.
        assert!(receiver.push(seq0, ts, payload0).expect("restart").is_empty());
        assert_eq!(receiver.push(seq1, ts + 1, payload1), Err(H265Error::BadFragment));
    }

    #[test]
    fn boundary_nal_types_are_classified_exactly() {
        for nal_type in 0..=47u8 {
            let unit = vcl(nal_type, b"x");
            assert_eq!(nal_type_of(&unit), nal_type);
        }
        for nal_type in 48..=63u8 {
            let mut header = [0u8; 2];
            header[0] = nal_type << 1;
            header[1] = 1;
            assert!(H265Depacketizer::default().push(1, 1, &header).is_err(), "type {nal_type}");
            assert_eq!(packetize_h265(&[&header], 99, 1, &mut 0, 1), Err(H265Error::BadType), "type {nal_type}");
        }
    }

    #[test]
    fn packetizer_does_not_advance_sequence_on_late_validation_error() {
        let good = vcl(1, b"good");
        let bad = [0x80, 0x01];
        let mut sequence = 41;
        assert_eq!(packetize_h265(&[&good, &bad], 99, 1, &mut sequence, 1), Err(H265Error::BadHeader));
        assert_eq!(sequence, 41);
    }

    fn nal_type_of(nal: &[u8]) -> u8 {
        nal_type(nal).expect("header")
    }
}

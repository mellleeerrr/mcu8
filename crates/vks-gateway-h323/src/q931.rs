//! Strict TPKT/Q.931 framing. H.225/H.245 ASN.1 is decoded by the sidecar.
//!
//! An IP-looking byte sequence in opaque User-User data is never a negotiated
//! RTP address. In particular, no BER-style `0xa0` FastStart marker exists in
//! H.225 aligned PER.

use std::net::Ipv4Addr;
use vks_common::Error;

pub const TPKT_VERSION: u8 = 3;
pub const MSG_ALERTING: u8 = 0x01;
pub const MSG_CALL_PROCEEDING: u8 = 0x02;
pub const MSG_CONNECT: u8 = 0x07;
pub const MSG_SETUP: u8 = 0x05;
pub const MSG_RELEASE_COMPLETE: u8 = 0x5a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Q931Message {
    pub message_type: u8,
    pub call_reference: u16,
    /// Opaque ASN.1 PER octets; negotiation belongs to the real codec.
    pub h225_user_information: Option<Vec<u8>>,
    pub cause: Option<u8>,
    /// Retained for source compatibility; framing alone cannot populate media.
    pub remote_media_ip: Option<Ipv4Addr>,
    pub remote_media_port: Option<u16>,
}

pub fn encode_tpkt(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= u16::MAX as usize - 4, "TPKT payload exceeds RFC 1006 frame bound");
    let mut output = vec![TPKT_VERSION, 0];
    output.extend_from_slice(&((payload.len() + 4) as u16).to_be_bytes());
    output.extend_from_slice(payload);
    output
}

/// Caller-originated RELEASE COMPLETE, normal clearing. The call-reference
/// direction bit stays zero when the endpoint that originated SETUP hangs up.
pub fn build_q931_release_complete(call_ref: u16) -> Vec<u8> {
    let mut payload = vec![8, 2];
    payload.extend_from_slice(&(call_ref & 0x7fff).to_be_bytes());
    payload.extend_from_slice(&[MSG_RELEASE_COMPLETE, 8, 2, 0x80, 0x90]);
    encode_tpkt(&payload)
}

pub fn parse_q931_message(data: &[u8]) -> Result<Q931Message, Error> {
    let invalid = |message: &str| Error::InvalidInput(message.into());
    if data.len() < 8 || data[0..2] != [3, 0] {
        return Err(invalid("Invalid or truncated TPKT header"));
    }
    let length = u16::from_be_bytes([data[2], data[3]]) as usize;
    if length != data.len() || length < 8 {
        return Err(invalid("TPKT length mismatch: supply exactly one complete frame"));
    }
    let payload = &data[4..];
    if payload[0] != 8 || !matches!(payload[1], 1 | 2) {
        return Err(invalid("Invalid Q.931 protocol/call reference length"));
    }
    let reference_length = payload[1] as usize;
    if payload.len() < reference_length + 3 {
        return Err(invalid("Truncated Q.931 call reference"));
    }
    let call_reference = if reference_length == 1 {
        (payload[2] & 0x7f) as u16
    } else {
        u16::from_be_bytes([payload[2], payload[3]]) & 0x7fff
    };
    let message_type = payload[reference_length + 2];
    let mut position = reference_length + 3;
    let mut user_information = None;
    let mut cause = None;
    while position < payload.len() {
        let tag = payload[position];
        position += 1;
        if tag & 0x80 != 0 {
            continue; // Single-octet IE, including codeset shift.
        }
        let count = if tag == 0x7e { 2 } else { 1 };
        if position + count > payload.len() {
            return Err(invalid("Truncated Q.931 IE length"));
        }
        let size = if count == 2 {
            u16::from_be_bytes([payload[position], payload[position + 1]]) as usize
        } else {
            payload[position] as usize
        };
        position += count;
        let value = payload.get(position..position + size).ok_or_else(|| invalid("Truncated Q.931 IE value"))?;
        position += size;
        if tag == 0x7e {
            if value.first() != Some(&5) || user_information.is_some() {
                return Err(invalid("Invalid or duplicate H.225 User-User IE"));
            }
            user_information = Some(value[1..].to_vec());
        } else if tag == 8 && value.len() >= 2 {
            cause = value.last().map(|value| value & 0x7f);
        }
    }
    Ok(Q931Message {
        message_type,
        call_reference,
        h225_user_information: user_information,
        cause,
        remote_media_ip: None,
        remote_media_port: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_polycom_connect_uses_two_octet_user_user_length() {
        // H.225 PER is deliberately opaque at the framing boundary.
        let packet = encode_tpkt(&[8, 2, 0xb1, 0x57, 7, 0x7e, 0, 4, 5, 0x22, 0xc0, 0]);
        let parsed = parse_q931_message(&packet).unwrap();
        assert_eq!(parsed.call_reference, 0x3157);
        assert_eq!(parsed.h225_user_information, Some(vec![0x22, 0xc0, 0]));
        assert_eq!(parsed.remote_media_port, None);
    }

    #[test]
    fn invented_fast_start_and_ip_scan_cannot_create_media() {
        let packet = encode_tpkt(&[8, 2, 0, 1, 7, 0x7e, 0, 11, 5, 0xa0, 8, 1, 0, 192, 168, 1, 132, 0x13, 0x8c]);
        let parsed = parse_q931_message(&packet).unwrap();
        assert_eq!(parsed.remote_media_ip, None);
        assert_eq!(parsed.remote_media_port, None);
    }

    #[test]
    fn rejects_truncated_or_coalesced_frames_and_old_single_byte_length() {
        let packet = build_q931_release_complete(42);
        assert!(parse_q931_message(&packet[..packet.len() - 1]).is_err());
        assert!(parse_q931_message(&[packet.clone(), packet].concat()).is_err());
        assert!(parse_q931_message(&encode_tpkt(&[8, 2, 0, 1, 7, 0x7e, 3, 5, 0xa0, 0])).is_err());
    }

    #[test]
    fn originating_hangup_preserves_call_reference_direction() {
        let packet = build_q931_release_complete(42);
        assert_eq!(packet[6] & 0x80, 0);
        let parsed = parse_q931_message(&packet).unwrap();
        assert_eq!(parsed.call_reference, 42);
        assert_eq!(parsed.cause, Some(16));
    }
}

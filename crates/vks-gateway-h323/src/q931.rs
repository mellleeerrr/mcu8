//! Micro Q.931 / H.225.0 Fast-Start builder and parser for H.323 outbound UAC.
//!
//! Provides lightweight, pure-Rust TPKT framing (RFC 1006) and Q.931 signaling
//! without heavyweight C++ dependencies.

use std::net::Ipv4Addr;
use vks_common::Error;

pub const TPKT_VERSION: u8 = 3;
pub const Q931_PROTOCOL_DISCRIMINATOR: u8 = 0x08;
pub const H225_PROTOCOL_DISCRIMINATOR: u8 = 0x05;

pub const MSG_ALERTING: u8 = 0x01;
pub const MSG_CALL_PROCEEDING: u8 = 0x02;
pub const MSG_CONNECT: u8 = 0x07;
pub const MSG_SETUP: u8 = 0x05;
pub const MSG_RELEASE_COMPLETE: u8 = 0x5a;

/// Parsed Q.931 response from remote H.323 endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Q931Message {
    pub message_type: u8,
    pub call_reference: u16,
    pub remote_media_ip: Option<Ipv4Addr>,
    pub remote_media_port: Option<u16>,
}

/// Wraps payload in 4-byte TPKT header (RFC 1006).
pub fn encode_tpkt(payload: &[u8]) -> Vec<u8> {
    let total_len = (payload.len() + 4) as u16;
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.push(TPKT_VERSION);
    out.push(0x00);
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Builds a Q.931 SETUP message with H.225.0 Fast-Start for G.711 PCMU audio.
pub fn build_q931_setup(
    call_ref: u16,
    caller_alias: &str,
    callee_alias: &str,
    local_ip: Ipv4Addr,
    local_rtp_port: u16,
) -> Vec<u8> {
    let mut q931 = Vec::with_capacity(256);
    // Protocol Discriminator
    q931.push(Q931_PROTOCOL_DISCRIMINATOR);
    // Call Reference length (2 bytes)
    q931.push(0x02);
    q931.extend_from_slice(&call_ref.to_be_bytes());
    // Message Type: SETUP
    q931.push(MSG_SETUP);

    // Bearer Capability (IE 0x04)
    q931.push(0x04);
    q931.push(0x03);
    q931.extend_from_slice(&[0x88, 0x90, 0xa5]); // 3.1 kHz Audio

    // Calling Party Number (IE 0x6c)
    let caller_bytes = caller_alias.as_bytes();
    q931.push(0x6c);
    q931.push((caller_bytes.len() + 1) as u8);
    q931.push(0x01); // presentation allowed
    q931.extend_from_slice(caller_bytes);

    // Called Party Number (IE 0x70)
    let callee_bytes = callee_alias.as_bytes();
    q931.push(0x70);
    q931.push((callee_bytes.len() + 1) as u8);
    q931.push(0x80); // unknown numbering plan
    q931.extend_from_slice(callee_bytes);

    // User-User IE (IE 0x7e) containing FastStart H.225.0 Setup-UUIE
    let mut uu_body = Vec::with_capacity(128);
    uu_body.push(H225_PROTOCOL_DISCRIMINATOR); // 0x05

    // Fast-Start OpenLogicalChannel payload for G.711 PCMU (payload type 0)
    // Structure: Tag (0xA0 = FastStart), length, IP (4 bytes), Port (2 bytes)
    uu_body.push(0xa0); // FastStart sequence tag
    let mut fs_body = Vec::with_capacity(32);
    // Forward channel (Tx from MCU to Terminal)
    fs_body.push(0x01); // logicalChannelNumber 1
    fs_body.push(0x00); // G.711 PCMU codec
    fs_body.extend_from_slice(&local_ip.octets());
    fs_body.extend_from_slice(&local_rtp_port.to_be_bytes());
    // Reverse channel (Rx to MCU from Terminal)
    fs_body.push(0x02); // logicalChannelNumber 2
    fs_body.push(0x00); // G.711 PCMU codec
    fs_body.extend_from_slice(&local_ip.octets());
    fs_body.extend_from_slice(&local_rtp_port.to_be_bytes());

    uu_body.push(fs_body.len() as u8);
    uu_body.extend_from_slice(&fs_body);

    // User-User header in Q.931: 0x7e, length
    q931.push(0x7e);
    if uu_body.len() < 128 {
        q931.push(uu_body.len() as u8);
    } else {
        let len = uu_body.len() as u16;
        q931.extend_from_slice(&len.to_be_bytes());
    }
    q931.extend_from_slice(&uu_body);

    encode_tpkt(&q931)
}

/// Builds a Q.931 RELEASE COMPLETE message.
pub fn build_q931_release_complete(call_ref: u16) -> Vec<u8> {
    let mut q931 = Vec::with_capacity(16);
    q931.push(Q931_PROTOCOL_DISCRIMINATOR);
    q931.push(0x02);
    // High bit set indicates flag from terminator
    q931.extend_from_slice(&(call_ref | 0x8000).to_be_bytes());
    q931.push(MSG_RELEASE_COMPLETE);

    // Cause IE (0x08)
    q931.push(0x08);
    q931.push(0x02);
    q931.extend_from_slice(&[0x80, 0x90]); // Normal call clearing (Cause 16)

    encode_tpkt(&q931)
}

/// Parses a TPKT-framed Q.931 packet and extracts message type, call reference, and media endpoint.
pub fn parse_q931_message(data: &[u8]) -> Result<Q931Message, Error> {
    if data.len() < 4 {
        return Err(Error::InvalidInput("TPKT packet too short".into()));
    }
    if data[0] != TPKT_VERSION {
        return Err(Error::InvalidInput(format!("Invalid TPKT version: {}", data[0])));
    }
    let tpkt_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    if data.len() < tpkt_len {
        return Err(Error::InvalidInput(format!("Truncated TPKT packet: expected {tpkt_len}, got {}", data.len())));
    }

    let q931_data = &data[4..tpkt_len];
    if q931_data.len() < 4 {
        return Err(Error::InvalidInput("Q.931 header too short".into()));
    }
    if q931_data[0] != Q931_PROTOCOL_DISCRIMINATOR {
        return Err(Error::InvalidInput(format!("Invalid Q.931 protocol discriminator: 0x{:02x}", q931_data[0])));
    }

    let cr_len = q931_data[1] as usize;
    if q931_data.len() < 2 + cr_len + 1 {
        return Err(Error::InvalidInput("Q.931 header truncated around call reference".into()));
    }
    let call_reference = if cr_len == 2 {
        u16::from_be_bytes([q931_data[2], q931_data[3]]) & 0x7fff
    } else if cr_len == 1 {
        (q931_data[2] & 0x7f) as u16
    } else {
        0
    };

    let msg_type = q931_data[2 + cr_len];

    // Search for User-User IE (0x7e) or FastStart markers in remaining payload
    let mut remote_media_ip = None;
    let mut remote_media_port = None;

    let mut idx = 2 + cr_len + 1;
    while idx < q931_data.len() {
        let ie_tag = q931_data[idx];
        idx += 1;
        if idx >= q931_data.len() {
            break;
        }
        let ie_len = q931_data[idx] as usize;
        idx += 1;
        if idx + ie_len > q931_data.len() {
            break;
        }
        let ie_data = &q931_data[idx..idx + ie_len];
        idx += ie_len;

        if ie_tag == 0x7e && ie_data.len() >= 8 {
            // 1. Check for structured FastStart tag 0xa0 inside User-User
            for i in 0..ie_data.len().saturating_sub(9) {
                if ie_data[i] == 0xa0 {
                    let ip = Ipv4Addr::new(ie_data[i + 4], ie_data[i + 5], ie_data[i + 6], ie_data[i + 7]);
                    let port = u16::from_be_bytes([ie_data[i + 8], ie_data[i + 9]]);
                    if port > 1024 && !ip.is_unspecified() {
                        remote_media_ip = Some(ip);
                        remote_media_port = Some(port);
                        break;
                    }
                }
            }

            // 2. Fallback heuristic scan for IP + RTP port sequence in User-User IE
            if remote_media_port.is_none() {
                for i in 0..ie_data.len().saturating_sub(6) {
                    let ip0 = ie_data[i];
                    let ip1 = ie_data[i + 1];
                    let ip2 = ie_data[i + 2];
                    let ip3 = ie_data[i + 3];
                    let port = u16::from_be_bytes([ie_data[i + 4], ie_data[i + 5]]);
                    if (ip0 == 10 || ip0 == 172 || ip0 == 192 || ((1..=223).contains(&ip0) && ip0 != 127))
                        && port >= 1024
                        && port.is_multiple_of(2)
                    {
                        remote_media_ip = Some(Ipv4Addr::new(ip0, ip1, ip2, ip3));
                        remote_media_port = Some(port);
                        break;
                    }
                }
            }
        }
    }

    Ok(Q931Message { message_type: msg_type, call_reference, remote_media_ip, remote_media_port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_q931_setup_and_parse() {
        let local_ip = Ipv4Addr::new(192, 168, 1, 93);
        let port = 41002;
        let setup_bytes = build_q931_setup(0x1234, "vks-mcu", "terminal-132", local_ip, port);

        assert_eq!(setup_bytes[0], TPKT_VERSION);
        let msg = parse_q931_message(&setup_bytes).expect("parse failed");
        assert_eq!(msg.message_type, MSG_SETUP);
        assert_eq!(msg.call_reference, 0x1234);
        assert_eq!(msg.remote_media_ip, Some(local_ip));
        assert_eq!(msg.remote_media_port, Some(port));
    }

    #[test]
    fn test_q931_release_complete() {
        let rel_bytes = build_q931_release_complete(0x0042);
        let msg = parse_q931_message(&rel_bytes).expect("parse release failed");
        assert_eq!(msg.message_type, MSG_RELEASE_COMPLETE);
        assert_eq!(msg.call_reference, 0x0042);
    }
}

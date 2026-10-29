//! Golden H.323 wire vectors for the native TPKT/Q.931 boundary.
//!
//! The Polycom frames are independent captures; the H.235/H.245-security
//! frames are deterministic pycrate encodings. See `tests/golden/h323/README.md`
//! for provenance. ASN.1 bodies stay opaque here on purpose: the sidecar owns
//! them today, so the native test must not reinterpret their bytes as media.

use vks_gateway_h323::{encode_tpkt, parse_q931_message, MSG_ALERTING, MSG_CONNECT, MSG_RELEASE_COMPLETE, MSG_SETUP};

const ALERTING: &[u8] = include_bytes!("golden/h323/polycom_alerting.tpkt");
const CONNECT: &[u8] = include_bytes!("golden/h323/polycom_connect.tpkt");
const RELEASE_BUSY: &[u8] = include_bytes!("golden/h323/polycom_release_adaptive_busy.tpkt");
const SETUP_CLEAR: &[u8] = include_bytes!("golden/h323/h225_setup_cleartext.tpkt");
const SETUP_TOKEN: &[u8] = include_bytes!("golden/h323/h225_setup_h235_token.tpkt");
const SETUP_TLS: &[u8] = include_bytes!("golden/h323/h225_setup_tls_only.tpkt");
const CONNECT_TLS: &[u8] = include_bytes!("golden/h323/h225_connect_tls.tpkt");
const RELEASE_SECURITY: &[u8] = include_bytes!("golden/h323/h225_release_security_denied.tpkt");

const H245_INCOMING_OLC: &[u8] = include_bytes!("golden/h323/h245_incoming_olc.bin");
const H245_OUTGOING_ACK: &[u8] = include_bytes!("golden/h323/h245_outgoing_ack.bin");
const H245_INCOMING_VIDEO_OLC: &[u8] = include_bytes!("golden/h323/h245_incoming_video_olc.bin");
const H245_OUTGOING_VIDEO_ACK: &[u8] = include_bytes!("golden/h323/h245_outgoing_video_ack.bin");
const H245_ENCRYPTED_OLC: &[u8] = include_bytes!("golden/h323/h245_open_encryption_sync.bin");

const H225_FRAMES: [&[u8]; 8] =
    [ALERTING, CONNECT, RELEASE_BUSY, SETUP_CLEAR, SETUP_TOKEN, SETUP_TLS, CONNECT_TLS, RELEASE_SECURITY];

#[test]
fn captured_q931_frames_keep_message_type_reference_and_opaque_h225() {
    for (frame, message_type, reference) in [
        (ALERTING, MSG_ALERTING, 0x3157u16),
        (CONNECT, MSG_CONNECT, 0x3157),
        (RELEASE_BUSY, MSG_RELEASE_COMPLETE, 0x3157),
        (SETUP_CLEAR, MSG_SETUP, 0x1234),
        (SETUP_TOKEN, MSG_SETUP, 0x1234),
        (SETUP_TLS, MSG_SETUP, 0x1234),
        (CONNECT_TLS, MSG_CONNECT, 0x1234),
        (RELEASE_SECURITY, MSG_RELEASE_COMPLETE, 0x1234),
    ] {
        let parsed = parse_q931_message(frame).expect("golden frame must parse");
        assert_eq!(parsed.message_type, message_type, "unexpected Q.931 message type");
        assert_eq!(parsed.call_reference, reference);
        assert!(parsed.h225_user_information.is_some(), "User-User IE must be retained");
        assert_eq!(parsed.remote_media_ip, None, "framing must not invent an RTP address");
        assert_eq!(parsed.remote_media_port, None);
        // The native boundary is byte-preserving: re-framing the original
        // payload must reproduce the capture exactly.
        let payload = &frame[4..];
        assert_eq!(encode_tpkt(payload), frame);
    }
}

#[test]
fn security_denied_release_carries_the_q931_cause() {
    let parsed = parse_q931_message(RELEASE_SECURITY).expect("golden release");
    assert_eq!(parsed.cause, Some(16));
    let busy = parse_q931_message(RELEASE_BUSY).expect("golden release");
    assert_eq!(busy.cause, None, "H.225 release reason is not a Q.931 cause IE");
}

#[test]
fn every_prefix_and_single_byte_mutation_is_panic_free() {
    for frame in H225_FRAMES {
        for length in 0..frame.len() {
            let prefix = &frame[..length];
            let result = std::panic::catch_unwind(|| parse_q931_message(prefix));
            assert!(result.is_ok(), "prefix {length} of {}-byte frame panicked", frame.len());
            assert!(result.unwrap().is_err(), "prefix {length} must be rejected");
        }
        for index in 0..frame.len() {
            for bit in 0..8 {
                let mut mutated = frame.to_vec();
                mutated[index] ^= 1 << bit;
                let result = std::panic::catch_unwind(|| parse_q931_message(&mutated));
                assert!(result.is_ok(), "mutation at byte {index} bit {bit} panicked");
            }
        }
    }
}

#[test]
fn malformed_or_duplicate_user_user_ies_are_rejected() {
    // Wrong UUIE protocol discriminator.
    let mut wrong_protocol = SETUP_CLEAR.to_vec();
    let uuie = wrong_protocol.iter().position(|byte| *byte == 0x7e).expect("User-User IE");
    wrong_protocol[uuie + 3] = 4;
    assert!(parse_q931_message(&wrong_protocol).is_err());

    // A second User-User IE is ambiguous and must not be merged.
    let mut duplicate = SETUP_CLEAR.to_vec();
    let uuie_start = SETUP_CLEAR.iter().position(|byte| *byte == 0x7e).expect("User-User IE");
    let second = &SETUP_CLEAR[uuie_start..];
    duplicate.extend_from_slice(second);
    let old_length = u16::from_be_bytes([duplicate[2], duplicate[3]]) as usize;
    duplicate[2..4].copy_from_slice(&((old_length + second.len()) as u16).to_be_bytes());
    assert!(parse_q931_message(&duplicate).is_err());

    // Coalesced frames and a trailing byte are not "one complete frame".
    let mut coalesced = CONNECT.to_vec();
    coalesced.extend_from_slice(CONNECT);
    assert!(parse_q931_message(&coalesced).is_err());
    let mut trailing = ALERTING.to_vec();
    trailing.push(0);
    assert!(parse_q931_message(&trailing).is_err());
}

#[test]
fn h245_golden_frames_keep_their_captured_shape() {
    // Request/response CHOICE tags and channel numbers, without decoding the
    // ASN.1 body natively (the sidecar remains the decoder in this stage).
    assert_eq!(H245_INCOMING_OLC[0], 0x03);
    assert_eq!(H245_INCOMING_OLC[1..3], [0x00, 0x00]);
    assert_eq!(H245_OUTGOING_ACK[0], 0x22);
    assert_eq!(H245_INCOMING_VIDEO_OLC[0], 0x03);
    assert_eq!(H245_OUTGOING_VIDEO_ACK[0], 0x22);
    assert_eq!(H245_ENCRYPTED_OLC[0], 0x03);
    assert!(
        H245_ENCRYPTED_OLC.windows(3).any(|window| window == b"key"),
        "the security fixture must carry an H.235 key to exercise fail-closed rejection"
    );
}

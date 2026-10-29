"""Golden-vector parity for the pycrate H.323 sidecar.

Reads the pinned files under ``tests/golden/h323``; no network and no live
terminal. The Polycom files are independent captures, the security files are
deterministic pycrate encodings (see the README next to the vectors).
"""
import unittest
from pathlib import Path

import h323_listener as listener
import h323_sidecar as sidecar
import h323_audio as audio

ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "tests" / "golden" / "h323"
ASN = listener.asn


def frame(name):
    return (GOLDEN / name).read_bytes()


class GoldenVectorTests(unittest.TestCase):
    def test_every_h225_vector_decodes_with_expected_identity(self):
        expected = {
            "polycom_alerting.tpkt": ("alerting", 1, 0x3157),
            "polycom_connect.tpkt": ("connect", 7, 0x3157),
            "polycom_release_adaptive_busy.tpkt": ("releaseComplete", 90, 0x3157),
            "h225_setup_cleartext.tpkt": ("setup", 5, 0x1234),
            "h225_setup_h235_token.tpkt": ("setup", 5, 0x1234),
            "h225_setup_tls_only.tpkt": ("setup", 5, 0x1234),
            "h225_connect_tls.tpkt": ("connect", 7, 0x1234),
            "h225_release_security_denied.tpkt": ("releaseComplete", 90, 0x1234),
        }
        for name, (body_name, message, reference) in expected.items():
            with self.subTest(name=name):
                data = frame(name)
                self.assertEqual(data[:2], b"\x03\x00")
                self.assertEqual(int.from_bytes(data[2:4], "big"), len(data))
                parsed_ref, _raw_ref, parsed_message, info, _cause, _called = listener.parse_q931(data[4:])
                self.assertEqual((parsed_ref, parsed_message), (reference, message))
                name_on_wire, params = info["h323-uu-pdu"]["h323-message-body"]
                self.assertEqual(name_on_wire, body_name)
                self.assertNotIn("fastStart", params, "current sidecar does not implement FastStart")

    def test_h235_security_vectors_are_fail_closed(self):
        self.assertFalse(audio.h225_security_requested(
            listener.parse_q931(frame("h225_setup_cleartext.tpkt")[4:])[3]["h323-uu-pdu"]["h323-message-body"][1]
        ))
        for name in ("h225_setup_h235_token.tpkt", "h225_setup_tls_only.tpkt"):
            with self.subTest(name=name):
                info = listener.parse_q931(frame(name)[4:])[3]
                self.assertTrue(audio.h225_security_requested(info["h323-uu-pdu"]["h323-message-body"][1]))
        connect = listener.parse_q931(frame("h225_connect_tls.tpkt")[4:])[3]
        self.assertTrue(audio.h225_security_requested(connect["h323-uu-pdu"]["h323-message-body"][1]))

    def test_security_release_has_the_exact_token(self):
        info = listener.parse_q931(frame("h225_release_security_denied.tpkt")[4:])[3]
        self.assertEqual(audio.release_reason_token(info, 16), "remote_release:securityDenied:cause=16")

    def test_h245_olc_requests_roundtrip_and_encryption_is_detected(self):
        for name in ("h245_incoming_olc.bin", "h245_incoming_video_olc.bin"):
            with self.subTest(name=name):
                data = frame(name)
                decoded = listener.decode(ASN.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, data)
                self.assertEqual(decoded[0], "request")
                self.assertEqual(decoded[1][0], "openLogicalChannel")
                self.assertEqual(
                    listener.encode(ASN.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, decoded),
                    data,
                )
        encrypted = frame("h245_open_encryption_sync.bin")
        decoded = listener.decode(ASN.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, encrypted)
        self.assertTrue(audio.h245_encryption_requested(decoded[1][1]))
        self.assertEqual(decoded[1][1]["encryptionSync"]["synchFlag"], 96)

    def test_h245_acks_keep_directional_media_addresses(self):
        # pycrate canonicalizes one length octet on re-encode (0x06 -> 0x08 in
        # the captured ACKs), so assert decoded semantics rather than bytes.
        for name, session, audio_port, control_port in (
            ("h245_outgoing_ack.bin", 1, 3230, 3231),
            ("h245_outgoing_video_ack.bin", 2, 3232, 3233),
        ):
            with self.subTest(name=name):
                decoded = listener.decode(
                    ASN.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, frame(name)
                )
                self.assertEqual(decoded[0], "response")
                self.assertEqual(decoded[1][0], "openLogicalChannelAck")
                params = decoded[1][1]["forwardMultiplexAckParameters"][1]
                self.assertEqual(params["sessionID"], session)
                media = params["mediaChannel"][1][1]
                control = params["mediaControlChannel"][1][1]
                self.assertEqual(int.from_bytes(media["network"], "big"), 0xC0A80184)
                self.assertEqual(media["tsapIdentifier"], audio_port)
                self.assertEqual(control["tsapIdentifier"], control_port)

    def test_sidecar_uii_uses_the_same_ia5_symbols(self):
        class FakeSocket:
            def __init__(self):
                self.sent = []

            def sendall(self, value):
                self.sent.append(value)

        call = sidecar.Call(sidecar.Configuration.load({
            "target_ip": "192.168.1.132", "target_port": 1720,
            "local_ip": "192.168.1.93", "audio_port": 41002,
            "video_port": None, "timeout_seconds": 30,
            "call_id": "00010203-0405-0607-0809-0a0b0c0d0e0f", "call_ref": 0x3157,
        }))
        call.control = FakeSocket()
        call.send_dtmf(15)
        decoded = listener.decode(
            ASN.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:]
        )
        self.assertEqual(decoded, ("indication", ("userInput", ("signal", {"signalType": "D"}))))


if __name__ == "__main__":
    unittest.main()

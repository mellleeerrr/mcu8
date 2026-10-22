"""Contract tests for the inbound H.323 UAS glue (no network, no real calls)."""
import unittest

import ipaddress
import h323_listener as listener
from h323_listener import asn


def q931_setup(alias: str, called_number: str = "123456") -> bytes:
    body = listener.encode(asn.H323_MESSAGES.H323_UserInformation, {
        "h323-uu-pdu": {
            "h323-message-body": ("setup", {
                "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
                "sourceInfo": {"terminal": {}, "mc": False, "undefinedNode": False},
                "sourceAddress": [("h323-ID", "Polycom")],
                "destinationAddress": [("h323-ID", alias)],
                "activeMC": False, "conferenceID": bytes(range(16)),
                "conferenceGoal": ("create", 0), "callType": ("pointToPoint", 0),
                "callIdentifier": {"guid": bytes(range(16))},
                "mediaWaitForConnect": False, "canOverlapSend": False,
                "multipleCalls": False, "maintainConnection": False,
            }),
            "h245Tunneling": False,
        }
    })
    called = b"\x70" + bytes([len(called_number) + 1]) + b"\x80" + called_number.encode()
    return b"\x08\x02\x12\x34\x05\x04\x03\x88\x90\xa5" + called + b"\x7e" + (len(body) + 1).to_bytes(2, "big") + b"\x05" + body


class InboundContractTests(unittest.TestCase):
    def test_setup_keeps_raw_call_reference_and_full_h323_alias(self):
        ref, raw, message, information, cause, called = listener.parse_q931(q931_setup("4d313d01-8300-450c-ab08-28c8a00f9f25"))
        self.assertEqual((ref, raw, message, cause), (0x1234, b"\x12\x34", 5, None))
        name, setup = information["h323-uu-pdu"]["h323-message-body"]
        self.assertEqual(name, "setup")
        self.assertEqual(listener.setup_alias(setup), "4d313d01-8300-450c-ab08-28c8a00f9f25")
        self.assertEqual(called, "123456")

    def test_h225_alias_wins_over_truncated_q931_number(self):
        _, _, _, information, _, called = listener.parse_q931(q931_setup("room-uuid", "4d313d01-8300-450c-ab08"))
        setup = information["h323-uu-pdu"]["h323-message-body"][1]
        self.assertEqual(listener.setup_alias(setup) or called, "room-uuid")

    def test_video_channel_close_keeps_audio_call_alive(self):
        call = object.__new__(listener.InboundCall)
        call.incoming_channels = {1: "audio", 2: "video"}
        call.requested_channels = {}
        call.incoming_video = True
        call.remote_video = ("127.0.0.1", 4002)
        call.closed = False
        call.reason = "local_hangup"
        call.acks = []
        call.events = []
        call.send_control = lambda value: call.acks.append(value)
        call.event_sink = lambda event, **fields: call.events.append((event, fields))
        call.on_close({"forwardLogicalChannelNumber": 2})
        self.assertFalse(call.closed)
        self.assertEqual(call.reason, "local_hangup")
        self.assertEqual(call.events[0][0], "channel_closed")
        self.assertEqual(call.events[0][1]["kind"], "video")
        call.on_close({"forwardLogicalChannelNumber": 1})
        self.assertTrue(call.closed)

    def test_capability_set_advertises_g722_and_g711_as_one_audio_slot(self):
        call = object.__new__(listener.InboundCall)
        call.config = {}
        caps = call.capability_set()
        table = {entry["capabilityTableEntryNumber"]: entry["capability"] for entry in caps["capabilityTable"]}
        self.assertEqual(table[1], ("receiveAndTransmitAudioCapability", ("g722-64k", 20)))
        self.assertEqual(table[2], ("receiveAndTransmitAudioCapability", ("g711Ulaw64k", 20)))
        self.assertEqual(table[3], ("receiveAndTransmitAudioCapability", ("g711Alaw64k", 20)))
        self.assertEqual(table[4][1][0], "g7231")
        self.assertEqual(caps["capabilityDescriptors"][0]["simultaneousCapabilities"], [[1, 2, 3, 4]])
        self.assertEqual(listener.select_audio_codec({"G722", "G711U"}, {"G722", "G711U"}), "G722")
        self.assertIsNone(listener.select_audio_codec({"G722"}, {"G711U"}))

    def test_incoming_g723_with_multi_frame_sdu_is_rejected(self):
        call = object.__new__(listener.InboundCall)
        call.config = {"local_ip": "127.0.0.1", "audio_port": 41002}
        call.audio_codec = "G723"
        call.incoming_audio = False
        call.rejected = []
        call.reject_open = lambda number, cause: call.rejected.append(number)
        params = {
            "forwardLogicalChannelNumber": 7,
            "forwardLogicalChannelParameters": {
                "dataType": ("audioData", ("g7231", {"maxAl-sduAudioFrames": 2, "silenceSuppression": False})),
                "multiplexParameters": ("h2250LogicalChannelParameters", {"sessionID": 1}),
            },
        }
        call.on_open(params)
        self.assertEqual(call.rejected, [7])

    def test_duplicate_incoming_audio_olc_is_rejected(self):
        call = object.__new__(listener.InboundCall)
        call.config = {"local_ip": "127.0.0.1", "audio_port": 41002}
        call.audio_codec = "G722"
        call.incoming_audio = True
        call.rejected = []
        call.reject_open = lambda number, cause: call.rejected.append(number)
        params = {
            "forwardLogicalChannelNumber": 1,
            "forwardLogicalChannelParameters": {
                "dataType": ("audioData", ("g722-64k", 20)),
                "multiplexParameters": ("h2250LogicalChannelParameters", {"sessionID": 1}),
            },
        }
        call.on_open(params)
        self.assertEqual(call.rejected, [1])

    def test_audio_reject_opens_next_candidate_on_channel_three(self):
        call = object.__new__(listener.InboundCall)
        call.wire = lambda *args, **kwargs: None
        call.request_window = 0.0
        call.received_requests = 0
        call.requested_channels = {1: "audio"}
        call.audio_channel = 1
        call.requested_audio_codec = {1: "G722"}
        call.remote_audio = None
        call.incoming_audio = False
        call.audio_candidates = ["G722", "G711U"]
        call.remote_audio_rx = {"G722", "G711U"}
        call.remote_audio_tx = {"G722", "G711U"}
        call.audio_fallback_channel = 3
        call.audio_codec = "G722"
        call.config = {"audio_port": 41002}
        call.opened = []
        call.open_outgoing = lambda number, kind, port: call.opened.append((number, kind))
        packet = listener.encode(
            listener.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage,
            ("response", ("openLogicalChannelReject", {"forwardLogicalChannelNumber": 1, "cause": ("dataTypeNotSupported", 0)})),
        )
        call.on_control(packet)
        self.assertEqual(call.opened, [(3, "audio")])
        self.assertEqual(call.audio_codec, "G711U")

    def test_incoming_g722_olc_is_acked_with_pt9_and_local_ports(self):
        call = object.__new__(listener.InboundCall)
        call.config = {"local_ip": "192.168.1.93", "audio_port": 41002}
        call.audio_codec = "G722"
        call.incoming_channels = {}
        call.local_audio_pt = 0
        call.incoming_audio = False
        call.acks = []
        call.send_control = lambda value: call.acks.append(value)
        params = {
            "forwardLogicalChannelNumber": 1,
            "forwardLogicalChannelParameters": {
                "dataType": ("audioData", ("g722-64k", 20)),
                "multiplexParameters": ("h2250LogicalChannelParameters", {"sessionID": 1}),
            },
        }
        call.on_open(params)
        self.assertTrue(call.incoming_audio)
        self.assertEqual(call.local_audio_pt, 9)
        ack = call.acks[-1][1][1]["forwardMultiplexAckParameters"][1]
        self.assertNotIn("dynamicRTPPayloadType", ack)
        self.assertEqual(ack["mediaChannel"], listener.h245_address("192.168.1.93", 41002))

    def test_release_complete_is_a_valid_q931_frame(self):
        class FakeConn:
            def __init__(self):
                self.sent = b""

            def sendall(self, value):
                self.sent += value

        conn = FakeConn()
        listener.release_complete(conn, b"\x12\x34", bytes(range(16)), 21)
        self.assertTrue(conn.sent.startswith(b"\x03\x00"))
        self.assertEqual(conn.sent[4:5], b"\x08")
        self.assertEqual(conn.sent[8:9], b"\x5a")
        self.assertIn(b"\x08\x02\x80\x15", conn.sent)

    def test_allowlist_filters_peer_networks(self):
        networks = [ipaddress.ip_network("192.168.1.0/24"), ipaddress.ip_network("127.0.0.1/32")]
        self.assertTrue(listener.peer_allowed("192.168.1.132", networks))
        self.assertTrue(listener.peer_allowed("127.0.0.1", networks))
        self.assertFalse(listener.peer_allowed("10.1.2.3", networks))
        self.assertFalse(listener.peer_allowed("not-an-ip", networks))
        self.assertFalse(listener.peer_allowed("203.0.113.9", []))

    def test_h245_control_uses_h225_flat_ip_choice(self):
        self.assertEqual(listener.h245_ip_address("192.168.1.93", 1234)[0], "ipAddress")
        self.assertEqual(listener.h245_ip_address("192.168.1.93", 1234)[1]["port"], 1234)
        self.assertEqual(listener.h245_address("192.168.1.93", 1234)[0], "unicastAddress")


if __name__ == "__main__":
    unittest.main()

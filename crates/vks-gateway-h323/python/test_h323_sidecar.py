"""Regression tests from independent Polycom wire captures, never real calls."""
import contextlib
import io
import json
import unittest
from unittest.mock import patch

import h323_sidecar as h

ALERTING = bytes.fromhex('0802b157017e0067052380060008914a000522c0b500233127506f6c79636f6d205265616c50726573656e6365204465736b746f7020666f722057696e646f77730a332e352e302e353935383201b0d8001100000102030405060708090a0b0c0d0e0f010001800100014010800100')
CONNECT = bytes.fromhex('0802b15707280770726f73746f727e007e0522c0060008914a000500c0a801840cad22c0b500233127506f6c79636f6d205265616c50726573656e6365204465736b746f7020666f722057696e646f77730a332e352e302e353935383200000102030405060708090a0b0c0d0e0f1d0cc01100000102030405060708090a0b0c0d0e0f010001800100014010800100')
INCOMING_OLC = bytes.fromhex('030000000c6027800a04000100c0a801840c9f')
OUTGOING_ACK = bytes.fromhex('22c000000680101c0000c0a801840c9e00c0a801840c9f')
INCOMING_VIDEO_OLC = bytes.fromhex('030000010a001d60000700088171000001400c800402914002a2001d003200180062001a801804600200c0a801840ca16c000b50080008817100000001da')
OUTGOING_VIDEO_ACK = bytes.fromhex('22c000010680101c0200c0a801840ca000c0a801840ca1')


class FakeSocket:
    def __init__(self): self.sent = []
    def sendall(self, value): self.sent.append(value)


class ContractTests(unittest.TestCase):
    def call(self, video=False):
        return h.Call(h.Configuration.load({
            'target_ip': '192.168.1.132', 'target_port': 1720,
            'local_ip': '192.168.1.93', 'audio_port': 41002,
            'video_port': 41004 if video else None, 'timeout_seconds': 30,
            'call_id': '00010203-0405-0607-0809-0a0b0c0d0e0f', 'call_ref': 0x3157,
        }))

    def test_independent_polycom_connect_decodes_true_h245_address(self):
        ref, kind, value, cause = h.parse_q931(CONNECT)
        self.assertEqual((ref, kind, cause), (0x3157, 7, None))
        name, connect = value['h323-uu-pdu']['h323-message-body']
        self.assertEqual(name, 'connect')
        self.assertEqual(connect['h245Address'], ('ipAddress', {'ip': b'\xc0\xa8\x01\x84', 'port': 3245}))
        self.assertNotIn('fastStart', connect)
        self.assertFalse(value['h323-uu-pdu']['h245Tunneling'])

    def test_alerting_is_not_connect_and_contains_no_media(self):
        _, message, value, _ = h.parse_q931(ALERTING)
        self.assertEqual(message, 1)
        self.assertEqual(value['h323-uu-pdu']['h323-message-body'][0], 'alerting')
        call = self.call()
        with contextlib.redirect_stdout(io.StringIO()): call.on_signal(ALERTING)
        self.assertFalse(call.connected)
        self.assertFalse(call.signal_connected)
        self.assertIsNone(call.remote_audio)

    def test_invented_faststart_packet_is_not_decodable_as_h225(self):
        packet = bytes.fromhex('08020001077e000b05a0080100c0a80184138c')
        with self.assertRaises(Exception): h.parse_q931(packet)

    def test_truncated_user_user_and_duplicate_ie_are_rejected(self):
        with self.assertRaises(h.ProtocolError): h.parse_q931(CONNECT[:-1])
        with self.assertRaises(h.ProtocolError): h.parse_q931(CONNECT + b'\x7e\x00\x01\x05')

    def test_real_polycom_ack_negotiates3230_without_heuristic(self):
        call = self.call()
        call.control = FakeSocket()
        call.requested_channels[1] = 'audio'
        with contextlib.redirect_stdout(io.StringIO()): call.on_control(OUTGOING_ACK)
        self.assertEqual(call.remote_audio, ('192.168.1.132', 3230))
        self.assertFalse(call.connected, 'one outgoing channel does not mean bidirectional connection')

    def test_real_polycom_incoming_olc_ack_uses_our_audio_pair(self):
        call = self.call()
        call.control = FakeSocket()
        with contextlib.redirect_stdout(io.StringIO()): call.on_control(INCOMING_OLC)
        self.assertTrue(call.incoming_audio)
        ack = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        params = ack[1][1]['forwardMultiplexAckParameters'][1]
        self.assertEqual(params['mediaChannel'], h.h245_address('192.168.1.93', 41002))
        self.assertEqual(params['mediaControlChannel'], h.h245_address('192.168.1.93', 41003))
        self.assertFalse(call.connected)

    def test_connect_emits_only_after_both_audio_directions(self):
        call = self.call()
        call.signal_connected = call.tcs_acked = call.master_determined = True
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            call.maybe_connected()
            call.remote_audio = ('192.168.1.132', 3230)
            call.maybe_connected()
            self.assertFalse(call.connected)
            call.incoming_audio = True
            call.maybe_connected()
        events = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]['event'], 'connected')
        self.assertEqual(events[0]['remote_audio_payload_type'], 0)

    def test_requested_h264_waits_for_both_video_channels(self):
        call = self.call(video=True)
        call.signal_connected = call.tcs_acked = call.master_determined = call.incoming_audio = True
        call.remote_audio = ('192.168.1.132', 3230)
        call.remote_h264 = True
        with contextlib.redirect_stdout(io.StringIO()):
            call.maybe_connected()
            self.assertFalse(call.connected)
            call.remote_video = ('192.168.1.132', 3232)
            call.video_tx_pt = 96
            call.maybe_connected()
            self.assertFalse(call.connected)
            call.incoming_video, call.video_rx_pt = True, 109
            call.maybe_connected()
        self.assertTrue(call.connected)

    def test_h264_without_incoming_video_downgrades_after_grace(self):
        call = self.call(video=True)
        call.signal_connected = call.tcs_acked = call.master_determined = call.incoming_audio = True
        call.remote_audio = ('192.168.1.132', 3230)
        call.remote_h264 = True
        call.audio_ready_at = h.time.monotonic() - 3
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            call.maybe_connected()
        self.assertTrue(call.connected)
        self.assertIn('video_unavailable', output.getvalue())

    def test_unsolicited_channel_reject_does_not_downgrade_video(self):
        call = self.call(video=True)
        value = ('response', ('openLogicalChannelReject', {'forwardLogicalChannelNumber': 99, 'cause': ('unspecified', 0)}))
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_control(h.encode(model, value))
        self.assertFalse(call.requested_video_rejected)

    def test_rejected_video_channel_still_connects_audio(self):
        call = self.call(video=True)
        call.control = FakeSocket()
        call.signal_connected = call.tcs_acked = call.master_determined = call.incoming_audio = True
        call.remote_audio = ('192.168.1.132', 3230)
        call.remote_h264 = True
        rejected = {
            'forwardLogicalChannelNumber': 2,
            'forwardLogicalChannelParameters': {
                'dataType': ('videoData', h.video_capability()),
                'multiplexParameters': ('h2250LogicalChannelParameters', {'sessionID': 1}),
            },
        }
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            call.on_open(rejected)
            call.maybe_connected()
        self.assertTrue(call.requested_video_rejected)
        self.assertIn('connected', output.getvalue())

    def test_unsolicited_and_foreign_ack_never_abort_the_call(self):
        call = self.call()
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_ack({'forwardLogicalChannelNumber': 99})
            call.on_ack({'forwardLogicalChannelNumber': 1, 'forwardMultiplexAckParameters': ('h2250LogicalChannelAckParameters', {})})
        self.assertIsNone(call.remote_audio)
        call.requested_channels[1] = 'audio'
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_ack({'forwardLogicalChannelNumber': 1, 'forwardMultiplexAckParameters': ('h2250LogicalChannelAckParameters', {'mediaChannel': h.h245_address('10.0.0.99', 3231)})})
        self.assertEqual(call.remote_audio, ('192.168.1.132', 3231))

    def test_independent_polycom_h264_olc_and_ack_keep_directional_payloads(self):
        # Captured in the real H.323/H.264 call: terminal sends PT109, while
        # our transmitter negotiated PT96. These values are not interchangeable.
        call = self.call(video=True)
        call.control = FakeSocket()
        call.requested_channels[2] = 'video'
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_control(INCOMING_VIDEO_OLC)
            call.on_control(OUTGOING_VIDEO_ACK)
        self.assertEqual(call.video_rx_pt, 109)
        self.assertEqual(call.video_tx_pt, 96)
        self.assertEqual(call.remote_video, ('192.168.1.132', 3232))
        self.assertTrue(call.incoming_video)
        response = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[0][4:])
        media = response[1][1]['forwardMultiplexAckParameters'][1]
        self.assertEqual(media['dynamicRTPPayloadType'], 109)
        self.assertEqual(media['mediaChannel'], h.h245_address('192.168.1.93', 41004))

    def test_asn1_trailing_bytes_are_rejected(self):
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
        with self.assertRaises(h.ProtocolError): h.decode(model, OUTGOING_ACK + b'\x00')

    def test_setup_has_video_bearer_and_valid_per_not_private_marker(self):
        call = self.call(video=True)
        call.signal = FakeSocket()
        call.send_setup()
        packet = call.signal.sent[-1]
        self.assertEqual(packet[9:14], b'\x04\x03\x88\x93\xa5')
        _, message, information, _ = h.parse_q931(packet[4:])
        self.assertEqual(message, 5)
        self.assertEqual(information['h323-uu-pdu']['h323-message-body'][0], 'setup')
        self.assertEqual(information['h323-uu-pdu']['h323-message-body'][1]['callIdentifier']['guid'], bytes(range(16)))

    def test_h264_capability_has_baseline_and_non_interleaved_payload(self):
        call = self.call(video=True)
        value = ('request', ('terminalCapabilitySet', call.capability_set()))
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
        decoded = h.decode(model, h.encode(model, value))
        capabilities = decoded[1][1]
        self.assertEqual(capabilities['capabilityTable'][1]['capability'][1], h.video_capability())
        packetization = capabilities['multiplexCapability'][1]['mediaPacketizationCapability']
        self.assertEqual(packetization['rtpPayloadType'][0]['payloadDescriptor'], ('oid', h.H264_NON_INTERLEAVED))
        self.assertEqual(h.codec_kind(('videoData', h.video_capability())), ('video', None))

    def test_foreign_peer_address_and_overlapping_ports_fail_closed(self):
        with self.assertRaises(h.ProtocolError): h.endpoint(h.h245_address('10.0.0.99', 3230), '192.168.1.132')
        with self.assertRaises(h.ProtocolError):
            h.Configuration.load({'target_ip':'192.168.1.132','local_ip':'192.168.1.93','audio_port':41002,'video_port':41003,'timeout_seconds':30,'call_id':'00010203-0405-0607-0809-0a0b0c0d0e0f','call_ref':1})

    def test_nil_call_id_is_rejected_before_any_tcp_connection(self):
        # Polycom answers an all-zero conferenceID/callIdentifier with Q.931
        # Release Complete invalidCID, so reject it before the socket opens.
        with self.assertRaises(h.ProtocolError):
            h.Configuration.load({'target_ip':'192.168.1.132','local_ip':'192.168.1.93','audio_port':41002,'timeout_seconds':30,'call_id':'00000000-0000-0000-0000-000000000000','call_ref':1})

    def test_fragmented_tpkt_buffers_until_complete(self):
        call = self.call()
        class Reader:
            def __init__(self): self.chunks = [h.tpkt(ALERTING)[:2], h.tpkt(ALERTING)[2:]]
            def recv(self, limit): return self.chunks.pop(0)
        sock = Reader()
        call.signal, call.buffers[sock] = sock, b''
        with contextlib.redirect_stdout(io.StringIO()):
            call.frames(sock)
            self.assertEqual(len(call.buffers[sock]), 2)
            call.frames(sock)
        self.assertEqual(call.buffers[sock], b'')
        self.assertFalse(call.connected)


    def test_invalid_h241_level_is_rejected_before_tcp(self):
        with self.assertRaises(h.ProtocolError):
            h.Configuration.load({
                'target_ip': '192.168.1.132', 'local_ip': '192.168.1.93',
                'audio_port': 41002, 'video_port': 41004, 'timeout_seconds': 30,
                'call_id': '00010203-0405-0607-0809-0a0b0c0d0e0f', 'call_ref': 1,
                'video_h264_level': 75,
            })

    def test_tcs_and_olc_use_the_same_configured_h241_capability(self):
        call = h.Call(h.Configuration.load({
            'target_ip': '192.168.1.132', 'local_ip': '192.168.1.93',
            'audio_port': 41002, 'video_port': 41004, 'timeout_seconds': 30,
            'call_id': '00010203-0405-0607-0809-0a0b0c0d0e0f', 'call_ref': 1,
            'video_max_bitrate_100bps': 40960, 'video_h264_level': 80,
        }))
        tcs = call.capability_set()
        video = next(e['capability'] for e in tcs['capabilityTable']
                     if e['capability'][0] == 'receiveAndTransmitVideoCapability')
        sent = []
        call.send_control = lambda value: sent.append(value)
        call.open_outgoing(2, 'video', 41004)
        olc = sent[0][1][1]['forwardLogicalChannelParameters']['dataType']
        self.assertEqual(video[1][1]['maxBitRate'], olc[1][1]['maxBitRate'])
        self.assertEqual(video[1][1]['collapsing'], olc[1][1]['collapsing'])
        self.assertEqual(olc[1][1]['maxBitRate'], 40960)
        self.assertEqual(olc[1][1]['collapsing'][1]['parameterValue'][1], 80)

    def test_video_capability_uses_live_profile_limits(self):
        config = h.Configuration.load({
            'target_ip': '192.168.1.132', 'local_ip': '192.168.1.93',
            'audio_port': 41002, 'video_port': 41004, 'timeout_seconds': 30,
            'call_id': '00010203-0405-0607-0809-0a0b0c0d0e0f', 'call_ref': 1,
            'video_max_bitrate_100bps': 40960, 'video_h264_level': 80,
        })
        capability = h.video_capability(config)
        self.assertEqual(capability[1]['maxBitRate'], 40960)
        params = {p['parameterIdentifier']: p['parameterValue'][1]
                  for p in capability[1]['collapsing']}
        self.assertEqual(params[('standard', 41)], 64)
        self.assertEqual(params[('standard', 42)], 80)
        # Defaults stay wire-compatible with the first live Polycom capture.
        default = h.video_capability()
        self.assertEqual(default[1]['maxBitRate'], 5120)
        self.assertEqual(default[1]['collapsing'][1]['parameterValue'][1], 71)


if __name__ == '__main__': unittest.main()


class H241PeerParametersTest(unittest.TestCase):
    def test_peer_parameters_are_extracted_and_clamped_by_gateway(self):
        capability = ("genericVideoCapability", {
            "capabilityIdentifier": ("standard", h.H264_OID),
            "maxBitRate": 40960,
            "collapsing": [
                {"parameterIdentifier": ("standard", 41), "parameterValue": ("booleanArray", 64)},
                {"parameterIdentifier": ("standard", 42), "parameterValue": ("unsignedMin", 80)},
            ],
        })
        self.assertEqual(
            h.h241_video_parameters(capability),
            {"max_bitrate_100bps": 40960, "level_idc": 80, "profile_bits": 64},
        )
        self.assertEqual(h.h241_video_parameters(("genericVideoCapability", {
            "capabilityIdentifier": ("standard", ("oid", b"other")), "maxBitRate": 5120})), {})
        self.assertEqual(h.h241_video_parameters(None), {})

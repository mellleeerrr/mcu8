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
        call.audio_ready_at = h.time.monotonic() - 9
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
        table = {entry['capabilityTableEntryNumber']: entry['capability'] for entry in capabilities['capabilityTable']}
        self.assertEqual(table[1], ('receiveAndTransmitAudioCapability', ('g722-64k', 20)))
        self.assertEqual(table[2], ('receiveAndTransmitAudioCapability', ('g711Ulaw64k', 20)))
        self.assertEqual(table[3], ('receiveAndTransmitAudioCapability', ('g711Alaw64k', 20)))
        self.assertEqual(table[4][1][0], 'g7231')
        self.assertEqual(table[5][1], h.video_capability())
        self.assertEqual(capabilities['capabilityDescriptors'][0]['simultaneousCapabilities'], [[1, 2, 3, 4], [5]])
        packetization = capabilities['multiplexCapability'][1]['mediaPacketizationCapability']
        self.assertEqual(packetization['rtpPayloadType'][0]['payloadDescriptor'], ('oid', h.H264_NON_INTERLEAVED))
        self.assertEqual(h.codec_kind(('videoData', h.video_capability())), ('video', None))

    def test_symmetric_audio_selection_prefers_g722_and_rejects_asymmetry(self):
        self.assertEqual(h.select_audio_codec({'G722', 'G711U'}, {'G722', 'G711U'}), 'G722')
        self.assertEqual(h.select_audio_codec({'G711U'}, {'G711U', 'G722'}), 'G711U')
        self.assertEqual(h.select_audio_codec({'G723', 'G711U'}, {'G723', 'G711U'}), 'G711U')
        self.assertEqual(h.select_audio_codec({'G723', 'G711A'}, {'G723', 'G711A'}), 'G711A')
        self.assertEqual(h.select_audio_codec({'G711A'}, {'G711A'}), 'G711A')
        self.assertIsNone(h.select_audio_codec({'G722'}, {'G711U'}))
        self.assertIsNone(h.select_audio_codec(set(), set()))

    def test_native_audio_data_types_use_static_payloads(self):
        cases = {
            'G722': ('g722-64k', 9),
            'G723': ('g7231', 4),
            'G711U': ('g711Ulaw64k', 0),
            'G711A': ('g711Alaw64k', 8),
        }
        for codec, (key, pt) in cases.items():
            data_type = h.audio_data_type(codec)
            self.assertEqual(data_type[0], 'audioData')
            self.assertEqual(data_type[1][0], key)
            self.assertEqual(h.audio_payload_type(codec), pt)
            self.assertEqual(h.codec_kind(data_type), ('audio', pt))

    def test_peer_g723_with_multi_frame_sdu_is_not_selected(self):
        call = self.call()
        call.control = FakeSocket()
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage

        def feed(g723_max):
            caps = call.capability_set()
            caps['capabilityTable'] = [
                {'capabilityTableEntryNumber': 1, 'capability': ('receiveAndTransmitAudioCapability', ('g7231', {'maxAl-sduAudioFrames': g723_max, 'silenceSuppression': False}))},
                {'capabilityTableEntryNumber': 2, 'capability': ('receiveAndTransmitAudioCapability', ('g711Ulaw64k', 20))},
            ]
            caps['capabilityDescriptors'][0]['simultaneousCapabilities'] = [[1, 2]]
            with contextlib.redirect_stdout(io.StringIO()):
                call.on_control(h.encode(model, ('request', ('terminalCapabilitySet', caps))))

        feed(2)
        self.assertEqual(call.audio_candidates, ['G711U'])
        feed(1)
        self.assertEqual(call.audio_candidates, ['G711U', 'G723'])

    def test_tcs_descriptors_limit_usable_capability_entries(self):
        call = self.call()
        call.control = FakeSocket()
        caps = call.capability_set()
        caps['capabilityTable'] = [
            {'capabilityTableEntryNumber': 1, 'capability': ('receiveAndTransmitAudioCapability', ('g722-64k', 20))},
            {'capabilityTableEntryNumber': 2, 'capability': ('receiveAndTransmitAudioCapability', ('g711Alaw64k', 20))},
        ]
        caps['capabilityDescriptors'][0]['simultaneousCapabilities'] = [[1]]
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_control(h.encode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, ('request', ('terminalCapabilitySet', caps))))
        self.assertEqual(call.audio_candidates, ['G722'])

    def test_receive_only_peer_is_explicitly_rejected(self):
        call = self.call()
        call.control = FakeSocket()
        caps = call.capability_set()
        caps['capabilityTable'] = [
            {'capabilityTableEntryNumber': 1, 'capability': ('receiveAudioCapability', ('g722-64k', 20))},
        ]
        caps['capabilityDescriptors'][0]['simultaneousCapabilities'] = [[1]]
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(h.ProtocolError, 'two-way'):
            call.on_control(h.encode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, ('request', ('terminalCapabilitySet', caps))))

    def test_peer_selected_incoming_pt_adopts_codec_and_reopens_outgoing(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        call.audio_fallback_channel = 3
        call.remote_audio = None
        call.incoming_audio = False
        params = {
            'forwardLogicalChannelNumber': 1,
            'forwardLogicalChannelParameters': {
                'dataType': ('audioData', ('g711Alaw64k', 20)),
                'multiplexParameters': ('h2250LogicalChannelParameters', {'sessionID': 1}),
            },
        }
        with contextlib.redirect_stdout(io.StringIO()): call.on_open(params)
        self.assertEqual(call.audio_codec, 'G711A')
        self.assertEqual(call.audio_rx_pt, 8)
        self.assertTrue(call.incoming_audio)
        opened = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-2][4:])
        self.assertEqual(opened[1][1]['forwardLogicalChannelNumber'], 3)
        self.assertEqual(opened[1][1]['forwardLogicalChannelParameters']['dataType'][1][0], 'g711Alaw64k')

    def test_duplicate_incoming_audio_olc_is_rejected(self):
        call = self.call()
        call.control = FakeSocket()
        call.incoming_audio = True
        params = {
            'forwardLogicalChannelNumber': 1,
            'forwardLogicalChannelParameters': {
                'dataType': ('audioData', ('g722-64k', 20)),
                'multiplexParameters': ('h2250LogicalChannelParameters', {'sessionID': 1}),
            },
        }
        with contextlib.redirect_stdout(io.StringIO()): call.on_open(params)
        reject = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        self.assertEqual(reject[1][0], 'openLogicalChannelReject')

    def test_audio_fallback_walks_down_the_native_preference_chain(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        call.audio_candidates = ['G722', 'G711U', 'G723']
        call.remote_audio_rx = {'G722', 'G711U', 'G723'}
        call.remote_audio_tx = {'G722', 'G711U', 'G723'}
        call.requested_channels[1] = 'audio'
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
        for channel, expected in [(1, 'G711U'), (3, 'G723')]:
            reject = {'forwardLogicalChannelNumber': channel, 'cause': ('dataTypeNotSupported', 0)}
            with contextlib.redirect_stdout(io.StringIO()):
                call.on_control(h.encode(model, ('response', ('openLogicalChannelReject', reject))))
            self.assertEqual(call.audio_codec, expected)
            opened = h.decode(model, call.control.sent[-1][4:])[1][1]
            self.assertEqual(opened['forwardLogicalChannelParameters']['dataType'][1][0], h.audio_data_type(expected)[1][0])

    def test_g722_outgoing_olc_uses_static_pt9_and_ack_keeps_it_symmetric(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        call.open_outgoing(1, 'audio', 41002)
        request = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        fw = request[1][1]['forwardLogicalChannelParameters']
        self.assertEqual(fw['dataType'], ('audioData', ('g722-64k', 20)))
        params = fw['multiplexParameters'][1]
        self.assertNotIn('dynamicRTPPayloadType', params)
        self.assertIs(params['silenceSuppression'], False)
        call.requested_channels[1] = 'audio'
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_control(OUTGOING_ACK)
        self.assertEqual(call.audio_tx_pt, 9)
        self.assertEqual(call.remote_audio, ('192.168.1.132', 3230))

    def test_g722_reject_retries_g711_on_fresh_channel(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        call.remote_audio_rx = {'G722', 'G711U'}
        call.remote_audio_tx = {'G722', 'G711U'}
        call.requested_channels[1] = 'audio'
        reject = {'forwardLogicalChannelNumber': 1, 'cause': ('dataTypeNotSupported', 0)}
        model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
        packet = h.encode(model, ('response', ('openLogicalChannelReject', reject)))
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_control(packet)
        self.assertEqual(call.audio_codec, 'G711U')
        self.assertIn(3, call.requested_channels)
        request = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        data_type = request[1][1]['forwardLogicalChannelParameters']['dataType']
        self.assertEqual(data_type, ('audioData', ('g711Ulaw64k', 20)))
        self.assertEqual(call.audio_channel, 3)

    def test_fallback_requires_g711_in_both_peer_directions(self):
        for rx, tx, expect_error in [
            ({'G722', 'G711U'}, {'G722', 'G711U'}, False),
            ({'G722'}, {'G722', 'G711U'}, True),
            ({'G722', 'G711U'}, {'G722'}, True),
        ]:
            call = self.call()
            call.control = FakeSocket()
            call.audio_codec = 'G722'
            call.remote_audio_rx, call.remote_audio_tx = rx, tx
            call.requested_channels[1] = 'audio'
            model = h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage
            reject = {'forwardLogicalChannelNumber': 1, 'cause': ('dataTypeNotSupported', 0)}
            packet = h.encode(model, ('response', ('openLogicalChannelReject', reject)))
            with contextlib.redirect_stdout(io.StringIO()):
                if expect_error:
                    with self.assertRaises(h.ProtocolError):
                        call.on_control(packet)
                else:
                    call.on_control(packet)
                    self.assertEqual(call.audio_codec, 'G711U')

    def test_incoming_audio_olc_with_dynamic_pt_is_rejected(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        params = {
            'forwardLogicalChannelNumber': 1,
            'forwardLogicalChannelParameters': {
                'dataType': ('audioData', ('g722-64k', 20)),
                'multiplexParameters': ('h2250LogicalChannelParameters', {'sessionID': 1, 'dynamicRTPPayloadType': 96}),
            },
        }
        with contextlib.redirect_stdout(io.StringIO()):
            call.on_open(params)
        self.assertFalse(call.incoming_audio)
        reject = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        self.assertEqual(reject[1][0], 'openLogicalChannelReject')

    def test_g722_incoming_olc_is_acked_without_dynamic_pt(self):
        call = self.call()
        call.control = FakeSocket()
        call.audio_codec = 'G722'
        params = {
            'forwardLogicalChannelNumber': 1,
            'forwardLogicalChannelParameters': {
                'dataType': ('audioData', ('g722-64k', 20)),
                'multiplexParameters': ('h2250LogicalChannelParameters', {'sessionID': 1}),
            },
        }
        with contextlib.redirect_stdout(io.StringIO()): call.on_open(params)
        self.assertTrue(call.incoming_audio)
        self.assertEqual(call.audio_rx_pt, 9)
        ack = h.decode(h.asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, call.control.sent[-1][4:])
        ack_params = ack[1][1]['forwardMultiplexAckParameters'][1]
        self.assertNotIn('dynamicRTPPayloadType', ack_params)
        self.assertEqual(ack_params['mediaChannel'], h.h245_address('192.168.1.93', 41002))

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

    def test_non_baseline_h264_profile_is_rejected_before_tcp(self):
        with self.assertRaises(h.ProtocolError):
            h.Configuration.load({
                'target_ip': '192.168.1.132', 'local_ip': '192.168.1.93',
                'audio_port': 41002, 'video_port': 41004, 'timeout_seconds': 30,
                'call_id': '00010203-0405-0607-0809-0a0b0c0d0e0f', 'call_ref': 1,
                'video_h264_profile_bits': 128,
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

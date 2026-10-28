#!/usr/bin/env python3
"""H.323 inbound UAS: one listener parent, forked child per call.

The listener accepts Q.931/H.225 Setup, asks the Rust side for a room and RTP
ports, then performs H.245 TCS/MSD/OLC over a separate TCP channel. Signaling
state stays in Python/Pycrate; media samples always stay in the Rust bridge.

stdin: JSON configuration line, then commands:
  {"command":"accept","call_id":...,"local_ip":...,"audio_port":...,"video_port":...}
  {"command":"reject","call_id":...}
  {"command":"hangup","call_id":...}
  {"command":"shutdown"}
stdout: one JSON event per line (see README).
"""
from __future__ import annotations

import collections
import contextlib
import ctypes
import ipaddress
import json
import os
import select
import signal
import socket
import sys
import time
import uuid

from h323_audio import (
    AUDIO_CODECS,
    audio_capability_entries,
    audio_codec_for_payload,
    audio_data_type,
    audio_capabilities_from_tcs,
    can_adopt_audio_codec,
    audio_payload_type,
    next_audio_candidate,
    peer_transmits_g723,
)

MAX_FRAME = 65535
MAX_COMMAND = 4096
H264_OID = (0, 0, 8, 241, 0, 0, 1)
H264_NON_INTERLEAVED = (0, 0, 8, 241, 0, 0, 0, 1)

try:
    with contextlib.redirect_stdout(sys.stderr):
        import h323_asn as asn
        import h323_audio as audio
        from pycrate_core.charpy import Charpy
except ImportError as dependency_error:  # pragma: no cover - startup contract
    sys.stdout.write(json.dumps({"event": "failed", "reason": "ASN.1 runtime unavailable"}) + "\n")
    sys.stdout.flush()
    raise SystemExit(1) from dependency_error


class ProtocolError(Exception):
    pass


def emit(event: str, **fields):
    # Parent and forked children share stdout. One write per event keeps JSON
    # lines atomic on a pipe (well below PIPE_BUF).
    data = {"event": event, **fields}
    line = json.dumps(data, ensure_ascii=True, separators=(",", ":")) + "\n"
    if len(line) > 60_000:
        line = json.dumps({"event": "failed", "reason": "event_too_large"}) + "\n"
    try:
        os.write(1, line.encode("utf-8"))
    except OSError:
        pass


def encode(model, value) -> bytes:
    with contextlib.redirect_stdout(sys.stderr):
        model.set_val(value)
        return model.to_aper()


def decode(model, data: bytes):
    with contextlib.redirect_stdout(sys.stderr):
        buffer = Charpy(data)
        model.from_aper(buffer)
        if buffer.len_bit() != 0:
            raise ProtocolError("Trailing octets after ASN.1 PER value")
        return model.get_val()


def tpkt(payload: bytes) -> bytes:
    if not 1 <= len(payload) <= MAX_FRAME - 4:
        raise ProtocolError("TPKT payload length outside bound")
    return b"\x03\x00" + (len(payload) + 4).to_bytes(2, "big") + payload


def parse_q931(payload: bytes):
    if len(payload) < 4 or payload[0] != 8 or payload[1] not in (1, 2):
        raise ProtocolError("Invalid Q.931 header")
    reference_length = payload[1]
    if len(payload) < reference_length + 3:
        raise ProtocolError("Truncated Q.931 call reference")
    raw_reference = payload[2:2 + reference_length]
    reference = int.from_bytes(raw_reference, "big") & (0x7F if reference_length == 1 else 0x7FFF)
    message = payload[reference_length + 2]
    position = reference_length + 3
    user_information, cause, called = None, None, None
    while position < len(payload):
        tag = payload[position]
        position += 1
        if tag & 0x80:
            continue
        length_octets = 2 if tag == 0x7E else 1
        if position + length_octets > len(payload):
            raise ProtocolError("Truncated Q.931 IE length")
        length = int.from_bytes(payload[position:position + length_octets], "big")
        position += length_octets
        if position + length > len(payload):
            raise ProtocolError("Truncated Q.931 IE payload")
        data = payload[position:position + length]
        position += length
        if tag == 0x7E:
            if user_information is not None or not data or data[0] != 5:
                raise ProtocolError("Invalid or duplicate H.225 User-User IE")
            user_information = decode(asn.H323_MESSAGES.H323_UserInformation, data[1:])
        elif tag == 8 and len(data) >= 2:
            cause = data[-1] & 0x7F
        elif tag == 0x70 and data:
            called = data[1:] if data[0] & 0x80 else data
            called = called.decode("ascii", "ignore")
    return reference, raw_reference, message, user_information, cause, called


def h245_address(ip: str, port: int):
    return ("unicastAddress", ("iPAddress", {"network": ipaddress.IPv4Address(ip).packed, "tsapIdentifier": port}))


def h245_ip_address(ip: str, port: int):
    # H.225 h245Address is a TransportAddress CHOICE whose IP variant uses
    # flat ip/port fields, unlike H.245 mediaChannel's unicastAddress.
    return ("ipAddress", {"ip": ipaddress.IPv4Address(ip).packed, "port": port})


def video_capability(config):
    return ("genericVideoCapability", {
        "capabilityIdentifier": ("standard", H264_OID),
        "maxBitRate": config["max_bitrate_100bps"],
        "collapsing": [
            {"parameterIdentifier": ("standard", 41), "parameterValue": ("booleanArray", 64)},
            {"parameterIdentifier": ("standard", 42), "parameterValue": ("unsignedMin", config["level_idc"])},
        ],
    })


def h241_video_parameters(capability):
    if not capability or capability[0] != "genericVideoCapability":
        return {}
    body = capability[1]
    if body.get("capabilityIdentifier") != ("standard", H264_OID):
        return {}
    result = {}
    max_bitrate = body.get("maxBitRate")
    if isinstance(max_bitrate, int) and 0 < max_bitrate <= 192400:
        result["max_bitrate_100bps"] = max_bitrate
    for parameter in body.get("collapsing", []):
        identifier = parameter.get("parameterIdentifier")
        value = parameter.get("parameterValue")
        if identifier == ("standard", 42) and value and value[0] == "unsignedMin":
            result["level_idc"] = int(value[1])
        elif identifier == ("standard", 41) and value and value[0] == "booleanArray":
            result["profile_bits"] = int(value[1])
    return result


def codec_kind(data_type):
    kind, codec = data_type
    if kind == "audioData" and codec[0] in AUDIO_CODECS:
        return "audio", AUDIO_CODECS[codec[0]][1]
    if kind == "videoData" and codec[0] == "genericVideoCapability":
        if codec[1].get("capabilityIdentifier") == ("standard", H264_OID):
            return "video", None
    return None, None


def endpoint(address, expected_ip):
    if not address or address[0] != "unicastAddress" or address[1][0] != "iPAddress":
        raise ProtocolError("Only H.245 unicast IPv4 media is supported")
    value = address[1][1]
    ip = str(ipaddress.IPv4Address(value["network"]))
    port = value["tsapIdentifier"]
    if ip != expected_ip or not 1 <= port <= 65535:
        raise ProtocolError("Peer advertised an invalid or foreign media address")
    return ip, port


def setup_alias(params):
    for address in params.get("destinationAddress") or []:
        if address[0] in ("h323-ID", "dialedDigits", "e164"):
            value = address[1]
            if isinstance(value, bytes):
                value = value.decode("ascii", "ignore")
            return str(value)
    return ""


def source_alias(params):
    for address in params.get("sourceAddress") or []:
        if address[0] in ("h323-ID", "dialedDigits", "e164"):
            value = address[1]
            if isinstance(value, bytes):
                value = value.decode("ascii", "ignore")
            return str(value)
    return ""


class CommandReader:
    """Blocking JSON-line reader used by a forked child over its control socket."""

    def __init__(self, fd: int = 0):
        self.fd = fd
        self.buffer = b""
        self.pending = []

    def poll(self):
        while not self.pending:
            data = os.read(self.fd, MAX_COMMAND)
            if not data:
                self.pending.append({"command": "shutdown"})
                break
            self.buffer += data
            if len(self.buffer) > MAX_COMMAND * 8:
                raise ProtocolError("IPC command buffer exceeded bound")
            while b"\n" in self.buffer:
                line, self.buffer = self.buffer.split(b"\n", 1)
                self.pending.append(json.loads(line))
        return self.pending.pop(0)


class CommandQueue:
    """Non-blocking JSON-line queue for the parent process."""

    def __init__(self):
        self.buffer = b""
        self.pending = collections.deque()

    def feed(self, data: bytes):
        self.buffer += data
        if len(self.buffer) > MAX_COMMAND * 32:
            self.buffer = self.buffer[-MAX_COMMAND * 8 :]
        while b"\n" in self.buffer:
            line, self.buffer = self.buffer.split(b"\n", 1)
            if line.strip():
                self.pending.append(json.loads(line))

    def pop(self):
        return self.pending.popleft() if self.pending else None


DTMF_EVENT_BY_SYMBOL = {
    **{str(digit): digit for digit in range(10)},
    "*": 10,
    "#": 11,
    "A": 12,
    "B": 13,
    "C": 14,
    "D": 15,
}


def user_input_digits(value) -> list[int]:
    """RFC 4733 event codes from a decoded H.245 UserInputIndication.

    `signal` carries one IA5 symbol ("5", "#", "*", A-D); `alphanumeric` can
    carry a whole string, one event per character. Everything else (encrypted
    strings, updates, unknown choices) is ignored.
    """
    if not (isinstance(value, tuple) and len(value) == 2):
        return []
    kind, payload = value
    if kind == "signal" and isinstance(payload, dict):
        symbols = [payload.get("signalType") or ""]
    elif kind == "alphanumeric" and isinstance(payload, str):
        symbols = list(payload)
    else:
        return []
    codes = []
    for symbol in symbols:
        if not isinstance(symbol, str):
            continue
        code = DTMF_EVENT_BY_SYMBOL.get(symbol.strip().upper()[:1])
        if code is not None:
            codes.append(code)
    return codes


class InboundCall:
    def __init__(self, signal, remote, setup):
        self.signal = signal
        self.remote = remote
        self.ref, self.raw_ref, call_id, self.called, self.caller, self.conference = setup
        self.call_id = uuid.UUID(bytes=call_id) if isinstance(call_id, (bytes, bytearray)) else uuid.UUID(str(call_id))
        self.control = None
        self.control_listener = None
        self.control_buffer = b""
        self.signal_buffer = b""
        self.config = None
        self.connected = False
        self.closed = False
        self.reason = "local_hangup"
        self.tcs_acked = False
        self.master_determined = False
        self.is_master = True
        self.remote_tcs_seen = False
        self.video_tx_pt = 96
        self.video_rx_pt = None
        self.control_window = 0.0
        self.control_messages = 0
        self.remote_h264 = False
        self.remote_h241 = {}
        self.local_audio_pt = 0
        self.audio_tx_pt = 0
        self.audio_codec = "G711U"
        self.audio_candidates = []
        self.audio_fallback_channel = 3
        self.audio_channel = None
        self.requested_audio_codec = {}
        self.remote_audio = None
        self.remote_video = None
        self.incoming_audio = False
        self.incoming_video = False
        self.requested_channels = {}
        self.incoming_channels = {}
        self.event_sink = None
        self.channels_opened = False
        self.local_msd_number = int.from_bytes(os.urandom(3), "big")
        self.started = time.monotonic()
        self.audio_ready_at = None

    def send_signal(self, message_type, body_name, params):
        body = encode(asn.H323_MESSAGES.H323_UserInformation, {
            "h323-uu-pdu": {"h323-message-body": (body_name, params), "h245Tunneling": False}
        })
        bearer = b"\x88\x93\xa5" if self.config.get("video_port") else b"\x88\x90\xa5"
        packet = b"\x08" + bytes([len(self.raw_ref)]) + self.raw_ref + bytes([message_type]) + b"\x04\x03" + bearer + b"\x28\x07VKS MCU\x7e" + (len(body) + 1).to_bytes(2, "big") + b"\x05" + body
        self.signal.sendall(tpkt(packet))

    def send_control(self, value):
        if self.control is None:
            raise ProtocolError("H.245 control channel unavailable")
        body = encode(asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, value)
        self.control.sendall(tpkt(body))

    def send_release(self):
        try:
            self.send_signal(0x5A, "releaseComplete", {
                "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
                "callIdentifier": {"guid": self.call_id.bytes},
            })
        except Exception:
            pass

    def capability_set(self):
        entries = [
            {"capabilityTableEntryNumber": number, "capability": capability}
            for number, capability in audio_capability_entries()
        ]
        simultaneous = [list(range(1, len(entries) + 1))]
        if self.config.get("video_port"):
            video_number = len(entries) + 1
            entries.append({"capabilityTableEntryNumber": video_number, "capability": ("receiveAndTransmitVideoCapability", video_capability(self.config))})
            simultaneous.append([video_number])
        distribution = {"centralizedControl": False, "distributedControl": False, "centralizedAudio": False, "distributedAudio": False, "centralizedVideo": False, "distributedVideo": False}
        multipoint = {"multicastCapability": False, "multiUniCastConference": False, "mediaDistributionCapability": [distribution]}
        packetization = {"h261aVideoPacketization": False}
        if self.config.get("video_port"):
            packetization["rtpPayloadType"] = [{"payloadDescriptor": ("oid", H264_NON_INTERLEAVED)}]
        multiplex = {"maximumAudioDelayJitter": 60, "receiveMultipointCapability": multipoint, "transmitMultipointCapability": multipoint, "receiveAndTransmitMultipointCapability": multipoint, "mcCapability": {"centralizedConferenceMC": True, "decentralizedConferenceMC": False}, "rtcpVideoControlCapability": False, "mediaPacketizationCapability": packetization, "logicalChannelSwitchingCapability": False, "t120DynamicPortCapability": False}
        return {"sequenceNumber": 1, "protocolIdentifier": (0, 0, 8, 245, 0, 13), "multiplexCapability": ("h2250Capability", multiplex), "capabilityTable": entries, "capabilityDescriptors": [{"capabilityDescriptorNumber": 0, "simultaneousCapabilities": simultaneous}]}

    def open_outgoing(self, number, kind, local_port):
        if kind == "audio":
            data_type = audio_data_type(self.audio_codec)
        else:
            data_type = ("videoData", video_capability(self.config))
        params = {"sessionID": 1 if kind == "audio" else 2, "mediaControlChannel": h245_address(self.config["local_ip"], local_port + 1)}
        if kind == "audio":
            params["silenceSuppression"] = False
        else:
            params["dynamicRTPPayloadType"] = 96
            params["mediaPacketization"] = ("rtpPayloadType", {"payloadDescriptor": ("oid", H264_NON_INTERLEAVED), "payloadType": 96})
        self.requested_channels[number] = kind
        if kind == "audio":
            self.audio_channel = number
            self.requested_audio_codec[number] = self.audio_codec
        self.send_control(("request", ("openLogicalChannel", {"forwardLogicalChannelNumber": number, "forwardLogicalChannelParameters": {"dataType": data_type, "multiplexParameters": ("h2250LogicalChannelParameters", params)}})))

    def reject_open(self, number, cause):
        self.send_control(("response", ("openLogicalChannelReject", {"forwardLogicalChannelNumber": number, "cause": (cause, 0)})))

    def on_open(self, params):
        number = params["forwardLogicalChannelNumber"]
        forward = params["forwardLogicalChannelParameters"]
        kind, pt = codec_kind(forward["dataType"])
        mux = forward.get("multiplexParameters")
        if kind is None or not mux or mux[0] != "h2250LogicalChannelParameters":
            self.reject_open(number, "dataTypeNotSupported")
            return
        if kind == "video" and not self.config.get("video_port"):
            self.reject_open(number, "dataTypeNotSupported")
            return
        media = mux[1]
        expected_session = 1 if kind == "audio" else 2
        if media["sessionID"] != expected_session or "reverseLogicalChannelParameters" in params:
            self.reject_open(number, "unspecified")
            return
        local_port = self.config["audio_port"] if kind == "audio" else self.config["video_port"]
        ack = {"sessionID": expected_session, "mediaChannel": h245_address(self.config["local_ip"], local_port), "mediaControlChannel": h245_address(self.config["local_ip"], local_port + 1)}
        if kind == "video":
            remote_pt = media.get("dynamicRTPPayloadType")
            if remote_pt is None or not 96 <= remote_pt <= 127:
                self.reject_open(number, "dataTypeNotSupported")
                return
            ack["dynamicRTPPayloadType"] = remote_pt
            self.video_rx_pt = remote_pt
            self.incoming_video = True
        else:
            if self.incoming_audio or media.get("dynamicRTPPayloadType") is not None:
                self.reject_open(number, "dataTypeNotSupported")
                return
            if forward["dataType"][1][0] == "g7231" and (
                not peer_transmits_g723(forward["dataType"][1])
                or forward["dataType"][1][1].get("silenceSuppression") is not False
                or media.get("silenceSuppression") is True
            ):
                self.reject_open(number, "dataTypeNotSupported")
                return
            incoming_codec = audio_codec_for_payload(pt)
            if not can_adopt_audio_codec(incoming_codec, self.audio_candidates, self.audio_codec, self.remote_audio is not None):
                self.reject_open(number, "dataTypeNotSupported")
                return
            if incoming_codec != self.audio_codec:
                self.audio_codec = incoming_codec
                self.audio_candidates = [incoming_codec]
                self.open_outgoing(self.audio_fallback_channel, "audio", self.config["audio_port"])
                self.audio_fallback_channel += 1
            self.local_audio_pt = pt
            self.incoming_audio = True
        self.incoming_channels[number] = kind
        self.send_control(("response", ("openLogicalChannelAck", {"forwardLogicalChannelNumber": number, "forwardMultiplexAckParameters": ("h2250LogicalChannelAckParameters", ack)})))

    def on_close(self, params):
        number = params["forwardLogicalChannelNumber"]
        kind = self.incoming_channels.pop(number, None) or self.requested_channels.pop(number, None)
        self.send_control(("response", ("closeLogicalChannelAck", {"forwardLogicalChannelNumber": number})))
        if kind == "audio":
            self.reason, self.closed = "remote_channel_closed", True
        elif kind == "video":
            self.incoming_video = False
            self.remote_video = None
            if self.event_sink is not None:
                try:
                    self.event_sink("channel_closed", kind="video", channel=number)
                except OSError:
                    self.closed = True
        # Unknown/duplicate channel: acknowledge and keep the call alive.

    def on_ack(self, params):
        number = params["forwardLogicalChannelNumber"]
        kind = self.requested_channels.get(number)
        mux = params.get("forwardMultiplexAckParameters")
        if kind is None or not mux or mux[0] != "h2250LogicalChannelAckParameters":
            return
        channel = mux[1].get("mediaChannel")
        if not channel:
            return
        try:
            remote = endpoint(channel, self.remote)
        except ProtocolError:
            try:
                remote = (self.remote, int(channel[1][1]["tsapIdentifier"]))
            except (IndexError, KeyError, TypeError, ValueError):
                return
        if kind == "audio":
            if (self.audio_channel is not None and number != self.audio_channel) or self.remote_audio is not None:
                return
            if mux[1].get("dynamicRTPPayloadType") is not None:
                raise ProtocolError("Peer acknowledged a dynamic audio PT for a static OLC")
            self.remote_audio = remote
            self.audio_tx_pt = audio_payload_type(self.requested_audio_codec.get(number, self.audio_codec))
        else:
            pt = mux[1].get("dynamicRTPPayloadType", 96)
            if not 96 <= pt <= 127:
                return
            self.remote_video = remote
            self.video_tx_pt = pt

    def on_control(self, payload):
        try:
            self._on_control(payload)
        except ProtocolError:
            raise
        except (KeyError, IndexError, TypeError, ValueError, UnicodeError) as error:
            raise ProtocolError(f"Malformed H.245 message: {type(error).__name__}")

    def _on_control(self, payload):
        category, (name, params) = decode(asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, payload)
        if category == "request":
            if name == "terminalCapabilitySet":
                if self.channels_opened:
                    self.send_control(("response", ("terminalCapabilitySetAck", {"sequenceNumber": params["sequenceNumber"]})))
                    return
                caps = [entry.get("capability") for entry in params.get("capabilityTable", [])]
                self.remote_h264 = any(cap and cap[0] in ("receiveVideoCapability", "receiveAndTransmitVideoCapability") and codec_kind(("videoData", cap[1]))[0] == "video" for cap in caps)
                for cap in caps:
                    if cap and cap[0] in ("receiveVideoCapability", "receiveAndTransmitVideoCapability"):
                        parameters = h241_video_parameters(cap[1])
                        if parameters:
                            self.remote_h241 = parameters
                            break
                candidates = audio_capabilities_from_tcs(params)
                if not candidates:
                    raise ProtocolError("No common symmetric H.323 audio codec (two-way audio is required)")
                self.audio_candidates = candidates
                self.audio_codec = candidates[0]
                self.remote_tcs_seen = True
                self.send_control(("response", ("terminalCapabilitySetAck", {"sequenceNumber": params["sequenceNumber"]})))
            elif name == "masterSlaveDetermination":
                remote_type, number = params["terminalType"], params["statusDeterminationNumber"]
                if remote_type == 240:
                    difference = (number - self.local_msd_number) & 0xFFFFFF
                    if difference in (0, 0x800000):
                        self.send_control(("response", ("masterSlaveDeterminationReject", {"cause": ("identicalNumbers", 0)})))
                        raise ProtocolError("Ambiguous master/slave determination")
                    self.is_master = difference < 0x800000
                else:
                    self.is_master = remote_type < 240
                self.send_control(("response", ("masterSlaveDeterminationAck", {"decision": ("slave" if self.is_master else "master", 0)})))
            elif name == "openLogicalChannel":
                self.on_open(params)
            elif name == "roundTripDelayRequest":
                self.send_control(("response", ("roundTripDelayResponse", {"sequenceNumber": params["sequenceNumber"]})))
            elif name == "closeLogicalChannel":
                self.on_close(params)
        elif category == "response":
            if name == "terminalCapabilitySetAck" and params["sequenceNumber"] == 1:
                self.tcs_acked = True
            elif name == "terminalCapabilitySetReject":
                raise ProtocolError("Peer rejected our H.245 terminal capability set")
            elif name == "masterSlaveDeterminationReject":
                raise ProtocolError("Peer rejected master/slave determination")
            elif name == "masterSlaveDeterminationAck":
                self.master_determined = True
                self.is_master = params["decision"][0] == "master"
            elif name == "openLogicalChannelAck":
                self.on_ack(params)
            elif name == "openLogicalChannelReject":
                number = params["forwardLogicalChannelNumber"]
                kind = self.requested_channels.get(number)
                if kind == "audio":
                    if (self.audio_channel is not None and number != self.audio_channel) or self.remote_audio is not None:
                        return
                    cause = params.get("cause", (None, 0))[0]
                    if cause not in ("dataTypeNotSupported", "inconsistentParameters"):
                        raise ProtocolError(f"Peer rejected the audio channel with terminal cause {cause}")
                    self.requested_channels.pop(number, None)
                    if not self.incoming_audio:
                        rejected = self.requested_audio_codec.pop(number, self.audio_codec)
                        next_codec = next_audio_candidate(self.audio_candidates, rejected)
                        if next_codec is not None:
                            self.audio_codec = next_codec
                            self.open_outgoing(self.audio_fallback_channel, "audio", self.config["audio_port"])
                            self.audio_fallback_channel += 1
                            return
                    raise ProtocolError("Peer rejected the negotiated audio channel")
        elif category == "indication":
            if name == "userInput":
                for digit in user_input_digits(params):
                    fields = {"call_ref": self.ref, "digit": digit, "direction": "inbound"}
                    if self.event_sink is not None:
                        self.event_sink("dtmf_digit", **fields)
                    else:
                        emit("dtmf_digit", call_id=str(self.call_id), **fields)
        elif category == "command":
            if name == "endSessionCommand":
                self.reason, self.closed = "remote_end_session", True
            elif name == "miscellaneousCommand":
                # H.245 §MiscellaneousCommand: a receiver that lost sync asks
                # the sender for an I-frame. The Rust side turns this into an
                # IDR request for the encoder (same counter as RTCP PLI/FIR).
                command = params.get("type") or (None, None)
                if command[0] == "videoFastUpdatePicture":
                    fields = {
                        "call_ref": self.ref,
                        "direction": "inbound",
                        "logical_channel": params.get("logicalChannelNumber"),
                    }
                    if self.event_sink is not None:
                        self.event_sink("video_fast_update", **fields)
                    else:
                        emit("video_fast_update", call_id=str(self.call_id), **fields)

    def open_channels(self):
        if self.channels_opened or not (self.tcs_acked and self.master_determined and self.remote_tcs_seen):
            return
        self.channels_opened = True
        self.open_outgoing(1, "audio", self.config["audio_port"])
        if self.config.get("video_port") and self.remote_h264:
            self.open_outgoing(2, "video", self.config["video_port"])

    def maybe_connected(self):
        if self.connected or not (self.tcs_acked and self.master_determined and self.incoming_audio and self.remote_audio):
            return
        if self.config.get("video_port") and self.remote_h264 and not (self.remote_video and self.incoming_video):
            if self.audio_ready_at is None:
                self.audio_ready_at = time.monotonic()
            if time.monotonic() - self.audio_ready_at < 3.0:
                return
        self.connected = True
        emit("connected", direction="inbound", call_id=str(self.call_id), call_ref=self.ref,
             remote_ip=self.remote_audio[0], remote_audio_port=self.remote_audio[1],
             remote_audio_payload_type=self.audio_tx_pt, local_audio_payload_type=self.local_audio_pt,
             remote_video_ip=self.remote_video[0] if self.remote_video else None,
             remote_video_port=self.remote_video[1] if self.remote_video else None,
             video_tx_payload_type=self.video_tx_pt if self.remote_video else None,
             video_rx_payload_type=self.video_rx_pt if self.incoming_video else None,
             remote_video_max_bitrate_100bps=self.remote_h241.get("max_bitrate_100bps"),
             remote_video_h264_level=self.remote_h241.get("level_idc"),
             remote_video_h264_profile_bits=self.remote_h241.get("profile_bits"),
             video_status="connected" if self.remote_video and self.incoming_video else "unsupported" if self.config.get("video_port") else "not_requested")

    def on_signal(self, payload):
        ref, _raw, message, information, cause, called = parse_q931(payload)
        if ref != self.ref:
            raise ProtocolError("Mismatched Q.931 call reference")
        if message == 0x5A:
            self.reason = audio.release_reason_token(information, cause)
            self.closed = True
            return
        if message != 0x07 or information is None:
            return
        name, params = information["h323-uu-pdu"]["h323-message-body"]
        if name == "connect" and params.get("h245Address"):
            address = params["h245Address"]
            if address[0] == "ipAddress":
                address_ip = str(ipaddress.IPv4Address(address[1]["ip"]))
                if address_ip != self.remote:
                    # Media/H.245 destination is pinned to the TCP peer; a
                    # different address would turn the UAS into a UDP relay.
                    raise ProtocolError("Q.931 Connect redirects H.245 away from the TCP peer")

    def frames(self, sock):
        data = sock.recv(MAX_FRAME)
        if not data:
            self.reason, self.closed = "remote_closed", True
            return
        if sock is self.signal:
            self.signal_buffer += data
            buffer = self.signal_buffer
        else:
            self.control_buffer += data
            buffer = self.control_buffer
        while len(buffer) >= 4:
            if buffer[0:2] != b"\x03\x00":
                raise ProtocolError("Invalid TPKT header")
            length = int.from_bytes(buffer[2:4], "big")
            if not 5 <= length <= MAX_FRAME:
                raise ProtocolError("Invalid TPKT length")
            if len(buffer) < length:
                break
            payload, buffer = buffer[4:length], buffer[length:]
            if sock is self.signal:
                self.on_signal(payload)
            else:
                now = time.monotonic()
                if now - self.control_window >= 1.0:
                    self.control_window, self.control_messages = now, 0
                self.control_messages += 1
                if self.control_messages > 200:
                    raise ProtocolError("H.245 request rate exceeded")
                self.on_control(payload)
        if len(buffer) > MAX_FRAME:
            raise ProtocolError("TPKT receive buffer exceeded bound")
        if sock is self.signal:
            self.signal_buffer = buffer
        else:
            self.control_buffer = buffer

    def run(self, accept, commands):
        self.config = accept
        self.signal.settimeout(5)
        self.control_listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.control_listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.control_listener.bind((self.config["local_ip"], 0))
        self.control_listener.listen(1)
        control_port = self.control_listener.getsockname()[1]
        call_id = {"guid": self.call_id.bytes}
        self.send_signal(0x02, "callProceeding", {
            "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
            "destinationInfo": {"mcu": {}, "mc": True, "undefinedNode": False},
            "h245Address": h245_ip_address(self.config["local_ip"], control_port),
            "callIdentifier": call_id,
            "multipleCalls": False, "maintainConnection": False,
        })
        self.send_signal(0x07, "connect", {
            "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
            "h245Address": h245_ip_address(self.config["local_ip"], control_port),
            "destinationInfo": {"mcu": {}, "mc": True, "undefinedNode": False},
            "conferenceID": self.conference,
            "callIdentifier": call_id,
            "multipleCalls": False, "maintainConnection": False,
        })
        self.signal.settimeout(2)
        ready, _, _ = select.select([self.control_listener, commands.fd], [], [], 10)
        if commands.fd in ready:
            command = commands.poll()
            if command.get("command") in ("hangup", "shutdown"):
                self.reason, self.closed = "local_hangup", True
                return
        if self.control_listener not in ready:
            raise ProtocolError("Peer did not open the H.245 channel")
        self.control, control_peer = self.control_listener.accept()
        if control_peer[0] != self.remote:
            self.control.close()
            raise ProtocolError("H.245 connection did not come from the Q.931 peer")
        self.control.settimeout(2)
        emit("negotiating", call_id=str(self.call_id), protocol="h245")
        self.send_control(("request", ("terminalCapabilitySet", self.capability_set())))
        self.send_control(("request", ("masterSlaveDetermination", {"terminalType": 240, "statusDeterminationNumber": self.local_msd_number})))
        while not self.closed:
            if time.monotonic() - self.started > 60 and not self.connected:
                raise TimeoutError("H.323 inbound media negotiation timed out")
            sockets = [self.signal, self.control, commands.fd]
            ready, _, _ = select.select(sockets, [], [], .2)
            for sock in ready:
                if sock == commands.fd:
                    command = commands.poll()
                    if command.get("command") == "shutdown":
                        self.reason, self.closed = "parent_shutdown", True
                    elif command.get("command") == "hangup":
                        self.reason, self.closed = "local_hangup", True
                else:
                    self.frames(sock)
                if self.closed:
                    break
            self.open_channels()
            self.maybe_connected()

    def close(self):
        if self.control is not None:
            try:
                self.control.settimeout(.5)
                self.send_control(("command", ("endSessionCommand", ("disconnect", 0))))
            except Exception:
                pass
            self.control.close()
        if self.control_listener is not None:
            self.control_listener.close()
        self.send_release()
        try:
            self.signal.close()
        except OSError:
            pass
        emit("disconnected", call_id=str(self.call_id), reason=self.reason, was_connected=self.connected)


def release_complete(conn, raw_ref, call_id, cause=21):
    """ReleaseComplete for a call rejected before InboundCall exists."""
    try:
        body = encode(asn.H323_MESSAGES.H323_UserInformation, {
            "h323-uu-pdu": {
                "h323-message-body": ("releaseComplete", {
                    "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
                    "callIdentifier": {"guid": call_id},
                }),
                "h245Tunneling": False,
            }
        })
        packet = (b"\x08" + bytes([len(raw_ref)]) + raw_ref + b"\x5a"
                  + b"\x08\x02\x80" + bytes([cause])
                  + b"\x7e" + (len(body) + 1).to_bytes(2, "big") + b"\x05" + body)
        conn.sendall(tpkt(packet))
    except Exception:
        pass


def peer_allowed(peer_ip, networks):
    if not networks:
        # H.323 is unauthenticated/plaintext: no allowlist means no admission.
        return False
    try:
        address = ipaddress.ip_address(peer_ip)
    except ValueError:
        return False
    return any(address in network for network in networks)


def load_configuration():
    line = b""
    while len(line) <= MAX_COMMAND and not line.endswith(b"\n"):
        value = os.read(sys.stdin.fileno(), 1)
        if not value:
            break
        line += value
    if not line.endswith(b"\n") or len(line) > MAX_COMMAND:
        raise ProtocolError("Missing or oversized initial configuration")
    value = json.loads(line)
    bind = str(value.get("bind", "0.0.0.0:1720"))
    local_ip = str(ipaddress.IPv4Address(value.get("local_ip", "0.0.0.0")))
    if ipaddress.IPv4Address(local_ip).is_unspecified:
        raise ProtocolError("H.323 UAS local_ip must be concrete")
    raw_allow = value.get("allow_cidrs") or []
    if isinstance(raw_allow, str):
        raw_allow = [part for part in raw_allow.split(",") if part.strip()]
    networks = []
    for cidr in raw_allow:
        try:
            networks.append(ipaddress.ip_network(str(cidr).strip(), strict=False))
        except ValueError as error:
            raise ProtocolError(f"Invalid VKS_H323_UAS_ALLOW entry: {cidr}") from error
    return {
        "bind": bind,
        "local_ip": local_ip,
        "max_bitrate_100bps": max(1, min(int(value.get("video_max_bitrate_100bps", 5120)), 192400)),
        "level_idc": int(value.get("video_h264_level", 71)),
        "max_children": max(1, min(int(value.get("max_children", 64)), 256)),
        "max_children_per_ip": max(1, min(int(value.get("max_children_per_ip", 8)), 64)),
        "allow_cidrs": networks,
    }


def read_setup(conn):
    buffer = b""
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        data = conn.recv(MAX_FRAME)
        if not data:
            raise ProtocolError("Connection closed before H.225 Setup")
        buffer += data
        if len(buffer) > MAX_FRAME * 4:
            raise ProtocolError("H.225 receive buffer exceeded bound")
        while len(buffer) >= 4:
            if buffer[0:2] != b"\x03\x00":
                raise ProtocolError("Invalid TPKT header")
            length = int.from_bytes(buffer[2:4], "big")
            if not 5 <= length <= MAX_FRAME:
                raise ProtocolError("Invalid TPKT length")
            if len(buffer) < length:
                break
            payload, buffer = buffer[4:length], buffer[length:]
            ref, raw_ref, message, information, _cause, called = parse_q931(payload)
            if message != 5 or information is None:
                continue
            name, params = information["h323-uu-pdu"]["h323-message-body"]
            if name != "setup":
                continue
            guid = params.get("callIdentifier", {}).get("guid")
            conference = params.get("conferenceID") or guid
            if not guid or len(guid) != 16:
                raise ProtocolError("H.225 Setup without a valid callIdentifier")
            return ref, raw_ref, guid, (setup_alias(params) or called), source_alias(params), conference
    raise TimeoutError("H.225 Setup did not arrive")


def arm_parent_death_signal(parent_pid):
    """Kill forked call children if the listener parent dies unexpectedly."""
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        # Linux PR_SET_PDEATHSIG = 1, SIGKILL = 9.
        libc.prctl(1, 9, 0, 0, 0)
    except (OSError, AttributeError):
        pass
    if os.getppid() != parent_pid:
        os._exit(1)


def run_child(conn, peer, configuration, control, control_fd):
    # The parent is the only writer to the Rust stdout pipe. A child sends
    # events through its control socket; if the parent dies, the child sees
    # EPIPE and exits instead of keeping Rust's stdout pipe open forever.
    try:
        os.dup2(control_fd, 1)
    except OSError:
        pass
    call = None
    call_id = None
    raw_ref = b""
    raw_call_id = None
    try:
        conn.settimeout(10)
        ref, raw_ref, raw_call_id, called, caller, conference = read_setup(conn)
        call_id = str(uuid.UUID(bytes=raw_call_id))
        emit(
            "incoming",
            call_id=call_id,
            call_ref=ref,
            from_alias=caller,
            called_alias=called,
            peer_ip=peer[0],
            peer_port=peer[1],
        )
        control.sendall((json.dumps({"event": "register", "call_id": call_id}) + "\n").encode())
        commands = CommandReader(control_fd)
        accept = None
        deadline = time.monotonic() + 10
        while accept is None and time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            ready, _, _ = select.select([control_fd], [], [], max(0.0, remaining))
            if not ready:
                break
            command = commands.poll()
            if command.get("command") == "accept":
                accept = command
            elif command.get("command") in ("reject", "hangup"):
                raise ProtocolError("call rejected locally")
            elif command.get("command") == "shutdown":
                return
        if accept is None:
            raise TimeoutError("Parent did not accept the H.323 call")
        accept = {**configuration, **accept}
        call = InboundCall(conn, peer[0], (ref, raw_ref, raw_call_id, called, caller, conference))
        call.event_sink = lambda event, **fields: control.sendall(
            (json.dumps({"event": event, "call_id": str(call.call_id), **fields}, separators=(",", ":")) + "\n").encode()
        )
        signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(SystemExit(0)))
        call.run(accept, commands)
    except (ProtocolError, TimeoutError, OSError, ValueError) as error:
        fail = {"reason": f"{type(error).__name__}: {str(error)[:400]}"}
        if call_id is not None:
            fail["call_id"] = call_id
        emit("failed", **fail)
        if raw_call_id is not None:
            release_complete(conn, raw_ref, raw_call_id, 21)
        try:
            conn.close()
        except OSError:
            pass
    finally:
        if call is not None:
            call.close()
        try:
            control.sendall(b'{"event":"exited"}\n')
        except OSError:
            pass


def main():
    configuration = load_configuration()
    if not hasattr(os, "fork"):
        raise ProtocolError("H.323 UAS requires a POSIX fork-capable platform")
    parent_pid = os.getpid()
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    host, port = configuration["bind"].rsplit(":", 1)
    listener.bind((host, int(port)))
    listener.listen(64)
    listener.setblocking(False)
    emit("listening", bind=f"{host}:{port}")

    commands = CommandQueue()
    children = {}
    by_call = {}
    pending = {}
    peer_counts = {}
    stopping = False

    def close_child_socket(child):
        sock = child.get("sock")
        child["sock"] = None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass

    def release_child(pid, *, drain=False):
        """Idempotent teardown used by both EOF and waitpid paths."""
        child = children.get(pid)
        if child is None:
            return
        if drain:
            sock = child.get("sock")
            if sock is not None:
                while True:
                    try:
                        data = sock.recv(65536)
                    except (BlockingIOError, InterruptedError, OSError):
                        break
                    if not data:
                        break
                    drain_child(pid, data)
        close_child_socket(child)
        child["closed"] = True
        if child.get("released"):
            return
        child["released"] = True
        call_id = child.get("call_id")
        if call_id and not child.get("exited") and not child.get("failed_emitted"):
            child["failed_emitted"] = True
            emit("failed", call_id=call_id, reason="listener_child_released")
        if call_id:
            if by_call.get(call_id) == pid:
                by_call.pop(call_id, None)
            pending.pop(call_id, None)
        peer = child.get("peer_ip")
        if peer in peer_counts:
            peer_counts[peer] -= 1
            if peer_counts[peer] <= 0:
                peer_counts.pop(peer, None)

    def on_reaped(pid):
        child = children.get(pid)
        if child is not None:
            # waitpid may win against the select/EOF path; drain any trailing
            # disconnected/exited events before the socket is closed.
            release_child(pid, drain=True)
        children.pop(pid, None)

    def reap_available():
        # Sole waitpid owner. A pid is never retained after waitpid reaps it,
        # so shutdown can never signal a stale/reused pid.
        while True:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if pid == 0:
                return
            on_reaped(pid)

    def send_child(pid, command):
        child = children.get(pid)
        sock = child.get("sock") if child is not None else None
        if sock is None:
            return
        if command.get("command") in ("hangup", "reject"):
            child["hangup_at"] = time.monotonic()
        try:
            sock.sendall((json.dumps(command, separators=(",", ":")) + "\n").encode())
        except OSError:
            pass

    def kill_timed_out_children():
        now = time.monotonic()
        for pid, child in list(children.items()):
            hangup_at = child.get("hangup_at")
            if hangup_at is None or child.get("exited"):
                continue
            if now - hangup_at > 5:
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass

    def drain_child(pid, data):
        child = children.get(pid)
        if child is None:
            return
        child["buffer"] += data
        while b"\n" in child["buffer"]:
            line, child["buffer"] = child["buffer"].split(b"\n", 1)
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            kind = event.get("event")
            call_id = event.get("call_id")
            if kind == "register" and call_id:
                child["call_id"] = call_id
                by_call[call_id] = pid
                for queued in pending.pop(call_id, []):
                    send_child(pid, queued)
            elif kind == "exited":
                child["exited"] = True
            else:
                # Parent is the sole stdout writer; preserve child event bytes.
                try:
                    os.write(1, (json.dumps(event, ensure_ascii=True, separators=(",", ":")) + "\n").encode())
                except OSError:
                    pass

    def forget(pid):
        # EOF path: the child is not reaped yet, so it stays in `children`
        # (and can be signalled) until the single reaper removes it.
        release_child(pid)
        reap_available()

    while not stopping:
        watched = [listener, 0] + [child["sock"] for child in children.values() if child.get("sock") is not None]
        try:
            ready, _, _ = select.select(watched, [], [], .5)
        except InterruptedError:
            continue
        except OSError:
            break
        kill_timed_out_children()
        if 0 in ready:
            data = os.read(0, MAX_COMMAND * 8)
            if not data:
                stopping = True
            else:
                try:
                    commands.feed(data)
                except (ValueError, ProtocolError):
                    emit("failed", reason="malformed IPC command")
                    break
                while True:
                    command = commands.pop()
                    if command is None:
                        break
                    if command.get("command") == "shutdown":
                        stopping = True
                        break
                    call_id = command.get("call_id")
                    pid = by_call.get(call_id) if call_id else None
                    if pid is not None:
                        send_child(pid, command)
                    elif call_id:
                        if len(pending) > 256:
                            pending.pop(next(iter(pending)), None)
                        pending.setdefault(call_id, []).append(command)
        if listener in ready and not stopping:
            try:
                conn, peer = listener.accept()
            except OSError:
                conn = None
            if conn is not None:
                peer_ip = peer[0]
                if not peer_allowed(peer_ip, configuration["allow_cidrs"]):
                    conn.close()
                    continue
                if (len(children) >= configuration["max_children"]
                        or peer_counts.get(peer_ip, 0) >= configuration["max_children_per_ip"]):
                    conn.close()
                    continue
                parent_sock, child_sock = socket.socketpair()
                try:
                    pid = os.fork()
                except OSError:
                    parent_sock.close()
                    child_sock.close()
                    conn.close()
                    continue
                if pid == 0:  # child
                    try:
                        arm_parent_death_signal(parent_pid)
                        parent_sock.close()
                        for sibling in children.values():
                            sibling["sock"].close()
                        listener.close()
                        run_child(conn, peer, configuration, child_sock, child_sock.fileno())
                    except BaseException as error:  # pragma: no cover - last resort
                        emit("failed", reason=f"{type(error).__name__}: {str(error)[:400]}")
                    finally:
                        os._exit(0)
                child_sock.close()
                conn.close()
                children[pid] = {"sock": parent_sock, "call_id": None, "buffer": b"", "peer_ip": peer_ip, "exited": False}
                peer_counts[peer_ip] = peer_counts.get(peer_ip, 0) + 1
        for pid, child in list(children.items()):
            if child["sock"] in ready:
                try:
                    data = child["sock"].recv(4096)
                except OSError:
                    data = b""
                if not data:
                    forget(pid)
                else:
                    drain_child(pid, data)
        reap_available()

    for pid in list(children):
        send_child(pid, {"command": "shutdown"})
    deadline = time.monotonic() + 3
    while children and time.monotonic() < deadline:
        kill_timed_out_children()
        watched = [child["sock"] for child in children.values() if child.get("sock") is not None]
        try:
            ready, _, _ = select.select(watched, [], [], .1)
        except (InterruptedError, OSError):
            ready = []
        for pid, child in list(children.items()):
            sock = child.get("sock")
            if sock is None or sock not in ready:
                continue
            try:
                data = sock.recv(4096)
            except OSError:
                data = b""
            if data:
                drain_child(pid, data)
            else:
                forget(pid)
        reap_available()
    for pid in list(children):
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError:
            pass
    kill_deadline = time.monotonic() + 2
    while children and time.monotonic() < kill_deadline:
        reap_available()
        if children:
            for pid in list(children):
                try:
                    os.kill(pid, signal.SIGKILL)
                except OSError:
                    pass
            time.sleep(.02)
    reap_available()
    try:
        listener.close()
    except OSError:
        pass


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        pass
    except Exception as error:
        emit("failed", reason=f"{type(error).__name__}: {str(error)[:400]}")
        raise SystemExit(1)

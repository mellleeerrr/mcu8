#!/usr/bin/env python3
"""One H.323 call per process; stdin commands and stdout JSON events only.

Author-written transport/FSM glue uses independently generated H.225/H.245 PER
objects. Python/Pycrate never handles media samples: negotiated RTP endpoints
are handed to the Rust media bridge. See PROVENANCE.md and requirements.lock.
"""
from __future__ import annotations

import contextlib
import ipaddress
import json
import os
import signal
import secrets
import select
import socket
import sys
import time
import uuid
from dataclasses import dataclass

MAX_FRAME = 65535
MAX_COMMAND = 4096
H264_OID = (0, 0, 8, 241, 0, 0, 1)
H264_NON_INTERLEAVED = (0, 0, 8, 241, 0, 0, 0, 1)

# Do not allow a diagnostic print in an upstream codec to corrupt stdout IPC.
try:
    with contextlib.redirect_stdout(sys.stderr):
        import h323_asn as asn
        from pycrate_core.charpy import Charpy
except ImportError as dependency_error:
    sys.stdout.write(json.dumps({"event": "failed", "reason": "ASN.1 runtime unavailable; Pycrate 0.8.1 and generated H.323 descriptions are required"}) + "\n")
    sys.stdout.flush()
    raise SystemExit(1) from dependency_error


class ProtocolError(Exception):
    pass


def emit(event: str, **fields):
    data = {"event": event, **fields}
    sys.stdout.write(json.dumps(data, ensure_ascii=True, separators=(",", ":")) + "\n")
    sys.stdout.flush()


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
    raw_reference = int.from_bytes(payload[2:2 + reference_length], "big")
    reference = raw_reference & (0x7F if reference_length == 1 else 0x7FFF)
    message = payload[reference_length + 2]
    position = reference_length + 3
    user_information, cause = None, None
    while position < len(payload):
        tag = payload[position]
        position += 1
        if tag & 0x80:  # single-octet IE, including codeset shift
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
    return reference, message, user_information, cause


def h245_address(ip: str, port: int):
    return ("unicastAddress", ("iPAddress", {"network": ipaddress.IPv4Address(ip).packed, "tsapIdentifier": port}))


def endpoint(address, expected_ip: str):
    if not address or address[0] != "unicastAddress" or address[1][0] != "iPAddress":
        raise ProtocolError("Only H.245 unicast IPv4 media is supported")
    value = address[1][1]
    ip = str(ipaddress.IPv4Address(value["network"]))
    port = value["tsapIdentifier"]
    if ip != expected_ip or not 1 <= port <= 65535:
        raise ProtocolError("Peer advertised an invalid or foreign media address")
    return ip, port


def video_capability():
    # H.241: baseline profile, Level 3.1; H264 RTP non-interleaved packetization.
    return ("genericVideoCapability", {
        "capabilityIdentifier": ("standard", H264_OID),
        "maxBitRate": 5120,  # units 100 bit/s
        "collapsing": [
            {"parameterIdentifier": ("standard", 41), "parameterValue": ("booleanArray", 64)},
            {"parameterIdentifier": ("standard", 42), "parameterValue": ("unsignedMin", 71)},
        ],
    })


def codec_kind(data_type):
    kind, codec = data_type
    if kind == "audioData" and codec[0] == "g711Ulaw64k":
        return "audio", 0
    if kind == "audioData" and codec[0] == "g711Alaw64k":
        return "audio", 8
    if kind == "videoData" and codec[0] == "genericVideoCapability":
        ident = codec[1].get("capabilityIdentifier")
        if ident == ("standard", H264_OID):
            # A baseline-compatible profile must be explicit when supplied.
            profile = next((v["parameterValue"][1] for v in codec[1].get("collapsing", []) if v["parameterIdentifier"] == ("standard", 41)), 64)
            if profile & 64:
                return "video", None
    return None, None


@dataclass
class Configuration:
    target_ip: str
    target_port: int
    local_ip: str
    audio_port: int
    video_port: int | None
    timeout: float
    call_id: str
    call_ref: int
    trace: bool = False

    @classmethod
    def load(cls, value):
        target_ip = str(ipaddress.IPv4Address(value["target_ip"]))
        local_ip = str(ipaddress.IPv4Address(value["local_ip"]))
        if ipaddress.IPv4Address(target_ip).is_unspecified or ipaddress.IPv4Address(local_ip).is_unspecified:
            raise ProtocolError("Unspecified H.323 address")
        port = int(value.get("target_port", 1720))
        audio, video = int(value["audio_port"]), value.get("video_port")
        if not 1 <= port <= 65535 or not 1 <= audio <= 65534:
            raise ProtocolError("Invalid H.323 TCP/RTP port")
        if video is not None:
            video = int(video)
            if not 1 <= video <= 65534 or abs(video - audio) < 2:
                raise ProtocolError("Overlapping H.323 media port pairs")
        call_id = uuid.UUID(value["call_id"])
        # Both H.225 conferenceID and callIdentifier are GUIDs. Polycom treats
        # an all-zero GUID as an invalid conference identifier and releases the
        # call with Q.931 cause invalidCID before H.245 starts.
        if call_id.int == 0:
            raise ProtocolError("H.323 call_id must be a non-nil UUID")
        return cls(target_ip, port, local_ip, audio, video, max(1, min(float(value["timeout_seconds"]), 180)), str(call_id), max(1, int(value["call_ref"]) & 0x7FFF), bool(value.get("trace", False)))


class Call:
    def __init__(self, config: Configuration):
        self.config = config
        self.signal = None
        self.control = None
        self.buffers = {}
        self.command_buffer = b""
        self.deadline = time.monotonic() + config.timeout
        self.connected = False
        self.signal_connected = False
        self.stopping = False
        self.reason = "local_hangup"
        self.tcs_acked = False
        self.master_determined = False
        self.is_master = True
        self.local_msd_number = secrets.randbelow(0xFFFFFF)
        self.channels_opened = False
        self.remote_audio = None
        self.remote_video = None
        self.audio_tx_pt = 0
        self.audio_rx_pt = 0
        self.video_tx_pt = None
        self.video_rx_pt = None
        self.incoming_audio = False
        self.incoming_video = False
        self.remote_h264 = False
        self.remote_video_known = False
        self.requested_channels = {}
        self.requested_video_rejected = False
        self.audio_ready_at = None
        self.received_requests = 0
        self.request_window = time.monotonic()
        self.remote_tcs_seen = False
        self.last_rtt = time.monotonic()

    def wire(self, direction, protocol, data, **fields):
        if self.config.trace:
            emit("wire", direction=direction, protocol=protocol, hex=data[:8192].hex(), length=len(data), **fields)

    def send_control(self, value):
        if self.control is None:
            raise ProtocolError("H.245 control channel unavailable")
        body = encode(asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, value)
        self.control.sendall(tpkt(body))
        self.wire("sent", "h245", body, name=value[1][0])

    def send_setup(self):
        c = self.config
        guid = uuid.UUID(c.call_id).bytes
        setup = {
            "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
            "sourceInfo": {"mcu": {}, "mc": True, "undefinedNode": False},
            "sourceAddress": [("h323-ID", "VKS MCU")],
            "destCallSignalAddress": ("ipAddress", {"ip": ipaddress.IPv4Address(c.target_ip).packed, "port": c.target_port}),
            "activeMC": True, "conferenceID": guid,
            "conferenceGoal": ("create", 0), "callType": ("pointToPoint", 0),
            "callIdentifier": {"guid": guid}, "mediaWaitForConnect": False,
            "canOverlapSend": False, "multipleCalls": False, "maintainConnection": False,
        }
        body = encode(asn.H323_MESSAGES.H323_UserInformation, {"h323-uu-pdu": {"h323-message-body": ("setup", setup), "h245Tunneling": False}})
        # H323Plus uses 384 kbit/s unrestricted digital bearer for video; 64
        # kbit/s bearer makes Polycom negotiate an audio-only call.
        bearer = b"\x88\x93\xa5" if c.video_port is not None else b"\x88\x90\xa5"
        q931 = b"\x08\x02" + c.call_ref.to_bytes(2, "big") + b"\x05\x04\x03" + bearer + b"\x28\x07VKS MCU\x7e" + (len(body) + 1).to_bytes(2, "big") + b"\x05" + body
        self.signal.sendall(tpkt(q931))
        self.wire("sent", "q931", q931, name="setup")

    def capability_set(self):
        entries = [{"capabilityTableEntryNumber": 1, "capability": ("receiveAndTransmitAudioCapability", ("g711Ulaw64k", 20))}]
        simultaneous = [[1]]
        if self.config.video_port is not None:
            entries.append({"capabilityTableEntryNumber": 2, "capability": ("receiveAndTransmitVideoCapability", video_capability())})
            simultaneous.append([2])
        distribution = {"centralizedControl": False, "distributedControl": False, "centralizedAudio": False, "distributedAudio": False, "centralizedVideo": False, "distributedVideo": False}
        multipoint = {"multicastCapability": False, "multiUniCastConference": False, "mediaDistributionCapability": [distribution]}
        packetization = {"h261aVideoPacketization": False}
        if self.config.video_port is not None:
            packetization["rtpPayloadType"] = [{"payloadDescriptor": ("oid", H264_NON_INTERLEAVED)}]
        multiplex = {"maximumAudioDelayJitter": 60, "receiveMultipointCapability": multipoint, "transmitMultipointCapability": multipoint, "receiveAndTransmitMultipointCapability": multipoint, "mcCapability": {"centralizedConferenceMC": True, "decentralizedConferenceMC": False}, "rtcpVideoControlCapability": False, "mediaPacketizationCapability": packetization, "logicalChannelSwitchingCapability": False, "t120DynamicPortCapability": False}
        return {"sequenceNumber": 1, "protocolIdentifier": (0, 0, 8, 245, 0, 13), "multiplexCapability": ("h2250Capability", multiplex), "capabilityTable": entries, "capabilityDescriptors": [{"capabilityDescriptorNumber": 0, "simultaneousCapabilities": simultaneous}]}

    def on_signal(self, payload):
        ref, message, information, cause = parse_q931(payload)
        if ref != self.config.call_ref:
            raise ProtocolError("Mismatched Q.931 call reference")
        self.wire("received", "q931", payload, message_type=message)
        if message == 0x5A:
            self.reason = "remote_release" + (f":cause={cause}" if cause is not None else "")
            self.stopping = True
            return
        if information is None:
            if message == 7:
                raise ProtocolError("CONNECT without H.225 information")
            return
        name, params = information["h323-uu-pdu"]["h323-message-body"]
        if message in (1, 2, 7) and {1: "alerting", 2: "callProceeding", 7: "connect"}[message] != name:
            raise ProtocolError("Q.931/H.225 message mismatch")
        identifier = params.get("callIdentifier", {}).get("guid")
        if identifier is not None and identifier != uuid.UUID(self.config.call_id).bytes:
            raise ProtocolError("Mismatched H.225 call identifier")
        if message == 1:
            emit("ringing", call_id=self.config.call_id)
        if message != 7:
            return
        if self.signal_connected:
            return
        self.signal_connected = True
        address = params.get("h245Address")
        if not address or address[0] != "ipAddress":
            raise ProtocolError("Peer did not provide a supported separate H.245 address")
        ip = str(ipaddress.IPv4Address(address[1]["ip"]))
        port = address[1]["port"]
        if ip != self.config.target_ip or not 1 <= port <= 65535:
            raise ProtocolError("Invalid or foreign H.245 control address")
        self.control = socket.create_connection((ip, port), timeout=min(5, max(.1, self.deadline - time.monotonic())))
        self.control.settimeout(2)
        self.buffers[self.control] = b""
        emit("negotiating", protocol="h245", control_ip=ip, control_port=port)
        self.send_control(("request", ("terminalCapabilitySet", self.capability_set())))
        self.send_control(("request", ("masterSlaveDetermination", {"terminalType": 240, "statusDeterminationNumber": self.local_msd_number})))

    def open_channels(self):
        if self.channels_opened or not (self.tcs_acked and self.master_determined and self.remote_tcs_seen):
            return
        self.channels_opened = True
        self.open_outgoing(1, "audio", self.config.audio_port)
        if self.config.video_port is not None and self.remote_h264:
            self.open_outgoing(2, "video", self.config.video_port)

    def open_outgoing(self, number, kind, local_port):
        data_type = ("audioData", ("g711Ulaw64k", 20)) if kind == "audio" else ("videoData", video_capability())
        params = {"sessionID": 1 if kind == "audio" else 2, "mediaControlChannel": h245_address(self.config.local_ip, local_port + 1)}
        if kind == "audio":
            params["silenceSuppression"] = False
        else:
            params["dynamicRTPPayloadType"] = 96
            params["mediaPacketization"] = ("rtpPayloadType", {"payloadDescriptor": ("oid", H264_NON_INTERLEAVED), "payloadType": 96})
        self.requested_channels[number] = kind
        self.send_control(("request", ("openLogicalChannel", {"forwardLogicalChannelNumber": number, "forwardLogicalChannelParameters": {"dataType": data_type, "multiplexParameters": ("h2250LogicalChannelParameters", params)}})))

    def reject_open(self, number, cause, kind):
        self.send_control(("response", ("openLogicalChannelReject", {"forwardLogicalChannelNumber": number, "cause": (cause, 0)})))
        if kind == "video":
            # A rejected video channel must not keep the audio call in the
            # "waiting for both directions" state until the global deadline.
            self.requested_video_rejected = True
            emit("video_unavailable", reason=cause)

    def on_open(self, params):
        number = params["forwardLogicalChannelNumber"]
        fw = params["forwardLogicalChannelParameters"]
        kind, pt = codec_kind(fw["dataType"])
        mux = fw.get("multiplexParameters")
        if kind is None or not mux or mux[0] != "h2250LogicalChannelParameters" or (kind == "video" and self.config.video_port is None) or (kind == "audio" and pt != 0):
            self.reject_open(number, "dataTypeNotSupported", kind)
            return
        media = mux[1]
        expected_session = 1 if kind == "audio" else 2
        if media["sessionID"] != expected_session or "reverseLogicalChannelParameters" in params:
            self.reject_open(number, "unspecified", kind)
            return
        local_port = self.config.audio_port if kind == "audio" else self.config.video_port
        # No zero-flow restriction is requested. Keep the baseline ACK form,
        # also used by the captured Polycom ACK, without that extension.
        ack = {"sessionID": expected_session, "mediaChannel": h245_address(self.config.local_ip, local_port), "mediaControlChannel": h245_address(self.config.local_ip, local_port + 1)}
        if kind == "video":
            pt = media.get("dynamicRTPPayloadType")
            if pt is None or not 96 <= pt <= 127:
                self.reject_open(number, "dataTypeNotSupported", kind)
                return
            packetization = media.get("mediaPacketization")
            if packetization and (packetization[0] != "rtpPayloadType" or packetization[1].get("payloadDescriptor") != ("oid", H264_NON_INTERLEAVED)):
                self.reject_open(number, "dataTypeNotSupported", kind)
                return
            ack["dynamicRTPPayloadType"] = pt
            self.video_rx_pt = pt
            self.incoming_video = True
        else:
            self.audio_rx_pt = pt
            self.incoming_audio = True
        self.send_control(("response", ("openLogicalChannelAck", {"forwardLogicalChannelNumber": number, "forwardMultiplexAckParameters": ("h2250LogicalChannelAckParameters", ack)})))
        emit("channel", direction="incoming", kind=kind, payload_type=pt, local_port=local_port)

    def on_ack(self, params):
        number = params["forwardLogicalChannelNumber"]
        kind = self.requested_channels.get(number)
        mux = params.get("forwardMultiplexAckParameters")
        if kind is None or not mux or mux[0] != "h2250LogicalChannelAckParameters":
            # Some terminals send an informational ACK we did not request.
            # It must not tear down an otherwise healthy call.
            return
        value = mux[1]
        channel = value.get("mediaChannel")
        if not channel:
            return
        try:
            remote = endpoint(channel, self.config.target_ip)
        except ProtocolError:
            # A multihomed terminal may advertise a different source IP.
            # Keep the pinned control-plane target and accept only its port.
            try:
                port = int(channel[1][1]["tsapIdentifier"])
            except (IndexError, KeyError, TypeError, ValueError):
                return
            if not 1 <= port <= 65535:
                return
            remote = (self.config.target_ip, port)
        if kind == "audio":
            self.remote_audio = remote
            self.audio_tx_pt = 0
        else:
            pt = value.get("dynamicRTPPayloadType", 96)
            if not 96 <= pt <= 127:
                return
            self.video_tx_pt = pt
            self.remote_video = remote
        emit("channel", direction="outgoing", kind=kind, remote_ip=remote[0], remote_port=remote[1], payload_type=self.audio_tx_pt if kind == "audio" else self.video_tx_pt)

    def on_control(self, payload):
        category, (name, params) = decode(asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage, payload)
        self.wire("received", "h245", payload, category=category, name=name)
        now = time.monotonic()
        if now - self.request_window >= 1:
            self.request_window, self.received_requests = now, 0
        self.received_requests += 1
        if self.received_requests > 500:
            raise ProtocolError("Excessive H.245 message rate")
        if category == "request":
            if name == "terminalCapabilitySet":
                capabilities = [entry.get("capability") for entry in params.get("capabilityTable", [])]
                self.remote_h264 = any(cap and cap[0] in ("receiveVideoCapability", "receiveAndTransmitVideoCapability") and codec_kind(("videoData", cap[1]))[0] == "video" for cap in capabilities)
                self.remote_tcs_seen = True
                self.remote_video_known = True
                self.send_control(("response", ("terminalCapabilitySetAck", {"sequenceNumber": params["sequenceNumber"]})))
                emit("capabilities", h264=self.remote_h264, entries=len(capabilities))
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
                self.send_control(("response", ("closeLogicalChannelAck", {"forwardLogicalChannelNumber": params["forwardLogicalChannelNumber"]})))
                self.reason, self.stopping = "remote_channel_closed", True
        elif category == "response":
            if name == "terminalCapabilitySetAck" and params["sequenceNumber"] == 1:
                self.tcs_acked = True
            elif name == "terminalCapabilitySetReject":
                raise ProtocolError("Peer rejected H.245 capabilities")
            elif name == "masterSlaveDeterminationAck":
                self.master_determined = True
                self.is_master = params["decision"][0] == "master"
            elif name == "masterSlaveDeterminationReject":
                raise ProtocolError("Peer rejected master/slave determination")
            elif name == "openLogicalChannelAck":
                self.on_ack(params)
            elif name == "openLogicalChannelReject":
                kind = self.requested_channels.get(params["forwardLogicalChannelNumber"])
                if kind == "audio":
                    raise ProtocolError("Peer rejected G.711 audio channel")
                if kind != "video":
                    return
                self.requested_video_rejected = True
                emit("video_unavailable", reason="peer_rejected_h264")
        elif category == "command" and name == "endSessionCommand":
            self.reason, self.stopping = "remote_end_session", True
        self.open_channels()
        self.maybe_connected()

    def maybe_connected(self):
        if self.connected or not (self.signal_connected and self.tcs_acked and self.master_determined and self.incoming_audio and self.remote_audio):
            return
        video_requested = self.config.video_port is not None
        if video_requested and self.remote_h264 and not self.requested_video_rejected and not (self.incoming_video and self.remote_video):
            # A camera-off peer advertises H.264 but never opens its incoming
            # video channel. Do not lose a working audio call to that.
            if self.audio_ready_at is None:
                self.audio_ready_at = time.monotonic()
            if time.monotonic() - self.audio_ready_at < 2.5:
                return
            self.requested_video_rejected = True
            emit("video_unavailable", reason="no_incoming_video")
        self.connected = True
        emit("connected", call_id=self.config.call_id, call_ref=self.config.call_ref,
             remote_ip=self.remote_audio[0], remote_audio_port=self.remote_audio[1],
             remote_audio_payload_type=self.audio_tx_pt, local_audio_payload_type=self.audio_rx_pt,
             remote_video_ip=self.remote_video[0] if self.remote_video else None,
             remote_video_port=self.remote_video[1] if self.remote_video else None,
             remote_video_payload_type=self.video_tx_pt, local_video_payload_type=self.video_rx_pt,
             video_status="connected" if self.remote_video and self.incoming_video else "unsupported" if video_requested else "not_requested")

    def frames(self, sock):
        data = sock.recv(MAX_FRAME)
        if not data:
            self.reason, self.stopping = ("remote_control_closed" if sock is self.control else "remote_signal_closed"), True
            return
        buffer = self.buffers[sock] + data
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
                self.on_control(payload)
        if len(buffer) > MAX_FRAME:
            raise ProtocolError("TPKT receive buffer exceeded bound")
        self.buffers[sock] = buffer

    def commands(self):
        data = os.read(sys.stdin.fileno(), MAX_COMMAND)
        if not data:
            self.reason, self.stopping = "parent_pipe_closed", True
            return
        self.command_buffer += data
        if len(self.command_buffer) > MAX_COMMAND:
            raise ProtocolError("IPC command exceeded bound")
        while b"\n" in self.command_buffer:
            line, self.command_buffer = self.command_buffer.split(b"\n", 1)
            value = json.loads(line)
            if value.get("command") == "hangup":
                self.reason, self.stopping = "local_hangup", True
            else:
                raise ProtocolError("Unsupported sidecar command")

    def close(self):
        # Limit shutdown latency independently of setup timeout and parent IO.
        if self.control is not None:
            try:
                self.control.settimeout(.5)
                self.send_control(("command", ("endSessionCommand", ("disconnect", 0))))
            except Exception:
                pass
            self.control.close()
        if self.signal is not None:
            try:
                self.signal.settimeout(.5)
                params = {"protocolIdentifier": (0, 0, 8, 2250, 0, 4), "callIdentifier": {"guid": uuid.UUID(self.config.call_id).bytes}}
                encoded = encode(asn.H323_MESSAGES.H323_UserInformation, {"h323-uu-pdu": {"h323-message-body": ("releaseComplete", params), "h245Tunneling": False}})
                packet = b"\x08\x02" + self.config.call_ref.to_bytes(2, "big") + b"\x5a\x08\x02\x80\x90\x7e" + (len(encoded) + 1).to_bytes(2, "big") + b"\x05" + encoded
                self.signal.sendall(tpkt(packet))
                self.wire("sent", "q931", packet, name="releaseComplete")
            except Exception:
                pass
            self.signal.close()
        emit("disconnected", reason=self.reason, was_connected=self.connected)

    def run(self):
        try:
            self.signal = socket.create_connection((self.config.target_ip, self.config.target_port), timeout=min(5, self.config.timeout))
            self.signal.settimeout(2)
            self.buffers[self.signal] = b""
            self.send_setup()
            while not self.stopping:
                if not self.connected and time.monotonic() >= self.deadline:
                    raise TimeoutError(
                        "H.323 setup/media negotiation timed out "
                        f"(signal={self.signal_connected} tcs={self.tcs_acked} "
                        f"master={self.master_determined} audio_in={self.incoming_audio} "
                        f"audio_out={bool(self.remote_audio)} video_in={self.incoming_video} "
                        f"video_out={bool(self.remote_video)} video_rejected={self.requested_video_rejected})"
                    )
                sockets = list(self.buffers)
                ready, _, _ = select.select(sockets + [sys.stdin.fileno()], [], [], .2)
                for sock in ready:
                    if sock == sys.stdin.fileno():
                        self.commands()
                    else:
                        self.frames(sock)
                    if self.stopping:
                        break
                self.maybe_connected()
                # Parent lifetime is tracked through stdin EOF; the gateway
                # watchdog handles media death independently.
        except Exception as exc:
            self.reason = f"{type(exc).__name__}: {str(exc)[:400]}"
            emit("failed", reason=self.reason)
        finally:
            self.close()


def main():
    # Initial configuration is bounded and consumed before select-based IPC.
    # Reading through BufferedReader here could prefetch a following hangup,
    # hiding it from the file descriptor subsequently monitored by select().
    line = b""
    while len(line) <= MAX_COMMAND and not line.endswith(b"\n"):
        value = os.read(sys.stdin.fileno(), 1)
        if not value:
            break
        line += value
    if not line.endswith(b"\n") or len(line) > MAX_COMMAND:
        raise ProtocolError("Missing or oversized initial configuration")
    configuration = Configuration.load(json.loads(line))
    call = Call(configuration)
    def stop_requested(_number, _frame):
        call.reason, call.stopping = "parent_terminated", True
    signal.signal(signal.SIGTERM, stop_requested)
    signal.signal(signal.SIGINT, stop_requested)
    call.run()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        emit("failed", reason=f"{type(error).__name__}: {str(error)[:400]}")
        raise SystemExit(1)

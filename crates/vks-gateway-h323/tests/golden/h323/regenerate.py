#!/usr/bin/env python3
"""Regenerate the H.323 golden vectors from the pinned pycrate runtime.

The Polycom Q.931 frames and the H.245 OLC/OLC-Ack frames are byte-for-byte
copies of the independent wire captures in ``python/test_h323_sidecar.py``.
They are not synthesized: keep them unchanged unless a newer capture replaces
them with provenance in the commit message.

The H.235/H.245-security fixtures are deterministic aligned-PER encodings made
with pycrate 0.8.1 from the ASN.1 modules shipped in ``python/schemas``. They
exist to prove fail-closed behavior: T16 never negotiates encrypted media and
must reject these messages instead of falling back to plaintext.

Usage:
    ~/.local/share/vks-mcu/h323-venv/bin/python3 regenerate.py
"""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "python"))

from h323_listener import asn, encode  # noqa: E402

HERE = Path(__file__).resolve().parent

CAPTURED = {
    "polycom_alerting.tpkt": "0802b157017e0067052380060008914a000522c0b500233127506f6c79636f6d205265616c50726573656e6365204465736b746f7020666f722057696e646f77730a332e352e302e353935383201b0d8001100000102030405060708090a0b0c0d0e0f010001800100014010800100",
    "polycom_connect.tpkt": "0802b15707280770726f73746f727e007e0522c0060008914a000500c0a801840cad22c0b500233127506f6c79636f6d205265616c50726573656e6365204465736b746f7020666f722057696e646f77730a332e352e302e353935383200000102030405060708090a0b0c0d0e0f1d0cc01100000102030405060708090a0b0c0d0e0f010001800100014010800100",
    "polycom_release_adaptive_busy.tpkt": "0802b1575a7e00270525c0060008914a00054888c01100000102030405060708090a0b0c0d0e0f0100014010800100",
    "h245_incoming_olc.bin": "030000000c6027800a04000100c0a801840c9f",
    "h245_outgoing_ack.bin": "22c000000680101c0000c0a801840c9e00c0a801840c9f",
    "h245_incoming_video_olc.bin": "030000010a001d60000700088171000001400c800402914002a2001d003200180062001a801804600200c0a801840ca16c000b50080008817100000001da",
    "h245_outgoing_video_ack.bin": "22c000010680101c0200c0a801840ca000c0a801840ca1",
}

BASE_SETUP = {
    "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
    "sourceInfo": {"terminal": {}, "mc": False, "undefinedNode": False},
    "sourceAddress": [("h323-ID", "Polycom")],
    "destinationAddress": [("h323-ID", "room")],
    "activeMC": False,
    "conferenceID": bytes(range(16)),
    "conferenceGoal": ("create", 0),
    "callType": ("pointToPoint", 0),
    "callIdentifier": {"guid": bytes(range(16))},
    "mediaWaitForConnect": False,
    "canOverlapSend": False,
    "multipleCalls": False,
    "maintainConnection": False,
}


def uuie(message_body):
    body = encode(asn.H323_MESSAGES.H323_UserInformation, {
        "h323-uu-pdu": {"h323-message-body": message_body, "h245Tunneling": False}
    })
    return b"\x7e" + (len(body) + 1).to_bytes(2, "big") + b"\x05" + body


def q931(message_type, body, ref=0x1234, cause=None):
    payload = b"\x08\x02" + ref.to_bytes(2, "big") + bytes([message_type])
    if cause is not None:
        payload += b"\x08\x02\x80" + bytes([cause])
    return b"\x03\x00" + (len(payload) + len(body) + 4).to_bytes(2, "big") + payload + body


def setup(extra):
    return uuie(("setup", {**BASE_SETUP, **extra}))


def write(name, data):
    (HERE / name).write_bytes(data)


def main():
    for name, value in CAPTURED.items():
        raw = bytes.fromhex(value)
        if name.startswith("polycom_"):
            raw = b"\x03\x00" + (len(raw) + 4).to_bytes(2, "big") + raw
        write(name, raw)

    caps = {
        "encryption": ("default", 0),
        "authenticaton": ("default", 0),
        "integrity": ("default", 0),
    }
    write("h225_setup_cleartext.tpkt", q931(0x05, setup({"h245SecurityCapability": [("noSecurity", 0)]})))
    write("h225_setup_h235_token.tpkt", q931(0x05, setup({"tokens": [{"tokenOID": (0, 0, 8, 235, 0, 0, 1)}]})))
    write("h225_setup_tls_only.tpkt", q931(0x05, setup({"h245SecurityCapability": [("tls", caps)]})))
    write("h225_connect_tls.tpkt", q931(0x07, uuie(("connect", {
        "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
        "h245Address": ("ipAddress", {"ip": b"\xc0\xa8\x01\x84", "port": 3245}),
        "destinationInfo": {"mcu": {}, "mc": True, "undefinedNode": False},
        "conferenceID": bytes(range(16)),
        "callIdentifier": {"guid": bytes(range(16))},
        "h245SecurityMode": ("tls", caps),
        "multipleCalls": False,
        "maintainConnection": False,
    }))))
    write("h225_release_security_denied.tpkt", q931(0x5a, uuie(("releaseComplete", {
        "protocolIdentifier": (0, 0, 8, 2250, 0, 4),
        "callIdentifier": {"guid": bytes(range(16))},
        "reason": ("securityDenied", 0),
    })), cause=16))

    olc = {
        "forwardLogicalChannelNumber": 1,
        "forwardLogicalChannelParameters": {
            "dataType": ("audioData", ("g711Ulaw64k", 20)),
            "multiplexParameters": ("h2250LogicalChannelParameters", {"sessionID": 1}),
        },
        "encryptionSync": {"synchFlag": 96, "h235Key": b"key"},
    }
    write("h245_open_encryption_sync.bin", encode(
        asn.MULTIMEDIA_SYSTEM_CONTROL.MultimediaSystemControlMessage,
        ("request", ("openLogicalChannel", olc)),
    ))


if __name__ == "__main__":
    main()

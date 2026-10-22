"""H.323 native audio codec negotiation shared by the outbound sidecar and UAS.

Only codecs whose RTP framing is implemented by the Rust media bridge are
listed. G.722 has priority (wideband), then G.711U, G.711A and G.723.1.
Every codec uses its static H.245 payload type; dynamic audio PTs are
deliberately unsupported. G.728 is deferred until its H.245 frame count is
verified against the bridge's 40-byte/20-ms packet.
"""

AUDIO_CODECS = {
    "g722-64k": ("G722", 9),
    "g711Ulaw64k": ("G711U", 0),
    "g711Alaw64k": ("G711A", 8),
    "g7231": ("G723", 4),
}
AUDIO_PREFERENCE = ("G722", "G711U", "G711A", "G723")


def select_audio_candidates(peer_rx, peer_tx):
    """Symmetric native codecs in preference order."""
    usable = set(peer_rx) & set(peer_tx)
    return [codec for codec in AUDIO_PREFERENCE if codec in usable]


def audio_capabilities_from_tcs(params):
    """Return the symmetric native audio candidates from an H.245 TCS request."""
    referenced = set()
    for descriptor in params.get("capabilityDescriptors", []):
        for alternative in descriptor.get("simultaneousCapabilities", []):
            referenced.update(alternative)
    peer_rx, peer_tx = set(), set()
    for entry in params.get("capabilityTable", []):
        if referenced and entry.get("capabilityTableEntryNumber") not in referenced:
            continue
        capability = entry.get("capability")
        if not capability or capability[0] not in (
            "receiveAudioCapability",
            "transmitAudioCapability",
            "receiveAndTransmitAudioCapability",
        ):
            continue
        payload = capability[1]
        if not isinstance(payload, tuple) or not payload or payload[0] not in AUDIO_CODECS:
            continue
        codec = AUDIO_CODECS[payload[0]][0]
        if capability[0] in ("receiveAudioCapability", "receiveAndTransmitAudioCapability"):
            peer_rx.add(codec)
        if capability[0] in ("transmitAudioCapability", "receiveAndTransmitAudioCapability"):
            if codec == "G723" and not peer_transmits_g723(payload):
                continue
            peer_tx.add(codec)
    return select_audio_candidates(peer_rx, peer_tx)


def can_adopt_audio_codec(codec, candidates, selected, remote_audio_is_set):
    """A peer may pick only a codec we both advertised and the bridge can use."""
    return codec in candidates and (codec == selected or not remote_audio_is_set)


def audio_choice(codec):
    """H.245 AudioCapability CHOICE for a native codec (also used in OLC)."""
    if codec == "G723":
        return ("g7231", {"maxAl-sduAudioFrames": 1, "silenceSuppression": False})
    for key, (name, _) in AUDIO_CODECS.items():
        if name == codec:
            return (key, 20)
    raise KeyError(codec)


def audio_capability(codec):
    return ("receiveAndTransmitAudioCapability", audio_choice(codec))


def audio_data_type(codec):
    return ("audioData", audio_choice(codec))


def audio_capability_entries():
    """(entry number, H.245 capability) in the advertised preference order."""
    return [(index + 1, audio_capability(codec)) for index, codec in enumerate(AUDIO_PREFERENCE)]


def next_audio_candidate(candidates, rejected):
    try:
        index = candidates.index(rejected) + 1
    except ValueError:
        return None
    return candidates[index] if index < len(candidates) else None


def audio_payload_type(codec):
    for _, (name, payload_type) in AUDIO_CODECS.items():
        if name == codec:
            return payload_type
    raise KeyError(codec)


def audio_codec_for_payload(payload_type):
    for _, (name, pt) in AUDIO_CODECS.items():
        if pt == payload_type:
            return name
    return None


def peer_transmits_g723(choice):
    """G.723.1 is only safe when the peer sends one 30 ms SDU per packet."""
    if not isinstance(choice, tuple) or choice[0] != "g7231" or not isinstance(choice[1], dict):
        return False
    return choice[1].get("maxAl-sduAudioFrames") == 1

#!/usr/bin/env python3
"""Fail-closed drift detector for the PNM vendor bundle (T11b D1).

The original T11b plan assumed the PNM vendor bundle contained hand-written
SDP code whose 1+7 mids could be patched in place. It does not: the bundle
never calls the WebRTC SDP API, and inventing vendor anchors for a client that
does not exist would rot silently. This guard therefore does nothing today and
fails the build the moment the vendor bundle starts carrying SDP APIs — at
which point the per-source mid policy (direction-aware offer/answer, one mid
per publisher) must be re-validated against the real client before landing.

Exit codes:
  0  clean (no SDP API in the vendor bundle)
  1  drift (an SDP API appeared) — the CI step fails
  2  layout problem (vendor bundle directory is missing)
"""
from __future__ import annotations

import argparse
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
VENDOR_REL = "client/pnm-frontend/deployed/pnm"

# Pattern -> why it matters for T11b's per-source mid policy.
#
# Bare `RTCPeerConnection` is deliberately NOT tracked: the vendor bundle uses
# it only for feature detection (`"RTCPeerConnection" in window`). Drift means
# the bundle actually builds a peer connection / speaks SDP.
PATTERNS = {
    "new RTCPeerConnection": "peer-connection construction",
    "RTCPeerConnection(": "peer-connection construction",
    "createOffer": "offer creation",
    "setLocalDescription": "local description",
    "addTransceiver": "transceiver pre-offer (T11b 1+(N-1) mids)",
    "a=mid:": "raw SDP mid",
}
TEXT_SUFFIXES = {".js", ".mjs", ".html", ".htm", ".json"}


def scan(root: pathlib.Path) -> list[str]:
    """Return human-readable drift findings under ``root``'s vendor bundle."""
    vendor = root / VENDOR_REL
    findings: list[str] = []
    for path in sorted(vendor.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in TEXT_SUFFIXES:
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError as error:  # unreadable file is a drift risk too
            findings.append(f"{path.relative_to(root)}: unreadable ({error})")
            continue
        for pattern, why in PATTERNS.items():
            if pattern in text:
                findings.append(f"{path.relative_to(root)}: contains {pattern!r} ({why})")
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="PNM vendor WebRTC SDP drift check (T11b D1)")
    parser.add_argument("--root", type=pathlib.Path, default=ROOT, help="repository root")
    parser.add_argument(
        "--check",
        action="store_true",
        help="report only; nothing is mutated (accepted for symmetry with the vendor patch scripts)",
    )
    args = parser.parse_args(argv)
    vendor = args.root / VENDOR_REL
    if not vendor.is_dir():
        print(
            f"PNM WebRTC drift check: vendor bundle not found at {VENDOR_REL} (layout changed?)",
            file=sys.stderr,
        )
        return 2
    text_files = sum(
        1 for path in vendor.rglob("*") if path.is_file() and path.suffix.lower() in TEXT_SUFFIXES
    )
    findings = scan(args.root)
    if findings:
        print("PNM WebRTC drift check FAILED: the vendor bundle now contains SDP APIs", file=sys.stderr)
        for finding in findings:
            print(f"  - {finding}", file=sys.stderr)
        print(
            "  Re-validate the T11b per-source mid policy against the real client before landing.",
            file=sys.stderr,
        )
        return 1
    print(f"PNM WebRTC drift check ok: {text_files} text files, no SDP API in {VENDOR_REL}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

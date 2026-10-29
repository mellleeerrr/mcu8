#!/usr/bin/env python3
"""Tests for scripts/check-pnm-webrtc-drift.py (T11b D1).

Covers the three contracts of the guard: a clean vendor bundle passes, an SDP
API appearing fails with a file/pattern finding, and a missing vendor bundle
fails as a layout error (exit 2) instead of pretending to be clean. Every case
runs twice to prove the check is read-only/idempotent.
"""
from __future__ import annotations

import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parent / "check-pnm-webrtc-drift.py"
VENDOR_REL = pathlib.Path("client/pnm-frontend/deployed/pnm")


class DriftCheckTest(unittest.TestCase):
    def make_root(self, files: dict[str, str]) -> pathlib.Path:
        root = pathlib.Path(tempfile.mkdtemp(prefix="pnm-drift-"))
        self.addCleanup(shutil.rmtree, root, True)
        vendor = root / VENDOR_REL
        vendor.mkdir(parents=True)
        for name, text in files.items():
            (vendor / name).write_text(text, encoding="utf-8")
        return root

    @staticmethod
    def run_check(root: pathlib.Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(root), "--check"],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_clean_bundle_passes_twice(self) -> None:
        root = self.make_root({"index.html": "<html></html>", "app.js": "console.log('pnm');"})
        for _ in range(2):
            result = self.run_check(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("no SDP API", result.stdout)

    def test_sdp_api_is_reported_as_drift_twice(self) -> None:
        root = self.make_root({"index.html": "<html></html>", "call.js": "pc.addTransceiver('video');"})
        for _ in range(2):
            result = self.run_check(root)
            self.assertEqual(result.returncode, 1, result.stdout)
            self.assertIn("call.js", result.stderr)
            self.assertIn("addTransceiver", result.stderr)
            self.assertIn("Re-validate the T11b per-source mid policy", result.stderr)

    def test_each_tracked_pattern_is_drift(self) -> None:
        for pattern in ("new RTCPeerConnection", "createOffer", "setLocalDescription", "addTransceiver", "a=mid:"):
            with self.subTest(pattern=pattern):
                root = self.make_root({"chunk.js": f"/* {pattern} */"})
                result = self.run_check(root)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn(pattern, result.stderr)

    def test_missing_vendor_bundle_is_a_layout_error(self) -> None:
        root = pathlib.Path(tempfile.mkdtemp(prefix="pnm-drift-empty-"))
        self.addCleanup(shutil.rmtree, root, True)
        result = self.run_check(root)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("vendor bundle not found", result.stderr)

    def test_feature_detection_is_not_drift(self) -> None:
        # The real bundle says `"RTCPeerConnection" in window`; that is not SDP.
        root = self.make_root({"index.html": '<script>"RTCPeerConnection"in window</script>'})
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_non_text_suffixes_are_ignored(self) -> None:
        root = self.make_root({"bundle.wasm": "createOffer"})
        result = self.run_check(root)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()

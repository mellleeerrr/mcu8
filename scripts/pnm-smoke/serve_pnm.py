#!/usr/bin/env python3
"""Smoke-test static server for the vendored PNM frontend.

Maps:
  /pnm/*  -> deployed/pnm/* (SPA fallback to index.html)
  /mcu/*  -> deployed/pnm/*
Used only to verify the compiled bundle boots without missing assets.
"""
import json
import os
import posixpath
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.environ.get("PNM_ROOT", "client/pnm-frontend/deployed/pnm")

API_STUB = {
    "/api/v3/system/about": {
        "node_id": "vks-smoke-node",
        "server_name": "VKS-MCU",
        "versions": {"server": "0.1.0-smoke"},
        "state": {"status": "READY", "payload": {"package": "", "backup": "", "export": ""}},
    },
    "/api/v3/references": {
        "key": "", "index": "", "buy": "", "contacts": "",
        "trial_full_version": "", "license_renewal": "", "free_license_restrictions": "",
    },
    "/api/v3/login/status": {"status": "enabled"},
    "/api/v3/locales": {"locales": [{"id": "en_US", "description": "English"}, {"id": "ru_RU", "description": "Русский"}]},
    "/api/v3/system/time": {"time": 0, "timezone": "UTC"},
    "/api/v3/system/timezone": {"id": "UTC"},
    "/api/v3/settings/audio_codecs": {"audio_codecs": []},
    "/api/v3/settings/video_codecs": {"video_codecs": []},
    "/api/v3/time-zones": {"time_zones": [{"id": "UTC"}]},
    "/api/v3/abonents/stop_reasons": {},
    "/api/v3/ui/system/status": {
        "engine": {"connected": True, "description": "OK"},
        "database": {"connected": True},
        "network": {"editable": True},
        "disk": {"videorecords": 0, "free": "100G", "percent": 5, "full": "NORMAL"},
        "sip": {"account": False, "registration": False},
        "h323": {"account": False, "registration": False},
    },
    "/api/v3/profile": {
        "user": {
            "id": "1", "display_name": "Admin", "permissions": ["all"],
            "locale_id": "ru_RU", "time_zone_id": "UTC", "groups": [],
            "emails": [], "phones": [], "avatars": [], "description": "",
            "creator_id": "", "created_at": 0, "busy": False,
            "defaults": {"deletable": False, "editable": True},
        }
    },
    "/api/v3/features": {"features": {}},
    "/api/v3/license": {
        "state": "VALID", "type": "PERMANENT", "count": 100, "serial_id": "stub",
        "demo": False, "first_expired": 0, "last_expired": 0, "last_update_failed": False,
    },
    "/api/v3/sessions": {"sessions": [], "count": 0},
    "/api/v3/conferences": {"conferences": [], "count": 0},
    "/api/v3/abonents/count": {"count": 0},
    "/api/v3/conferences/status": {"status": "enabled"},
}

MIME = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".webmanifest": "application/manifest+json",
    ".xml": "application/xml; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".ico": "image/x-icon",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
    ".ttf": "font/ttf",
    ".webm": "video/webm",
    ".txt": "text/plain; charset=utf-8",
}


def safe_path(url_path: str) -> str | None:
    # Normalize and refuse traversal outside ROOT.
    norm = posixpath.normpath(url_path)
    if norm.startswith("/"):
        norm = norm[1:]
    parts = [p for p in norm.split("/") if p not in ("", ".")]
    if any(p == ".." for p in parts):
        return None
    return os.path.join(ROOT, *parts)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # keep quiet
        sys.stderr.write("%s\n" % (fmt % args))

    def _send(self, code, body: bytes, ctype: str):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _resolve(self, url_path: str):
        url_path = url_path.split("?", 1)[0]
        if url_path.startswith("/pnm"):
            rel = url_path[len("/pnm"):] or "/"
        elif url_path.startswith("/mcu"):
            rel = url_path[len("/mcu"):] or "/"
        else:
            return None
        return safe_path(rel)

    def do_HEAD(self):
        self.do_GET()

    def do_POST(self):
        url = self.path.split("?", 1)[0]
        length = int(self.headers.get("Content-Length", "0") or 0)
        body = self.rfile.read(length) if length else b"{}"
        if url == "/api/v3/login":
            token = json.dumps({"token": "stub-token-1"}).encode()
            return self._send(200, token, "application/json; charset=utf-8")
        err = json.dumps({"error": {"description": "not implemented", "code": 404}}).encode()
        return self._send(404, err, "application/json; charset=utf-8")

    def do_GET(self):
        url = self.path.split("?", 1)[0]
        if url.startswith("/api/v3/"):
            if url in API_STUB:
                body = json.dumps(API_STUB[url]).encode()
                return self._send(200, body, "application/json; charset=utf-8")
            err = json.dumps({"error": {"description": "not implemented", "code": 404}}).encode()
            return self._send(404, err, "application/json; charset=utf-8")
        path = self._resolve(self.path)
        if path is None:
            return self._send(404, b"not found", "text/plain")
        if os.path.isdir(path):
            path = os.path.join(path, "index.html")
        if os.path.isfile(path):
            with open(path, "rb") as f:
                body = f.read()
            ext = os.path.splitext(path)[1].lower()
            return self._send(200, body, MIME.get(ext, "application/octet-stream"))
        # SPA fallback only for /pnm routes (not assets)
        if self.path.startswith("/pnm"):
            idx = os.path.join(ROOT, "index.html")
            if os.path.isfile(idx):
                with open(idx, "rb") as f:
                    return self._send(200, f.read(), MIME[".html"])
        return self._send(404, b"not found", "text/plain")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8099
    srv = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"serving {ROOT} on http://127.0.0.1:{port}/pnm/", flush=True)
    srv.serve_forever()

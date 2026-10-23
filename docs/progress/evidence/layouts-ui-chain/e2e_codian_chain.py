#!/usr/bin/env python3
"""E2E: цепочка Codian-раскладки на свежем инстансе (порт 8081).

Проверяет живой HTTP-контракт до старта сессии:
  каталог (65+54, slots) -> calculate (codian) -> create/attach/PATCH раскладки
  -> mode-эндпоинт (пресет не подменяется, size из slot_count) -> старт/стоп
  сессии (gateway получает codian-геометрию).
"""
import json
import sys
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8081"
TOKEN = ""


def call(method, path, body=None, token=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Accept", "application/json")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    auth = token if token is not None else TOKEN
    if auth:
        req.add_header("Authorization", "Bearer " + auth)
    try:
        with urllib.request.urlopen(req, timeout=30) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as error:
        raw = error.read()
        try:
            return error.code, json.loads(raw) if raw else None
        except json.JSONDecodeError:
            return error.code, raw.decode(errors="replace")


def check(label, condition, detail=""):
    print(("PASS  " if condition else "FAIL  ") + label + ((" :: " + str(detail)) if detail else ""))
    if not condition:
        global failed
        failed = True


failed = False

status, data = call("POST", "/api/v3/login", {"login": "admin", "password": "admin_password"})
TOKEN = (data or {}).get("token", "")
check("login 200 + token", status == 200 and bool(TOKEN), status)

status, data = call("GET", "/api/v3/ui/video_layouts/presets")
presets = (data or {}).get("presets", [])
codian = [p for p in presets if p.get("source") == "codian"]
vendor = [p for p in presets if p.get("source") != "codian"]
check("каталог 200", status == 200, status)
check("каталог: 65 вендорских + 54 codian", len(vendor) == 65 and len(codian) == 54, (len(vendor), len(codian)))
entry = next((p for p in codian if p["id"] == "codian_07"), None)
check("codian_07 в каталоге", entry is not None and entry["windows"] == 13, entry)
check("каталог несёт геометрию slots", entry is not None and len(entry["slots"]) == 13,
      None if entry is None else len(entry["slots"]))
check("слоты каталога 1-based", entry is not None and entry["slots"][0]["position"] == 1, None if entry is None else entry["slots"][0])

# Тот же набор окон должен отдавать calculate — два источника согласованы.
status, data = call("POST", "/api/v3/ui/video_layouts/calculate", {"preset": "codian_07", "width": 1280, "height": 720})
slots = (data or {}).get("slots", [])
check("calculate codian_07 = 200", status == 200, status)
check("calculate: 13 окон", len(slots) == 13, len(slots))
if slots and entry:
    same = all(
        abs(float(a["x"]) - float(b["x"])) < 1e-6
        and abs(float(a["width"]) - float(b["width"])) < 1e-6
        for a, b in zip(slots, entry["slots"])
    )
    check("calculate согласован с каталогом", same)
status, data = call("POST", "/api/v3/ui/video_layouts/calculate", {"preset": "codian_99", "width": 1280, "height": 720})
check("calculate: неизвестный codian -> 400", status == 400, status)

import time

suffix = "%d" % int(time.time())
status, data = call("POST", "/api/v3/ui/conferences", {"display_name": "Codian E2E " + suffix})
conference = (data or {}).get("conference") or (data or {})
cid = conference.get("id") or (data or {}).get("id")
check("создана конференция", status in (200, 201) and bool(cid), (status, list((data or {}).keys())))

payload = {
    "mode": "manual",
    "preset": "codian_43",
    "type": "individual",
    "size": len(next(p for p in codian if p["id"] == "codian_43")["slots"]),
    "slots": [{"position": i + 1, "role": "AUTO"} for i in range(len(next(p for p in codian if p["id"] == "codian_43")["slots"]))],
}
status, data = call("POST", "/api/v3/ui/video_layouts", {"display_name": "Codian 43 (E2E)", "payload": payload})
layout = (data or {}).get("video_layout") or (data or {})
lid = layout.get("id") or (data or {}).get("id")
check("создана раскладка codian_43", status in (200, 201) and bool(lid), status)
check("payload.preset сохранён как codian_43 (L-3)", (layout.get("payload") or {}).get("preset") == "codian_43",
      (layout.get("payload") or {}).get("preset"))

status, _ = call("POST", "/api/v3/conferences/%s/video_layouts/%s" % (cid, lid))
check("attach раскладки к конференции", status in (200, 201, 204), status)

status, data = call("GET", "/api/v3/ui/conferences/%s/video_layouts" % cid)
items = (data or {}).get("video_layouts") or (data or {}).get("items") or []
found = next((item for item in items if (item.get("id") or item.get("resource_id")) == lid), None)
check("раскладка видна в списке конференции", found is not None, [i.get("id") for i in items])
check("список отдаёт codian-пресет (L-3)", found is not None and (found.get("payload") or {}).get("preset") == "codian_43",
      None if found is None else (found.get("payload") or {}).get("preset"))

# Путь проектного слоя: выбор другой схемы в палитре -> сохранение шаблона (PATCH).
entry07 = next(p for p in codian if p["id"] == "codian_07")
patched = {
    "mode": "manual",
    "preset": "codian_07",
    "type": "individual",
    "size": len(entry07["slots"]),
    "slots": [{"position": i + 1, "role": "AUTO"} for i in range(len(entry07["slots"]))],
}
status, data = call("PATCH", "/api/v3/ui/conferences/%s/video_layouts/%s" % (cid, lid), {"payload": patched})
check("PATCH раскладки 200", status in (200, 204), (status, data if isinstance(data, str) else None))
status, data = call("GET", "/api/v3/ui/conferences/%s/video_layouts" % cid)
items = (data or {}).get("video_layouts") or (data or {}).get("items") or []
found = next((item for item in items if (item.get("id") or item.get("resource_id")) == lid), None)
stored = (found or {}).get("payload") or {}
check("PATCH сохранил codian_07 и size=13 (L-3)", stored.get("preset") == "codian_07" and stored.get("size") == 13,
      (stored.get("preset"), stored.get("size")))

# Живой mode-эндпоинт: сессия + смена режима раскладки.
status, data = call("POST", "/api/v3/sessions", {"conference_id": cid})
session = (data or {}).get("session") or {}
sid = session.get("id") or (data or {}).get("id")
check("сессия запущена", status in (200, 201) and bool(sid), (status, (data or {}).get("error") if isinstance(data, dict) else None))
if sid:
    status, data = call(
        "POST",
        "/api/v3/ui/video_layouts/mode",
        {"session_id": sid, "video_layout_id": lid, "new_mode": "manual", "payload": patched},
    )
    mode_layout = (data or {}).get("video_layout") or (data or {})
    mode_payload = mode_layout.get("payload") or {}
    check("mode-эндпоинт 200", status in (200, 201), status)
    check("mode: пресет не подменён (L-3)", mode_payload.get("preset") == "codian_07", mode_payload.get("preset"))
    check("mode: size из slot_count = 13 (L-3)", mode_payload.get("size") == 13, mode_payload.get("size"))
    call("DELETE", "/api/v3/sessions/%s" % sid)

print()
print("RESULT: " + ("ALL PASS" if not failed else "FAILURES ABOVE"))
sys.exit(1 if failed else 0)

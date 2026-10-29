# Production deployment (vks-server)

## Files

- `systemd/vks-mcu-prod.service` — hardened system-level unit, `DynamicUser=yes`.
- `systemd/vks-mcu.service` — dev-only user unit for a local stand (`%h/mcu`);
  machine-specific values (`VKS_CANDIDATE_ADDR`, allowlist) задаются
  `systemctl --user set-environment` или drop-in, а не в репозитории.
- `vks.env.example` — secrets template; copy to `/etc/vks/vks.env` mode `0600`
  (owner `root`, readable by systemd before privileges drop).
- `../configs/prod.example.toml` — copy to `/etc/vks/server.toml`, mode `0600`.

## Layout

```
/opt/vks/bin/vks-server          # release binary
/opt/vks/frontend/pnm/           # deployed PNM UI; set frontend_dir in TOML
/opt/vks/h323-venv/bin/python3   # optional H.323 sidecar venv; set VKS_H323_PYTHON
/etc/vks/server.toml             # config, allow_insecure_defaults = false
/etc/vks/vks.env                 # VKS_JWT_SECRET, VKS_ADMIN_PASSWORD, ...
/var/lib/vks/                    # StateDirectory; SQLite database here if used
/var/log/vks/                    # LogsDirectory
```

`ProtectSystem=strict` makes `/opt/vks` and `/etc/vks` read-only. Use an
absolute SQLite DSN (`sqlite:///var/lib/vks/vks_data.db?mode=rwc`) or a
PostgreSQL URL; a relative `vks_data.db` in `/opt/vks` will fail with EROFS.
`LoadCredential=server.toml:/etc/vks/server.toml` copies the config into the
service's credential directory, so `/etc/vks/server.toml` can stay `0600
root:root` while `DynamicUser=yes` still reads it.

Requires systemd >= 247 for `LoadCredential=` and the `%d` credential
directory (verified on systemd 259).

## Inbound H.323 UAS

`h323_listener.py` is installed next to the outbound sidecar. Inbound H.323 is
disabled by default (`VKS_H323_UAS_ENABLED=0` in the sample env). The single
source of truth for the variables is `vks.env.example`: set
`VKS_H323_UAS_ENABLED=1`, `VKS_H323_UAS_HELPER`, and an explicit narrow
`VKS_H323_UAS_ALLOW` (the sample defaults to loopback-only `127.0.0.1/32`;
widen it to your LAN CIDRs, never `0.0.0.0/0`). Limits:
`VKS_H323_UAS_MAX_CALLS` / `VKS_H323_UAS_MAX_PER_IP` /
`VKS_H323_UAS_MAX_CHILDREN`. Then edit `/etc/vks/vks.env` and restart. Open TCP
1720 only from those networks. The listener forks one child per call;
the Rust supervisor owns the process group and reaps/terminates the whole tree
on shutdown. H.323 is plaintext and unauthenticated: do not expose 1720 to the
Internet, use a firewall/allowlist, and keep room aliases/PINs private.

The local `systemd/vks-mcu.service` dev unit intentionally contains **no**
`VKS_H323_UAS_ALLOW`/limit variables (T8): they come only from the env file or a
drop-in, otherwise the unit would silently override the single source.

## Inbound SIP UAS

Inbound SIP (UDP 5060) is **off by default** (`VKS_SIP_UAS_ENABLED=0`): it
accepts unauthenticated plaintext INVITEs. Enabling it is an explicit
deployment decision:

```bash
VKS_SIP_UAS_ENABLED=1
VKS_SIP_UAS_BIND=0.0.0.0:5060   # сузьте до LAN-интерфейса
VKS_SIP_UAS_MAX_CALLS=64
```

**Release note:** у SIP UAS нет реализованного IP-allowlist и digest-auth
(в отличие от H.323 UAS); защита — firewall/NAT, лимит `VKS_SIP_UAS_MAX_CALLS`
и token bucket 20 burst / 10 rps на IP. Не выставляйте 5060 в Интернет;
`candidate_addr` анонсируется в SDP, TLS/регистрация не поддерживаются.

## WebRTC media

Real browser WebRTC (str0m, ICE-lite, DTLS-SRTP) is **on by default**; a
non-ICE offer is rejected instead of receiving fake DTLS credentials. The
legacy hand-built SDP answer is only a bisect/rollback escape hatch:

```bash
# explicit opt-out back to the legacy answer path
VKS_WEBRTC_TRANSPORT=0
# production sizing (every peer owns one UDP socket)
VKS_WEBRTC_PORT_MIN=43000
VKS_WEBRTC_PORT_MAX=43100
VKS_WEBRTC_MAX_PEERS_PER_ROOM=8
VKS_WEBRTC_MAX_PEERS=256
```

The effective concurrency is
`min(VKS_WEBRTC_MAX_PEERS_PER_ROOM per room, VKS_WEBRTC_MAX_PEERS process,
size of the UDP range)` because every peer owns one UDP socket. For a host
running R rooms, the per-room cap bounds each room while the process cap and
the UDP range bound `R × per-room`: keep both
`R × VKS_WEBRTC_MAX_PEERS_PER_ROOM` and `VKS_WEBRTC_MAX_PEERS` inside the
range you size (43000–43100 holds 101 sockets). Both caps are parsed
fail-closed: an empty/non-numeric/zero value aborts startup (exit 78) instead
of silently using a default. Startup additionally *warns* (it does not fail)
when the process cap exceeds the UDP range, when the per-room cap exceeds the
process cap, or when a cap is implausibly large — these values parse but can
never be honoured, so the effective cap is lower. A client that pre-offers the
full `1+(N-1)` shape can only be shown 8 sources per kind
(`MAX_AUDIO_MIDS`/`MAX_VIDEO_MIDS`); a per-room cap above 8 is allowed but
counts the invisible sources as `webrtc_sources{dropped_no_mid}`. A legacy 1+1
client keeps one deterministic publisher per kind with the same explicit
`dropped_no_mid` degradation — two publishers are never glued into one mid.

Auxiliary/secondary processes on the same host are also real WebRTC servers
after this default flip; pass `VKS_WEBRTC_TRANSPORT=0` (or distinct UDP
ports) to any process that must not serve media. `candidate_addr` must be the
address browsers can reach; startup preflight
fails with exit 78 if it is empty/loopback (unless
`VKS_ALLOW_LOOPBACK_MEDIA=1`) or if the port pair is invalid/unbindable. Open
the UDP range in the firewall and port-forward it 1:1 to the server (the
advertised ICE host candidate is the bound port). ICE-lite does not allocate
TURN relay candidates; use client-side TURN if needed. Signalling clients
connect to `/ws` with a Bearer token, or from a browser with
`?token=<JWT>`. A query token can leak through proxy access logs, browser
history or a Referer, so terminate TLS, keep the JWT TTL short, do not log query
strings at the proxy, and preserve `Host` for the Origin check (override with
`VKS_WS_ORIGIN_ALLOW=https://ui.example`). Today the only end-to-end client is
`tests/e2e/webrtc.js`; the PNM UI is not yet wired to this transport. Rollback: set
`VKS_WEBRTC_TRANSPORT=0` and restart (live calls are dropped).

## First start

```bash
sudo install -d -m 0755 /opt/vks /opt/vks/bin /opt/vks/frontend
sudo install -d -m 0755 /opt/vks/share/vks-mcu
sudo install -d -m 0750 /etc/vks
sudo install -m 0644 crates/vks-gateway-h323/python/h323_sidecar.py \
  crates/vks-gateway-h323/python/h323_listener.py \
  crates/vks-gateway-h323/python/h323_audio.py \
  crates/vks-gateway-h323/python/h323_asn.py /opt/vks/share/vks-mcu/
sudo install -m 0644 crates/vks-gateway-h323/python/requirements.lock /opt/vks/share/vks-mcu/
sudo install -m 0755 target/release/vks-server /opt/vks/bin/vks-server
sudo cp -a client/pnm-frontend/deployed/pnm /opt/vks/frontend/
sudo chmod -R a+rX /opt/vks/frontend
sudo install -m 0600 configs/prod.example.toml /etc/vks/server.toml
sudo install -m 0600 deploy/vks.env.example /etc/vks/vks.env
sudoedit /etc/vks/server.toml      # absolute database_url, candidate_addr, frontend_dir
sudoedit /etc/vks/vks.env          # >=32-byte JWT secret; password for first start
# ВАЖНО: VKS_* env перекрывает TOML. В vks.env.example намеренно нет
# database_url/candidate_addr/frontend_dir, чтобы источником истины оставался
# server.toml. Если добавите их в env — правьте именно env.
sudo install -m 0644 deploy/systemd/vks-mcu-prod.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemd-analyze verify /etc/systemd/system/vks-mcu-prod.service
sudo systemctl enable --now vks-mcu-prod.service
```

The server refuses to start (exit 78 / `EX_CONFIG`) when
`allow_insecure_defaults = false` and:

- `admin_password` is non-empty but weak (`admin`, `admin_password`,
  `password`, `secret`, `change_me`/`CHANGE_ME...`, or shorter than 12 chars);
- `jwt_secret` is shorter than 32 bytes or equals the published dev secret;
- `database_url` is empty/`:memory:`/contains `CHANGE_ME`, or
  `storage_allow_fallback = true`;
- the **existing database** still accepts `admin`/`admin_password` for the
  bootstrap operator **and** no strong `VKS_ADMIN_PASSWORD` is supplied.

An empty `admin_password` is valid when the database already has a strong
operator password; it is only required for a fresh database and for the
one-time weak-password rotation.

`RestartPreventExitStatus=78` stops systemd from looping on such config errors.

### Existing database and password rotation

`VKS_ADMIN_PASSWORD` is **not** applied to an existing operator on every start.
The first start bootstraps it; later starts leave the stored password untouched.
Effective-secret detection covers the known list in
`Config::KNOWN_WEAK_PASSWORDS` (`admin`, `admin_password`, `password`, `secret`,
`change_me*`, `CHANGE_ME*`). An arbitrary short user password cannot be
detected from a hash; rotate it via the UI if it was provisioned weakly.
If a production database still has one of the known weak values:

1. Set a strong `VKS_ADMIN_PASSWORD` and `VKS_JWT_SECRET` in `/etc/vks/vks.env`.
2. Start once. The server detects the weak stored password, rotates it to
   `VKS_ADMIN_PASSWORD`, persists the new hash, and continues starting.
3. Remove `VKS_ADMIN_PASSWORD` from the env file if desired; later starts work
   with the stored strong password.

If the flag is `false`, a weak stored password and no `VKS_ADMIN_PASSWORD`
produce exit 78 with an explicit instruction instead of an insecure start. If a
fresh database has no operator and no password, startup also fails with 78.

For an externally managed auth provider (`auth_provider = "ldap"`/`"oidc"`)
the local operator is not used for login; the local password checks still apply
to the bootstrap account only.

TLS terminates at a reverse proxy (nginx/Caddy); the server itself listens on
HTTP. Do not expose `listen_addr = "0.0.0.0:8080"` directly to untrusted
networks.

### Operational endpoints (P1-7)

`/api/v3/mcu/farm`, `/api/v3/gateways/status` and `/api/v3/cluster/status`
expose room topology and encoder/gateway state: they require
`Authorization: Bearer <JWT>` with the Moderator role (401 anonymous,
403 lower roles). The UI does not call them; use the admin token
(`POST /api/v3/login`).

`/metrics` (Prometheus text format) is scrapeable from loopback without
credentials. For a remote scrape — including through a same-host reverse proxy,
where every request appears to come from `127.0.0.1` — set the environment
variable `VKS_METRICS_TOKEN`; when it is set, the token is mandatory for every
request and is accepted as `Authorization: Bearer <token>` or
`X-Metrics-Token: <token>`. Example scrape config:

```yaml
scrape_configs:
  - job_name: vks-mcu
    static_configs: [{ targets: ["127.0.0.1:8080"] }]
    # remote/proxied only:
    # authorization: { type: Bearer, credentials_file: /etc/vks/metrics.token }
```

The benchmark harness (`scripts/bench/matrix.py`, `t2_matrix.sh`) passes the
admin token to the farm endpoint via `VKS_API_TOKEN`/`/tmp/vks.token`.

### Escape hatch (dev/emergency only)

`VKS_ALLOW_INSECURE_DEFAULTS=true` overrides the TOML flag. In that mode the
server logs a `SECURITY:` warning and accepts dev credentials. Use it only on
a loopback/isolated stand; never in the production unit. It is intentionally not
blocked in release builds: an operator with root can already deploy the dev
TOML, so this guard protects against accidental misconfiguration rather than a
malicious administrator. The effective value is logged at startup.

## H.323 sidecar

The sidecar looks for `VKS_H323_PYTHON`/`VKS_H323_HELPER`, then a set of
release-relative paths under `/opt/vks`. With `DynamicUser=yes` and
`ProtectHome=true`, provision the helper and venv under `/opt/vks`:

```bash
sudo python3 -m venv /opt/vks/h323-venv
sudo /opt/vks/h323-venv/bin/pip install -r /opt/vks/share/vks-mcu/requirements.lock
```

`vks.env.example` already points `VKS_H323_PYTHON` and `VKS_H323_HELPER` there.
Keep the existing home venv for local development only. `PYTHONDONTWRITEBYTECODE=1`
is set in the unit because `/opt/vks` is read-only.

The sidecar has no H.235/H.245 encryption implementation. Calls that offer only
TLS/IPsec security, carry H.235 tokens, or open a channel with
`encryptionSync` are released with `securityDenied`; the gateway never falls
back to plaintext for them. FastStart, reverse OLC and H.239 presentation are
also not implemented (the native staged rewrite is ADR-0034). The inbound UAS is
disabled by default and, when enabled, still requires the explicit
`VKS_H323_UAS_ALLOW` allowlist and the call/IP/child limits above.

Golden H.323 vectors live in
`crates/vks-gateway-h323/tests/golden/h323/` and are checked by
`cargo test -p vks-gateway-h323 --test golden_h323` plus
`python/test_golden_vectors.py`; they place no real calls.

## Verification

- `curl -fsS http://127.0.0.1:8080/healthz` and `/readyz`.
- `curl -fsS -X POST /api/v3/login -d '{"login":"admin","password":"..."}'`.
- `systemd-analyze security vks-mcu-prod.service` (score should improve vs a
  plain unit).
- Only one `vks-server` process: `pgrep -af vks-server`.

## Database maintenance and dedup

The server holds an advisory `flock` on `<database_url path>.lock` for its whole
lifetime. A second server on the same SQLite file exits with code 78, and
`vks-db-dedup --apply` refuses to run while the lock is held. This is deliberate:
the running server keeps the PNM document in memory and would overwrite an
offline edit on its next commit.

Safe procedure:

1. Stop the service (`sudo systemctl stop vks-mcu-prod`) or make a copy of the
   DB and run the tool against the copy.
2. Dry-run and inspect the JSON report:
   `vks-db-dedup /var/lib/vks/vks_data.db`
3. Apply on the stopped DB: `vks-db-dedup /var/lib/vks/vks_data.db --apply`.
   Before `--apply` the tool takes the same lock, creates
   `<db>.before-dedup-<nanos>` with `VACUUM INTO` and verifies it with
   `PRAGMA quick_check`; the document update runs in one transaction.
4. Start the service and run the `Verification` checks above.

Dry-run is read-only and does not take the lock. The operation is idempotent:
a second `--apply` reports zero removals/rewrites. The tool only removes
byte-identical default layouts that are referenced, preserves the UI order of
`links`, remaps `active_layouts`/`layout_aliases`, and never touches legacy
manual-default layouts (different payload).

### Upgrade order (active layouts)

An older binary does not know `active_layouts` and would drop it on its next
persist (the advisory lock does not help: old builds never took it). Upgrade in
this order, never overlap the two versions on one DB:

1. `sudo systemctl stop vks-mcu-prod` (or stop the user unit).
2. Back up the SQLite file; run `vks-db-dedup <db>` to inspect, then
   `vks-db-dedup <db> --apply` once.
3. Install the new binary/config and `sudo systemctl start vks-mcu-prod`.
4. Verify `/readyz`, then `vks-db-dedup <db>` again — it must report zero
   removals/rewrites (idempotent).

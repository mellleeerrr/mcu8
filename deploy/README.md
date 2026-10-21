# Production deployment (vks-server)

## Files

- `systemd/vks-mcu-prod.service` — hardened system-level unit, `DynamicUser=yes`.
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

## First start

```bash
sudo install -d -m 0755 /opt/vks /opt/vks/bin /opt/vks/frontend
sudo install -d -m 0755 /opt/vks/share/vks-mcu
sudo install -d -m 0750 /etc/vks
sudo install -m 0644 crates/vks-gateway-h323/python/h323_sidecar.py \
  crates/vks-gateway-h323/python/h323_asn.py /opt/vks/share/vks-mcu/
sudo install -m 0644 crates/vks-gateway-h323/python/requirements.lock /opt/vks/share/vks-mcu/
sudo install -m 0755 target/release/vks-server /opt/vks/bin/vks-server
sudo cp -a client/pnm-frontend/deployed/pnm /opt/vks/frontend/
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

# T07 review A — backend/static serving (HEAD `2442a16`; T07 `916824b`)

- **Date:** 2026-10-29, 12:40–12:56 MSK.
- **Reviewer:** independent delegated correctness-reviewer (backend/static-serving half). Production sources were not modified; the only file written in the live tree is this review.
- **Object:** HEAD `2442a16` (`docs(tracker): close T08/T09a`), T07 code in `916824b` (`feat(T07): PNM deep-link survival, theme invariant, runtime gzip, import integrity`). The T07 half under review here: `crates/vks-server/src/frontend.rs`, root `Cargo.toml`/`Cargo.lock`, `scripts/import-pnm-frontend.sh`, `scripts/check-pnm-runtime.py`, `scripts/check-pnm-theme.py`, and the `frontend-integrity` CI job.
- **Pristine copy:** `git archive 2442a16 | tar -x -C /dev/shm/t07a.6cnDQl` (52 MB, no `.git`/`target`). `CARGO_TARGET_DIR=/home/pro/.cache/t07a_target`; Rust **1.94.0** from `rust-toolchain.toml`; all Cargo commands `--offline`.
- **Archive:** `client/pnm-frontend/archive/PNM-Frontend-20261016.zip` is untracked (not in `git archive`); the copy was pointed at the live file read-only with `VKS_PNM_ARCHIVE`. sha256 = `ea0314f8…cbdfa`, matching the script anchor. The zip has no central directory (`unzip` refuses it), so the import used its documented local-header salvage path (482 files restored, 1 source-only entry skipped), not a normal unzip.
- **Live environment:** the live DB `vks_data.db` was never opened; its size/mtime (`3 248 128 B`, `2026-10-29 12:38:31.642604976 +0300`) predate this review and md5 at the end was `a925e7fa52831ed3aa8480c0e9531ff0`. The live listeners `:8080`/`:18082` were pre-existing processes (PIDs 3255206/3253281) and were not restarted or deliberately exercised. The review server used a new `/dev/shm` SQLite file and `127.0.0.1:49203`.

**Verdict: 8.9 / 10 — done: yes. 0 P0, 0 P1, 3 P2, 7 P3.**

The required backend gates are green and the behaviour is real, not just unit-test theatre: a pristine build serves `content-encoding: gzip` for `/pnm/`, SPA fallback, `chunks/*.js`, CSS and large SVG; leaves tiny files, PNGs and video alone; emits exactly one `Vary: Accept-Encoding`; keeps 304/404/Range honest; and **does not compress** `/metrics`, `/healthz` or `/api/v1/*` because the compression layer lives only inside `Frontend::router`. Decompressed payloads are byte-identical to identity responses. The import script re-runs byte-identically (`run2 == run1`, MANIFEST hash unchanged), reaches **215/215** files with no `.gz`, and re-applies the theme invariant. Remaining P2s are observability/import-failure-path gaps plus one pre-existing rebrand gate gap; none blocks T07's static-serving acceptance.

---

## 1. Gate table (pristine `2442a16`)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/home/pro/.cache/t07a_target
cd /dev/shm/t07a.6cnDQl
```

| Command | Exit | Result |
|---|---:|---|
| `cargo +1.94.0 fmt --all -- --check` | **0** | no output |
| `cargo +1.94.0 clippy -p vks-server --all-targets --offline -- -D warnings` | **0** | `Finished dev profile … in 1m 10s`; no warnings |
| `cargo +1.94.0 test -p vks-server --lib frontend --offline` | **0** | **20 passed**, 0 failed, 129 filtered, 0.04 s; includes both new gzip tests |
| `cargo +1.94.0 metadata --locked --offline --no-deps` | **0** | lock consistent with manifests |
| `cargo +1.94.0 build -p vks-server --bin vks-server --locked --offline` | **0** | binary used for the live matrix |
| `python3 scripts/check-pnm-runtime.py` | **0** | `215 files, manifest matches, script refs exist, no .gz` |
| `python3 scripts/check-pnm-theme.py` | **0** | `meta == dark --c-bg, light token present, toggle uses computed token, manifest #17212B` |
| `cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256` | **0** | 215 × `OK`, 0 `FAILED` |
| import run 1 (`--force --gzip`) | **0** | 215 manifest lines, 0 `.gz`, theme step applied |
| import run 2 (`--force`) | **0** | file tree + MANIFEST byte-identical to run 1 |
| import without `--force` | **1** | `deployed/ уже непустой … добавьте --force` |

No flakes: the 20 frontend tests were run once on the same archive and were deterministic; the live server was a single build started once.

## 2. `CompressionLayer` semantics in tower-http 0.6.11

Sources checked in the vendored crate, not from memory:

- `compression/future.rs:39-56`: compression is skipped for any response that already has `Content-Encoding` or `Content-Range`, then the predicate is asked; `Vary: Accept-Encoding` is **appended only if no existing `Vary` value already contains `Accept-Encoding`** (`:51-61`).
- `compression/future.rs:108-121`: on actual compression the layer removes `Accept-Ranges` and `Content-Length` and inserts `Content-Encoding`.
- `services/fs/serve_dir/future.rs:284,317`: regular `ServeDir`/`ServeFile` responses carry `Content-Length` from the file metadata (also for HEAD, `serve_dir/open_file.rs:88-105`).

Consequences, all confirmed live:

1. **The predicate does get a real `Content-Length`** for file responses, so the `>= 1024` rule works. Live: 1 267 173-byte JS → gzip; 71-byte JS / 160-byte JSON / 418-byte XML / 11 757-byte PNG → identity.
2. **`Vary` is emitted exactly once.** `Frontend::router` applies `with_cache_control` inside `CompressionLayer` (`frontend.rs:111-113`); the cache middleware inserts `Vary: Accept-Encoding` (`frontend.rs:237-242`), the compression layer sees it and does not append a second value. Scan over every captured static response (`200`, `304`, `404`, `206`, `308`, HEAD) found `Vary` count = 1 and value `Accept-Encoding`; `/metrics` has no `Vary`.
3. **No double-Vary risk from `insert`:** the cache layer uses `insert` (`frontend.rs:241`), so even if the layer order were inverted it would replace, not duplicate. The only observable failure mode would be losing a different future `Vary` token, which does not exist today.
4. **304/404/HEAD/Range are correct.** `304` and `404` are rejected by `status.is_success()` and are never compressed; `206` is rejected by the layer's `Content-Range` guard before the predicate; HEAD is compressed at the header level (see P3-1) but carries no body, which matches the GET representation's chunked headers. Live evidence below.
5. **Scope is static-only.** `CompressionLayer` is attached inside `Frontend::router`, and that router is only merged into `build_app` (`crates/vks-server/src/lib.rs:213`). Dynamic routers (`/api/*`, `/metrics`, `/healthz`) are not under this layer, so the predicate cannot compress API JSON/tokens (no BREACH surface added). `/metrics` is 6 272 bytes of `text/plain` and `/api/v1/rooms` is JSON, and neither was compressed under `Accept-Encoding: gzip`.

### 2.1 Live HTTP matrix (pristine server, `/dev/shm` DB, `127.0.0.1:49203`)

Server: `VKS_FRONTEND_DIR=<pristine>/client/pnm-frontend/deployed/pnm`, `VKS_DATABASE_URL=sqlite:///dev/shm/t07a_server.db?mode=rwc`, `VKS_WEBRTC_TRANSPORT=0`, PID 312038. Raw headers were captured with `curl --raw -D` (chunk framing preserved), then `curl --compressed` was used to compare decoded bytes.

| Request (Accept-Encoding: gzip unless noted) | Status | `Content-Encoding` | `Content-Length` | `Vary` | `Cache-Control` |
|---|---:|---|---:|---|---|
| `GET /pnm/` | 200 | **gzip** | absent (chunked) | 1 × `Accept-Encoding` | `no-cache` |
| `GET /pnm/` (identity) | 200 | — | 28 992 | 1 × `Accept-Encoding` | `no-cache` |
| `GET /mcu/chunks/index.bundle-2.2.1.66.js` | 200 | **gzip** | absent | 1 | `no-cache` |
| same, identity | 200 | — | 1 267 173 | 1 | `no-cache` |
| `GET /mcu/chunks/sleep.bundle-2.2.1.66.js` (71 B) | 200 | — | 71 | 1 | `no-cache` |
| `GET /mcu/assets/yandex-browser-manifest.json` (160 B) | 200 | — | 160 | 1 | `no-cache` |
| `GET /mcu/assets/browserconfig.xml` (418 B) | 200 | — | 418 | 1 | `no-cache` |
| `GET /mcu/assets/android-chrome-144x144.png` | 200 | — | 11 757 | 1 | `no-cache` |
| `GET /mcu/assets/images/C4WZZoZe.svg` (23 869 B) | 200 | **gzip** | absent | 1 | `immutable` |
| `GET /mcu/chunks/index.bundle-2.2.1.66.css` (581 227 B) | 200 | **gzip** | absent | 1 | `no-cache` |
| `GET /pnm/authorization` (SPA fallback) | 200 | **gzip** | absent | 1 | `no-cache` |
| `GET /mcu/chunks/nope.js` | 404 | — | 0 | 1 | `no-cache` |
| `GET /pnm` | 308 | — | 0 | 1 | `no-cache` |
| `HEAD /mcu/chunks/index.bundle….js` | 200 | **gzip** | absent | 1 | `no-cache` |
| `GET /mcu/assets/auto_scroll8oj5ttnk.webm` + `Range: bytes=0-9` | 206 | — | 10 | 1 | `immutable` |
| `GET /mcu/chunks/index.bundle….js` + `Range: bytes=0-9` | 206 | — | 10 | 1 | `no-cache` |
| `GET` big JS + `If-Modified-Since` (fresh) | 304 | — | absent | 1 | `no-cache` |
| `GET /metrics` | 200 | — | 6 272 | 0 | — |
| `GET /api/v1/rooms` | 401 | — | 79 | 0 | — |
| `GET /healthz` | 200 | — | 14 | 0 | — |

Additional checks:

- `cmp` of `curl --compressed` output against the identity body: `/pnm/` and the 1.2 MB JS are **byte-identical**; gzip is real deflate (RFC 1952), not a stub.
- `Range` bodies are exactly 10 bytes, `Content-Range: bytes 0-9/<size>`, and `Accept-Ranges: bytes` is preserved because no compression happened.
- `304` keeps `Cache-Control: no-cache` + `Vary`, and has no `Content-Encoding` or `Content-Length`.
- The 404 has no `Content-Type`, an empty body, and still increments the cache/Vary policy; no SPA fallback under `/mcu`.
- `Vary` duplicate scan over all captured headers: no response had >1 `Vary` line.
- CSP/security headers are unchanged by gzip (they are app-wide, outside the static layer) on both gzip and identity responses.

## 3. Import script (`scripts/import-pnm-frontend.sh`)

### 3.1 Run in a `/dev/shm` copy

```bash
TMPDIR=/dev/shm/t07a_tmp \
VKS_PNM_ARCHIVE="$LIVE/client/pnm-frontend/archive/PNM-Frontend-20261016.zip" \
bash scripts/import-pnm-frontend.sh --force --gzip
```

- Exit **0**. `манифест: … (215 строк)`, `gz` files = **0**, `check-pnm-runtime.py` and `check-pnm-theme.py` both green, `sha256sum -c` all 215 OK.
- Theme step logged `meta theme-color: #0e1621; manifest theme_color: #17212B`; project layer logged saved/restored `theme-toggle.js` and `deeplink.js`; the `index-scripts.html` marker block and `index-overrides.html` were preserved.
- `--gzip` did not create a single `.gz`; the warning appeared (`ВНИМАНИЕ: --gzip больше не нужен … флаг игнорируется`).
- Ordering observed in the log is exactly the required one: project layer restored → three vendor patches → theme invariant → script-ref check → rebrand warning → final MANIFEST → `--gzip` warning. The manifest is therefore computed **after** the theme rewrite of `index.html`/`manifest.webmanifest`.
- **Idempotency:** a second `--force` run produced a byte-identical tree (`diff` of the 215-file sha256 list empty) and an identical `MANIFEST.sha256` (`73dbc6ba…`). A run without `--force` refuses with exit 1, as documented.

### 3.2 Fail-closed theme step (extracted snippet, exercised on mutations)

The theme heredoc (`import-pnm-frontend.sh:289-310`) was extracted verbatim and run against mutated copies of the real files:

| Mutation | Exit | Behaviour |
|---|---:|---|
| happy path, run twice | 0 | second run changes no byte (idempotent) |
| remove `meta[name=theme-color]` | 1 | `не найден статический meta[name=theme-color]` |
| remove `html.theme-dark --c-bg` | 1 | `не найден html.theme-dark --c-bg: тема не применена` |
| swap meta attribute order (`content` before `name`) | 1 | fails closed, no silent skip |
| remove manifest `theme_color` | 1 | `не найден theme_color в manifest` |
| add a **second** `meta[name=theme-color]` | **0** | only the first is rewritten; the second stays `#0097A7` (P3-6) |

The regex cannot silently skip the current vendor form: every missing/changed structure tested exits non-zero. The one gap is multiplicity, not absence.

### 3.3 Fail-closed script-ref check (`:316-329`)

| Mutation of `index.html` | Exit | Meaning |
|---|---:|---|
| baseline | 0 | all 7 external `/mcu/*.js` refs exist |
| delete `theme-toggle.js` from the tree | 1 | caught (`ОТСУТСТВУЮТ скрипты…`) |
| change ref to `/pnm/theme-toggle.js` | **0** | **silently ignored** (only `/mcu/` is scanned) |
| single-quoted `src='/mcu/deeplink.js'` | **0** | **silently ignored** |
| `src="/mcu/deeplink.js?v=1"` | **1** | **false positive**: the query is treated as part of the filename |
| files present but project-layer block lost | **0** | the inverse direction is not checked; features can be unreferenced while this gate is green |

The specific 404-class bug T07 targeted (ref present, file missing) is caught; the reverse and alternate quote/path forms are not (P3-2).

### 3.4 Re-import is stable but **not** a fixed point of HEAD (pre-existing)

Run 2 equals run 1, so the script is idempotent, but run 1 differs from HEAD in **90 files**: 64 files directly under `assets/` (rebranded icons, favicon, `manifest.webmanifest`), `assets/images/prostor-wordmark.svg`, 24 rebrand-touched chunks, and `index.html`. This is the out-of-repo PROSTOR rebrand layer described in `NOTES.md`; the script warns that it will not be re-applied, but the divergence is larger than the warning's "title/locales" wording: after re-import `<title>` and manifest `name` are back to `TrueConf MCU`, and **every frontend-integrity gate stays green** (`sha256sum -c`, runtime check, theme check, `node --check`, all three patch `--check`). See P2-3.

## 4. New check scripts and CI wiring

- `check-pnm-runtime.py:22-47` compares the manifest's **file list** against `rglob("*")` (excluding `*.gz`), reports extra/missing names, checks every exact `<script … src="/mcu/…js"></script>` ref, and rejects any `.gz`. It does **not** verify digests (CI does that separately with `sha256sum -c`), and `name.lstrip("./")` would corrupt a filename that genuinely starts with a dot (none today).
- `check-pnm-theme.py:20-41` checks the first static meta vs the first `html.theme-dark --c-bg`, the `html.theme-light` token, the literal `getPropertyValue('--c-bg')` substring in `theme-toggle.js`, and manifest `theme_color == #17212b`. It does not detect a duplicate second meta (demonstrated above) and cannot tell a read-and-discard token lookup from a real write.
- CI wiring is correct: `.github/workflows/ci.yml:240-258` runs `sha256sum -c ../../MANIFEST.sha256` from `deployed/pnm`, then `check-pnm-runtime.py`, then `check-pnm-theme.py`, followed by `node --check` and the three patch `--check` gates. Failure of the theme step is caught by CI: on a synthetic tree where the dark token was removed, the import exited 1 with vendor meta `#0097A7`, `check-pnm-runtime.py` still passed (215 files), but `check-pnm-theme.py` failed with `html.theme-dark --c-bg missing` + `manifest theme_color #0097A7 != #17212B`.
- No false positives were found on the real tree; the known false-negative classes are listed as P3-2/P3-5/P3-6, not blockers.

## 5. Cargo.lock consistency

`Cargo.toml` switches `tower-http` to `features = ["fs", "compression-gzip"]`; the lock diff adds exactly the expected closure: `async-compression`, `compression-codecs`, `compression-core`, `flate2`, `crc32fast`, `miniz_oxide`, `adler2`, `simd-adler32`, `zlib-rs`, and `async-compression` in `tower-http`'s dependency list. `cargo metadata --locked --offline` and `cargo build --bin vks-server --locked --offline` both exit 0, so the lock is complete for CI's `--locked` jobs. `tower-http` is consumed only by `vks-server`, so the feature addition does not silently change another crate's dependency graph.

## 6. P2 findings (non-blocking, should be fixed)

### P2-1. gzip responses disappear from `vks_http_response_bytes_total`

`http_metrics_middleware` counts response bytes only from `Content-Length` (`crates/vks-server/src/lib.rs:357-365`), and it is applied app-wide outside the compression layer (`lib.rs:214-219`). `CompressionLayer` removes `Content-Length` (`tower-http` `compression/future.rs:108-121`), so every compressed static/SPA response contributes **0 bytes**. This is new in T07 (before it, `ServeDir` always supplied `Content-Length`) and the comment "для статики/API этого достаточно" is now false.

Measured on the live server:

```text
static_bytes before      = 3 815 925
after gzip 1 267 173 B   = 3 815 925   (delta 0)
after identity same file = 5 083 098   (delta 1 267 173)
```

The request/status/duration counters are unaffected; only the byte counter under-reports, and it under-reports precisely the traffic T07 was introduced to reduce. Fix: count at body level rather than from a header (wrap the response body and count what actually goes on the wire, or preserve a pre-compression length for the metric before `CompressionLayer` strips it); simply moving the metric inside the static layer would leave `/api` unmeasured.

### P2-2. Theme/ref-check failures happen after the staging swap, so the tree is already replaced

The staging swap is at `import-pnm-frontend.sh:208-209`; the new theme step (`:288-310`) and ref check (`:315-329`) run later. On failure the script exits 1 but `deployed/` is already the new vendor tree. Synthetic proof (copy with the dark `--c-bg` declaration removed): import exit **1**, deployed meta `#0097A7`, `check-pnm-runtime.py` green, `check-pnm-theme.py` red. Nothing is silently accepted (the exit code and theme check catch it), but the "runtime not touched on failure" property that motivated the staging swap does not cover T07's new validations. Fix: run theme/ref checks against `$STAGE/pnm` before the `rm -rf $DEST`, or keep a backup/rollback.

### P2-3. Re-import reverts the rebrand layer while every integrity gate stays green (pre-existing, not introduced by `916824b`)

As shown in §3.4, `--force` restores vendor title/manifest/icons and changes 90 files. The script warns, but the warning covers only title/locales, and the new CI checks do not assert `PROSTOR` branding or manifest name. A maintainer who runs the documented re-import, sees a green `frontend-integrity` job, and commits will silently regress the rebrand. This predates T07 (the rebrand layer lives outside the repo), but T07's claim of "import integrity" is the natural place to close it with a cheap `title == PROSTOR` / manifest-name assertion (or to make it explicit in the CI job description).

## 7. P3 findings (minor)

1. **`head_request_is_not_compressed` does not test what its name says.** It asserts only status 200 and `Vary` (`frontend.rs:635-652`), both of which the cache layer alone satisfies. Live HEAD on a 1.2 MB JS with `Accept-Encoding: gzip` returns `Content-Encoding: gzip` and no `Content-Length` (allowed by RFC 9110: HEAD may mirror GET headers, and the GET representation is chunked), but the test would pass even if the compression layer were removed. Rename it to what it checks, or assert the actual HEAD headers.
2. **Fail-closed ref check is form-specific** (§3.3): `/pnm/` refs and single quotes are silently skipped, query strings are false positives, and "file exists but is unreferenced" is not checked. Consider parsing tags more robustly (either quotes, strip query/fragment) and asserting that `theme-toggle.js`/`deeplink.js` are referenced after the project layer is restored.
3. **`application/manifest+json` is not compressed.** The 1 562-byte `manifest.webmanifest` is served with `Content-Type: application/manifest+json` and stays identity; the allowlist (`frontend.rs:142`) has `application/json`/`application/xml`/`text/*`/`image/svg+xml` but no `+json`/manifest suffix. Minor size/consistency miss; add `application/manifest+json` (or a suffix rule) if desired.
4. **Stale `--gzip` documentation and late warning.** The usage header still says `--gzip` "сгенерировать `.gz`" (`import-pnm-frontend.sh:25-26`), while the implementation ignores it and warns only at the very end (`:351-355`), after the manifest. Update the header and warn at argument parsing time. Related stale counts: `NOTES.md:27,31,65` still say 212 files / 210 manifest lines while `NOTES.md:389` and ADR-0027 say 215.
5. **`check-pnm-runtime.py` is names-only and dotfile-unsafe.** It never verifies a digest (CI's preceding `sha256sum -c` does), so run standalone it would accept a manifest whose names are right but hashes are stale. `name.lstrip("./")` (`:30`) strips any leading dots/slashes, so a genuine dotfile would be reported under the wrong name (latent; no dotfiles today).
6. **Theme checks/first-match fragility.** Both `check-pnm-theme.py` (`:20,37`) and the import theme step (`:298,304`) rewrite/inspect only the **first** `meta[theme-color]`/`theme_color`; a duplicate stale tag passes both (proven: second meta kept `#0097A7`, exit 0). The `theme-toggle.js` check is a substring (`getPropertyValue('--c-bg')`), so a read-and-discard implementation would pass. Prefer count==1 assertions and a real write check (or the browser e2e already used by the frontend half).
7. **Predicate is path-agnostic by design.** `should_compress_static` inspects only status/headers (`frontend.rs:120-145`); the security property "API is not compressed" rests entirely on the layer being attached inside `Frontend::router`. Today that is correct and verified, but a future refactor that hoists `CompressionLayer` into `build_app` would gzip `/api` JSON (including auth payloads) and reintroduce BREACH risk. Worth a comment/test asserting `/api` stays identity if the layer ever moves.

## 8. Hygiene

- All builds/imports/servers ran in `/dev/shm` copies; `CARGO_TARGET_DIR=/home/pro/.cache/t07a_target`; no `/tmp` copy of the repo was used.
- The live DB was not opened and its mtime predates the review; the review server used `/dev/shm/t07a_server.db`. Ports `:8080`/`:18082` were not touched; the review listener `127.0.0.1:49203` was closed and PID 312038 is gone (log: `received SIGTERM, initiating shutdown`).
- The live tree contains only this new review file; the pristine `/dev/shm` copies, the import/failure copies, the review DB/logs, and `/home/pro/.cache/t07a_target` were removed after the evidence above was captured.

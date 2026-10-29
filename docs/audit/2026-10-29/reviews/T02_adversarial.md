# T02 adversarial review — commit `eb514e2` (auth hardening)

- Repo: `/home/pro/Рабочий стол/mcu`
- Commit: `eb514e2a64ed50e709baaf9beb56bc38360ad70e` "feat(auth): Argon2id passwords, 429
  backpressure, honest LDAP sync" (2026-10-29 08:23:38 +0300)
- Task: T02 in `docs/progress/MASTER_TASK_LIST_2026-10-29.md` §"Критерии приёмки" / T02;
  ADR-0033; BACKEND_RULES §1.3–1.5.
- Reviewer role: second, independent adversarial reviewer. Source was **not** modified; the
  live `vks_data.db` was **not** touched (read from a `/tmp` copy only); no servers started.
- Note: the working tree contains unrelated uncommitted SFU/WebRTC changes (configs, vks-media-sfu, main.rs after line 348, lib.rs after line 885). The T02 files reviewed here are identical to `eb514e2`; line numbers refer to the commit.
- Verdict: **FAIL — not mergeable as-is.**
- Score: **4.0 / 10** (one P0 + three P1; the core Argon2id/PBKDF2 migration itself is sound).

## Acceptance criteria vs. result

| Criterion (T02) | Result |
|---|---|
| Argon2id exactly `m=19456 KiB, t=2, p=1`, PHC in `password_hash_hex` | ✅ correct in all creation paths (`local_db.rs:27-30,43-46,395-400`; PHC observed: `$argon2id$v=19$m=19456,t=2,p=1$…`). `salt_hex` empty for new rows; storage schema is `TEXT` so PHC fits. |
| PBKDF2 rows still login; known-answer vector | ✅ KAT independently recomputed (Python `hashlib.pbkdf2_hmac`) → `49d49c25…d2a1e` = the test vector at `local_db.rs:575`. Constant-time hex compare retained (`local_db.rs:403-412`). |
| Unknown-user timing dummy hash | ⚠️ **stale**: dummy is still PBKDF2-100k (`local_db.rs:157-158,301-302`) while real hashes are Argon2id → measured ~2.5× timing gap (P2-2). |
| Lazy rehash: generation and `password_changed_at` unchanged | ✅ in the `LocalDb` unit test (`local_db.rs:321-333,586-605`), but the production path is **provider-agnostic** (credential overwrite, P1-2) and racy (P2-3). |
| Queue overflow → real HTTP 429 + `Retry-After` | ❌ **not reachable in the shipped wiring**: production always uses `auth_provider`, which maps `Busy` to 401 and a lockout failure (P1-1). |
| 4 hashes + bounded queue, no counter leak | ❌ **waiter-counter leak on cancellation ⇒ permanent auth DoS** (P0-1). |
| Global token bucket 200 burst / 50 rps | ⚠️ works on protected `/api/v3` + m5, but is bypassable via `/api/v1/*` and all exempt `/api/v3` routes; docs claim "global" (P1-3). One key ⇒ bounded memory ✅. |
| Password ≤128 bytes | ✅ enforced on register/register_async/set_password/set_password_async/login/change_password. |
| `sync-ldap` → 501 without a real LDAP provider, no fake 200 | ✅ behavior honest (always 501); latent mock-sync trap in the dead flag (P2-6); BACKEND_RULES §1.5 still advertises a real LDAP client (P2-7). |
| ADR-0033 + BACKEND_RULES + vks.env.example | ⚠️ partially: ADR/BACKEND_RULES describe behavior that does not exist (429 in prod, "global", real LDAP) (P2-7). |
| Tests meaningful / no accidental passes | ⚠️ 14 % flaky 429 test (P2-1); login tests exercise only the branch production never uses (P2-9). |

---

## P0 (blocking)

### P0-1. Semaphore waiter counter leaks on cancellation → unauthenticated permanent auth DoS

`crates/vks-auth/src/local_db.rs:54-62`

```rust
if WAITING.fetch_add(1, Ordering::SeqCst) >= AUTH_QUEUE_CAPACITY {
    WAITING.fetch_sub(1, Ordering::SeqCst);
    return Err(LocalDbError::Busy);
}
let permit = hash_gate().acquire().await.map_err(|_| LocalDbError::Busy); // <-- cancel here
WAITING.fetch_sub(1, Ordering::SeqCst);                                    // never runs on drop
```

There is no RAII guard for `WAITING`. If the handler future is dropped while it waits on
`acquire()` — client disconnect/reset, task abort, panic, request timeout — the increment at
`:57` is never undone. Once 128 slots have leaked, `fetch_add` always observes `>= 128` and
**every** call to `hash_permit` returns `Busy` forever (login, `change_password`, register,
`set_password`, `rehash_async`). The state is process-global (`static WAITING`, `static GATE`),
so it is shared by all `LocalDb` instances and all tests; only a process restart recovers.

Reachability: `POST /api/v3/login` is unauthenticated, is **exempt** from the global limiter
(`api_v3/mod.rs:476`), and calls `authenticate_async` for *any* username (unknown users still
enqueue before the dummy hash). 4 hashes run, 128 wait; an attacker sends ~140–200 concurrent
logins and closes the sockets. No credentials are needed.

Proof (external harness, `vks-auth` by path, no repo changes): spawn 140 `authenticate_async`
tasks, wait 20 ms (4 running, 128 queued), abort all, wait 300 ms, then 20 fresh hashes:

```
CONTROL (no cancellation): Busy 0/20
LEAK (140 tasks cancelled while queued): Busy 20/20
```

Required fix: make the queue accounting RAII — increment when the waiter is created and
decrement in `Drop` while it is still waiting (e.g. `struct WaitingGuard; impl Drop …`), or
replace the manual counter with semaphore-owned accounting (`try_acquire` + explicit queue
depth, or `Semaphore::acquire` with a pre-checked permit count). Add a regression test that
cancels waiters and asserts the counter returns to zero.

---

## P1 (required before merge)

### P1-1. `Busy → 429` is dead code in production and is counted as a wrong-password login

- `crates/vks-server/src/main.rs:226,236` always sets `admin_state.auth_provider = Some(…)`,
  including for `auth_provider="local"` (factory `crates/vks-auth/src/provider.rs:132-135`).
- `crates/vks-admin/src/api_v3/auth.rs:286-288`: the provider branch maps **every** error,
  including the saturated-queue error from `crates/vks-auth/src/core_adapter.rs:33-35`
  (`CoreAuthError::Other("password hashing queue is saturated")`), to `LoginError::Invalid`.
  The Busy-aware branch at `auth.rs:289-295` (the only one mapping to `LoginError::Busy`) is
  therefore unreachable in the shipped server.
- `auth.rs:317-322` then treats Busy as a wrong password: `LOGIN_ATTEMPTS["fail"]`, audit
  `login_fail`, and `lockout.record_failure`. A user whose password was never checked can be
  locked out by a hash flood. `change_password` (`auth.rs:345-352`) has the same problem
  (Busy → 401 + `login_fail`), and `set_password_async` Busy → 500 in directory paths.

Proof (external harness replicating `main.rs` wiring: `auth_provider = Some(LocalAuth)` +
local_db + jwt issuer; saturate the gate, then one login with the **correct** password):

```
PROVIDER path (prod wiring) with saturated hash queue: HTTP 401, Retry-After=None
LOCAL-DB path (unreachable in main.rs) with saturated hash queue: HTTP 429, Retry-After=Some("1")
```

Required fix: preserve the error kind across `AuthProvider::verify` (typed `Busy`/`Unavailable`
instead of `Other`), map it to `LoginError::Busy` in the provider branch, return 429 +
`Retry-After`, and **do not** `record_failure`/audit `login_fail` for a capacity event. Add a
test that uses the production wiring (`test_state()` does not set `auth_provider`).

### P1-2. Lazy rehash is provider-agnostic → LDAP login overwrites a local user's password and persists it

`crates/vks-admin/src/api_v3/auth.rs:303-314`

The lazy migration runs on **any** successful login, not only when the credential was verified
against `local_db`: it looks the login name up in `local_db` (`db_user_by_name`) and, if the
row is a legacy PBKDF2 one, calls `rehash_async(..., &body.password)` and persists the result
(`persist_lazy_rehash`, `auth.rs:477-491`). `rehash_async` (`local_db.rs:321-333`) hashes the
caller-supplied plaintext without re-verifying it against the stored row. Consequently an
LDAP-authenticated login whose username collides with a local legacy row silently resets that
local credential to the LDAP-typed password, with generation/`password_changed_at` unchanged
(no revocation).

Proof (external harness; `auth_provider = ldap` — mock transport is the only LDAP client this
build can construct; local user is a legacy PBKDF2 row exactly like the live DB):

```
LDAP login status: 200 OK
local admin accepts LDAP-typed password: true
local admin still accepts original PBKDF2 password: false
persisted credentials: 1
  user_id=00000000-0000-0000-0000-000000000001 phc_prefix=$argon2id$v=19$m=19456,t=2,p=1 generation=1
```

The live DB (read from a `/tmp` copy) has exactly two PBKDF2 rows, one of them the bootstrap
operator `00000000-…-0001` with a 32-char salt — i.e. the affected shape is live, not
hypothetical. With the mock LDAP (accepts any password) anyone can set the local admin password
that survives switching back to `auth_provider=local`.

Required fix: record which provider authenticated and run the lazy rehash only for a LocalDb
authentication; inside `rehash_async`, re-read the row under the lock and only overwrite if the
stored hash is still the same legacy value (compare-and-swap/generation check); never persist a
rehash whose plaintext was not verified against that local row.

### P1-3. The "global" rate limiter is bypassable

The limiter lives only in `token_middleware` (`crates/vks-admin/src/api_v3/auth.rs:192-198`,
`:445-452`), attached with `route_layer` to the protected v3 router (`api_v3/mod.rs:472`) and
to `m5_api` (`crates/vks-server/src/lib.rs:193`). It does **not** cover:

- all `/api/v1/*` (`crates/vks-admin/src/handlers.rs:740-776`; JWT/RBAC only — includes the
  M5.5 endpoints `/api/v1/records`, plus `/api/v1/audit/export`, `/api/v1/users`). A client
  with any valid JWT can hammer these without consuming a bucket token;
- the exempt `/api/v3` routes (`api_v3/mod.rs:474-486`): `login`, `login/status`, `about`,
  `references`, `system/about`, `ws` handshake, `/static/call_history|log_events/*`;
- the catch-all 501 (`api_v3/mod.rs:489-491`).

The commit message ("on all /api/v3"), BACKEND_RULES §1.4 ("Глобальный лимит API") and
ADR-0033 ("Глобальный API-бюджет") all claim more than the code does. Additionally the limiter
is checked **before** token validation (`auth.rs:192`), so invalid/expired-token traffic drains
the single global bucket and can push legitimate clients into 429; one key with no per-IP/user
dimension means one client can starve everyone.

m5 endpoints themselves *are* covered (they use the same `token_middleware`), so that specific
hole is closed — but the criterion "global token-bucket" is not met.

Required fix: mount the limiter at the composition root (`crates/vks-server/src/lib.rs:199-204`)
or explicitly on both v1 and v3, decide/document the exempt set, and add tests proving v1 and
exempt routes are limited (or narrow the docs/acceptance wording to "protected /api/v3").

---

## P2 (should fix; not individually blocking, but several are real)

1. **Flaky 429 test — 7/50 failures (14 %) on an idle host.**
   `crates/vks-admin/src/api_v3/tests.rs:351-365` asserts `statuses[200..] == [429; 10]` and
   `statuses[..200]` all 200. The limiter refills 50 tokens on every wall-clock second
   (`auth.rs:449`), and the 210-request loop can cross a second boundary; observed failure:
   `left: [200 ×10]  right: [429 ×10]`. This is exactly the "passes only by accident" class.
   Fix: inject a deterministic tick into `global_rate_limit_allows` (parameterize `now`, as
   `RateLimiter::allow(key, tick)` already supports) or freeze time; assert exact boundary with
   a controlled clock. Also note the exact-200 assertion only holds on a fresh process state.
2. **Unknown-user timing oracle.** Dummy path still uses PBKDF2-SHA256 100k
   (`local_db.rs:157-158,301-302`); real users are Argon2id. Release microbench on this host:
   Argon2id verify **34.1 ms** median vs PBKDF2-100k **84.1 ms** median (~2.5×). Before T02 both
   paths matched, so this is a regression introduced by the migration. Fix: verify a fixed dummy
   PHC with the same Argon2id parameters for unknown users (and keep the cost independent of the
   supplied password length).
3. **Lazy-rehash TOCTOU / stale persistence.** `rehash_async` computes the hash outside the map
   lock and then blindly overwrites (`local_db.rs:326-332`). A concurrent `set_password_async`
   (password change) that commits in that window is silently undone — the old plaintext becomes
   valid again — and `persist_lazy_rehash` (`auth.rs:477-491`) can write the stale old-password
   hash together with the *new* generation/change mark to storage, so the rollback survives a
   restart. Fix: CAS on the stored hash (or generation) under the lock and persist only the
   snapshot that is still current.
4. **`persist_lazy_rehash` wrong-user fallback.** `auth.rs:481` maps a non-UUID local id to
   `UserId::bootstrap_operator()`; if such a row ever exists, its hash is written over the admin
   credential. Today all production local ids are UUIDs, so this is latent (same pattern already
   exists in `change_password:390` and `operator_policy.rs:65`). Fix: skip persistence (and log)
   on parse failure instead of substituting another user.
5. **Semaphore bypass in a password-creation path.** `directory/accounts.rs:139` calls the sync
   `db.register(...)` (Argon2, ~34 ms) directly in an async handler: it does not take one of the
   4 permits and blocks a tokio worker. The invariant "≤ 4 concurrent Argon2 hashes"
   (BACKEND_RULES §1.3) is therefore not global. Fix: `register_async`/`spawn_blocking` + permit.
6. **`ldap_sync_enabled` is a dead, unsafe flag.** It is only ever `false`
   (`state.rs:216,322`) — so the 501 is effectively hardcoded — but it is `pub`, and if any
   future code sets it true, the handler still calls `MockLdapProvider::fetch()`
   (`handlers.rs:554-566`), i.e. a fake 200 with mock users. Fix: remove the flag + mock branch,
   or bind it to the actual provider type; add a negative-control test that a configured
   LDAP URL still does not sync mock users.
7. **Docs to fact.** `local_db.rs:70-75` (`User` fields still documented as PBKDF2/32-char
   salt), `:235` and `:281` ("fresh salt + PBKDF2 hash", "offloading PBKDF2") are stale;
   `docs/rules/BACKEND_RULES.md:41` still promises `ldap3 + GSSAPI/Kerberos` (no such dependency
   and no real LDAP client; sync is 501); ADR-0033 line ~30 says queue overflow → 429, which is
   false in the shipped wiring; the commit message says the limiter is "on all /api/v3".
8. **Busy audit/lockout/Retry-After semantics.** Busy is audited as `login_fail`, counted in the
   `fail` metric and in `record_failure` (`auth.rs:317-322`) — misleading and lockout-inducing;
   `Retry-After: 1` (`auth.rs:455-461`) is shorter than a realistic queue drain (128 waiters ×
   ~34 ms / 4 ≈ 1.1 s Argon2; ~2.7 s for PBKDF2 rows). Fix: separate `login_busy` audit, no
   lockout count, queue-derived (or conservative) Retry-After.
9. **Test gaps / test-quality.**
   - no test at all for `Busy → 429`; the only 429 tests are the global-bucket one (flaky) and WS;
   - no API-level lazy-rehash test: token issued after rehash valid, pre-existing token still
     valid, storage row updated, generation/mark unchanged. Only the `LocalDb` unit test exists;
   - no Argon2 PHC storage round-trip test (`vks-storage` contract test still uses
     `"deadbeef01"` with a non-empty salt);
   - no cancellation test (would have caught P0-1);
   - `api_v3::tests::test_state()` (`tests.rs:533-549`) does not set `auth_provider`, unlike
     `main.rs`, so the login/lockout tests exercise the branch production never uses and miss
     P1-1.
10. **Minor:** `global_rate_limit_allows` fails **open** on a poisoned mutex
    (`auth.rs:449 unwrap_or(true)`); the limiter's wall-clock seconds make a backward clock jump
    stall refill until the clock catches up; the limiter state is bounded (constant `"global"`
    key) — the "no unbounded growth" criterion passes.

---

## What is genuinely good / verified correct

- Argon2id parameters are exactly `m=19456 KiB, t=2, p=1`; PHC output verified both by the
  test's exact prefix assertion and by an independent microbench that emitted
  `$argon2id$v=19$m=19456,t=2,p=1$Bwc…$eZ8…`.
- All password-creation paths (`register`, `register_async`, `set_password`,
  `set_password_async`, `rehash_async`) funnel through `hash_password`; `salt_hex` is empty for
  new rows; `restore_user` hydrates PHC/salt verbatim; the SQLite schema column is `TEXT`, so
  no truncation.
- The PBKDF2 known-answer vector is correct and independently reproduced; `ct_eq` is kept.
- Lazy rehash does not bump `generation` or `password_changed_at` (unit test passes), and
  rehash failure does not fail the login (warn-only).
- 128-byte password cap is enforced on every relevant path (and oversized input is rejected
  before hashing in `authenticate_async`).
- `sync-ldap` returns a real 501 (`error_code: ldap_sync_unavailable`); no fake-200 path is
  reachable today; the mock-LDAP factory gate (`VKS_ALLOW_MOCK_LDAP`) is untouched.
- m5 endpoints (`/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status`) do use
  `token_middleware`, so they are covered by the global limiter.
- `cargo clippy … -D warnings` is clean; both requested test suites are green.

---

## Commands run and results

| Command | Result |
|---|---|
| `git show eb514e2 --stat` | 26 files changed, 5544 insertions(+), 36 deletions(-) |
| `git diff eb514e2^ eb514e2` | reviewed all changed source + docs (local_db.rs, api_v3/auth.rs, mod.rs, tests.rs, state.rs, handlers.rs, ratelimit.rs, core_adapter.rs, main.rs, Cargo.*, BACKEND_RULES, ADR-0033, vks.env.example) |
| `cargo test -p vks-auth --locked` | **37 passed, 0 failed** (5.08 s) |
| `cargo test -p vks-admin --lib --locked` | **335 passed, 0 failed** (30.37 s) |
| `cargo clippy -p vks-auth -p vks-admin --all-targets --locked -- -D warnings` | **exit 0**, no warnings |
| Independent PBKDF2 KAT (`python3 hashlib.pbkdf2_hmac('sha256', b"correct horse battery staple", bytes(range(16)), 100000)`) | `49d49c25…75d2a1e` — matches the test vector |
| Release microbench (Argon2 0.5.3 / PBKDF2 0.12) | Argon2id verify median **34.1 ms**, PBKDF2-100k median **84.1 ms** (timing-gap evidence) |
| Burst test 50× (`vks-admin` test binary, exact test, `--test-threads=1`) | **7 failures / 50 runs (14 %)**; failure: `left: [200 ×10] right: [429 ×10]` |
| Cancellation harness `/tmp/leak_test` (path dep on `vks-auth`, release) | control **Busy 0/20**; 140 aborted waiters → **Busy 20/20** (permanent) |
| Production-wiring harness `/tmp/xprovider` (path deps on `vks-admin`/`vks-auth`, release) | LDAP login 200 → local admin credential overwritten + persisted as Argon2 under the bootstrap UUID, generation 1; saturated queue: provider path **401/None**, local-db path **429/1** |
| Live DB check (`cp vks_data.db /tmp/vks_review_copy.db`, Python sqlite3 `mode=ro`) | 2 credential rows, both 64-hex PBKDF2 + 32-char salt, incl. `00000000-0000-0000-0000-000000000001`; original DB untouched |

Only file written in the repository: `docs/audit/2026-10-29/reviews/T02_adversarial.md`.
The harnesses live in `/tmp` and import the workspace crates by path.

## Mergeability

**No.** Merge requires at least:

1. P0-1 (RAII waiter accounting + cancellation regression test);
2. P1-1 (typed Busy across the provider boundary, real 429, no lockout count, test with
   production wiring);
3. P1-2 (only rehash LocalDb-verified logins; CAS the stored hash before overwriting/persisting);

then P1-3 (honest limiter scope or honest docs) and the P2 items (especially the flaky test and
the timing dummy) before T02 can be marked done.

T02 status in `docs/progress/MASTER_TASK_LIST_2026-10-29.md` correctly remains
"🔄 in progress"; it cannot be closed on this commit.

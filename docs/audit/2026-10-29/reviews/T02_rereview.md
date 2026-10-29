# T02 re-review — commit `8dfc449` ("close T02 review P0/P1")

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`
- **Commit under review:** `8dfc449` "fix(auth): close T02 review P0/P1 — RAII hash gate, typed
  Busy->429, local-only lazy rehash, global bucket" (2026-10-29 08:52:41 +0300)
- **Baseline commits:** `eb514e2` (feature) and the two reviews
  `docs/audit/2026-10-29/reviews/T02.md` (6.0/10) and `T02_adversarial.md` (4.0/10).
- **Reviewer role:** third, independent adversarial re-review. Read-only: **no source file was
  modified**. The only file written in the repo is this report. Throwaway harnesses live in
  `/tmp/xprovider` and `/tmp/t02_time` (path deps on the workspace crates).
- **Live DB:** untouched. Read only from `cp vks_data.db /tmp/t02_rereview_db.db` with
  `file:…?mode=ro&immutable=1`; md5 of the original was identical before and after
  (`6a23442016492077f311becf14badace`). No server was started.
- **HEAD note:** HEAD moved to `d9aa3ce` (WebRTC T11a) at 08:56 while this review was running.
  `git diff 8dfc449 d9aa3ce -- crates/vks-auth crates/vks-admin crates/vks-core
  crates/vks-server/src/lib.rs …` touches only WebRTC code, the tracker line for T02 and
  `network_integration.rs`; **all T02 source and docs are identical to `8dfc449`** except the
  tracker row (`MASTER_TASK_LIST` now claims T02 "✅ … P0/P1 закрыты", which this review finds
  premature). Line numbers below are from `8dfc449` and are unchanged at `d9aa3ce` for every
  T02 file.
- **Concurrent worktree change (not mine, not reviewed):** at ~09:05 MSK the worktree gained an
  **uncommitted** modification to `crates/vks-auth/src/local_db.rs` (`hash_permit` now returns
  `OwnedSemaphorePermit`, plus a new `run_gated` that moves the permit into the
  `spawn_blocking` closure — i.e. someone is already addressing N1). It is **not** part of
  `8dfc449`, was not compiled/tested by this review, and is not counted in any verdict below.
  I modified no source file; `git status` shows only this report (and a pre-existing untracked
  `pitfalls/T04b.md`) besides that concurrent edit.

## Verdict

**Changes required — not mergeable as-is.** Score: **8.0 / 10**.

The fix is a real improvement: the P0 waiter-counter leak is genuinely gone (RAII + a
cancellation test), Busy now reaches HTTP 429 through the *production* provider wiring with no
lockout and no audit, the sync `register` bypass on the request path is closed, the CAS rehash
and the "LDAP must not rewrite local credentials" behaviour are implemented, the flaky bucket
test is deterministic, and the docs are more honest. However:

- the local-only lazy rehash was implemented on a branch (`LoginOk::Local`) that is **unreachable
  in the shipped binary** — `main.rs` always installs `auth_provider`, so a *local* login now
  never migrates a PBKDF2 row (new P1-class regression; the live DB has exactly two such rows,
  including `admin`);
- the semaphore permit is dropped when the request future is cancelled while the detached
  `spawn_blocking` Argon2 hash keeps running, so the "≤ 4 concurrent Argon2 hashes" invariant
  is still not cancellation-safe (new P1, pre-existing pattern, measured below);
- the "global" bucket moved to the `/api/v3` router but three `/api/v3` endpoints are mounted
  outside that router and, because the limiter was removed from `token_middleware`, they lost
  rate limiting entirely (new P2);
- `create_user_credentials` switched to `register_async`, which (unlike sync `register`) does
  not enforce login uniqueness (new P2 race);
- two admin credential routes still turn hash-gate saturation into 500/404 (residual of P1-1).

## Per-finding verdicts

Legend: **Fixed** = the reported defect is gone and the fix is verified; **Partial** = the
headline defect is mitigated but a residual of the same class remains; **Open** = not addressed.

The two reviews cite `eb514e2` line numbers; every evidence column below was re-anchored to
`8dfc449` (identical at current HEAD `d9aa3ce`).

| # | Finding (normalized) | Verdict | Evidence in `8dfc449` |
|---|---|---|---|
| P0-1 | Waiter-counter leak on future cancellation | **FIXED** | `crates/vks-auth/src/local_db.rs:52` single module static `HASH_WAITERS`; `:64-81` `QueueSlot` with `Drop`; `:83-87` `hash_permit` returns `(QueueSlot, SemaphorePermit)`; regression test `:650-676` (`cancelled_waiters_release_their_queue_slots`). Independently reproduced: 140 aborted waiters → 0/20 `Busy` (see commands). |
| P1-1 | Typed `Busy` → 429 through the production `auth_provider` path; no lockout, no `login_fail`; change-password/create-credentials too | **FIXED** (residual P2) | `crates/vks-core/src/ports/auth.rs:26-28` `AuthError::Busy`; `crates/vks-auth/src/core_adapter.rs:37` `LocalDbError::Busy → CoreAuthError::Busy`; `crates/vks-admin/src/api_v3/auth.rs:283-291` provider-branch mapping; `:331-336` Busy → 429 with only `LOGIN_ATTEMPTS["busy"]` (no `record_failure`, no audit); `:366-368` and `:388-392` `change_password` maps Busy on both the verify and the set; `crates/vks-admin/src/api_v3/directory/accounts.rs:139-147` `create_user_credentials` maps Busy→429 and `PasswordTooLong`→422. Tests: `api_v3/tests.rs:372-412` (provider Busy → 5×429 + `Retry-After: 1`, `blocked_for == None`). Independently reproduced with the **real** `LocalAuth` provider and a saturated gate: 429/`Retry-After: 1`, audit empty, no lockout. Residual: `accounts.rs:262-266` (`update_user_credentials`) and `:404-406`/`:423-427` (`profile_credentials`) still map Busy to 500 and Busy-on-old-password to 404 (P2, see below). |
| P1-2 | Lazy rehash only for LocalDb-verified logins, CAS, bootstrap fallback removed | **PARTIAL** | CAS is real: `local_db.rs:354-373` (`rehash_async_if_unchanged`, compare under the users lock at `:367`), test `:678-698`; the production login only calls it in the `LoginOk::Local` arm (`api_v3/auth.rs:309-327`), so an LDAP/provider login can no longer rewrite a local row (test `tests.rs:448-470`). **But** `LoginOk::Local` is only produced when `state.auth_provider == None` (`auth.rs:292-297`), and `vks-server/src/main.rs:220-236` always sets `auth_provider` (the local factory shares the same `local_db` Arc, `main.rs:157-159`). Hence a local login in the shipped binary takes `LoginOk::Provider` (`auth.rs:287`) and **never rehashes**: measured below — provider=local login 200, row still PBKDF2 (`needs_rehash` true); provider=None login 200, row upgraded. The bootstrap fallback is **not** removed: `auth.rs:500` (and reachable `auth.rs:404` in `change_password`) still do `UserId::parse(...).unwrap_or_else(|_| bootstrap_operator())`. |
| P1-3 | Gate bypass via sync `LocalDb::register` from async `create_user_credentials` | **FIXED** (nit) | `directory/accounts.rs:139` now `register_async`; no request-path caller of sync `register`/`set_password` remains (`grep`: sync `register` is used only by `vks-server/src/operator_policy.rs:51` at startup and tests; sync `set_password` has no production caller). Nit: `local_db.rs:132-139` only `#[doc(hidden)]`s `register` — still `pub`, so the gate remains bypassable by any future caller; `#[cfg(test)]`/private or a `register_unchecked` name would enforce it. Sync `authenticate` (un-gated) is still called at startup (`operator_policy.rs:82`), acceptable because it runs before serving. |
| P1-4 | "Global" bucket scope: login/ws/static/catch-all; docs accurate | **PARTIAL** | `crates/vks-admin/src/api_v3/mod.rs:500-506` mounts `global_bucket_middleware` as the outermost layer of the whole `api_v3::router`; `auth.rs:466-476` charges one process-wide bucket (`RateLimiter::new(200, 50.0)`) before auth; test `tests.rs:351-370` proves `/api/v3/login` is inside the bucket. **But** `vks-server/src/lib.rs:155-193` mounts three `/api/v3` endpoints (`/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status`) on a **separate** `m5_api` router (merged at `:203`), which only has `require_moderator` + `token_middleware`; the limiter was deleted from `token_middleware` (`auth.rs:186-196`), so those three lost rate limiting entirely (new P2). The docs claim the layer covers "**весь** `/api/v3`" (`docs/rules/BACKEND_RULES.md:38-41`), which is false for them. `/api/v1/*` remains outside the bucket, but the docs now scope the claim to `/api/v3`, which is honest. Pre-auth charging is documented as deliberate (`BACKEND_RULES:38-40`, `auth.rs:463-465`). |
| P2-1 | Flaky 429 test | **FIXED** | `tests.rs:351-370` installs `RateLimiter::new(5, 0.0)` via `api_v3/mod.rs:126-128` `install_test_bucket`; exact 5-then-429 boundary, zero refill. 12/12 repeated runs green (previously ~14 % flaky). |
| P2-2 | Status codes for oversized passwords on credential routes | **PARTIAL** | `accounts.rs:142-143` create → 429/422. Still open: `accounts.rs:262-266` and `:423-427` map `PasswordTooLong`/`Busy` to 500; `:404-406` maps Busy-on-old-password to 404; `api_v3/auth.rs:363-365` oversized old password in `change_password` still returns 401. |
| P2-3 | Lazy-rehash TOCTOU / stale persistence | **PARTIAL** | In-memory CAS is correct (`local_db.rs:367`). `persist_lazy_rehash` (`auth.rs:498-508`) still reads `generation`/`password_changed_at` **after** the CAS and writes the (possibly stale) hash, so a password change landing between CAS and upsert can persist the old-hash+new-generation pair. Dead in production today because the branch is unreachable (P1-2), but it is the same class of bug and should be fixed together with the wiring. |
| P2-4 | Missing tests | **PARTIAL** | Added: cancellation, CAS, provider-Busy (mock provider), deterministic bucket, provider-login-must-not-rewrite. Still missing: an end-to-end test with the **real** `LocalAuth` provider (the new Busy test uses a `BusyProvider` mock, so it does not exercise `core_adapter`; the lazy-rehash test would have caught P1-2), any HTTP-level lazy-rehash test, a change-password 128-byte test, and a test that the m5 routes are covered. `test_state()` still defaults to `auth_provider = None` (`tests.rs:634-647`), so most of the suite still exercises the branch production does not use. |
| P2-5 | Unknown-user dummy-hash timing parity | **PARTIAL** | `local_db.rs:319-327` now uses `hash_password` (Argon2id, same params) for unknown users. Measured on this host (release): legacy PBKDF2 verify **82.4 ms** vs unknown-user Argon2 dummy **32.2 ms** = **2.56×** — the oracle is gone for Argon2 rows but remains for the two live PBKDF2 rows (which now never migrate, see P1-2), i.e. exactly the accounts on this deployment. |
| P2-6 | Docs/code mismatches | **PARTIAL** | Fixed: LDAP plan/mock (`BACKEND_RULES:45-50`), `/api/v3` bucket scope, pre-auth charging, Busy≠login_fail (`:42-43`), `DEVELOPMENT_PLAN.md:242` Argon2 params, ADR queue RAII note. Still wrong: `BACKEND_RULES:25-27` also promises lazy rehash after a successful login (false in the binary); `BACKEND_RULES:38` says the layer covers "весь `/api/v3`" (m5 routes exempt); `BACKEND_RULES:30` still claims "Min 12 символов, top-10k check … в API-валидации" — no such validation exists; ADR-0033:22-25 and `local_db.rs:5-8` claim legacy rows are "lazily rehashed on the next successful login" — false in the binary; ADR-0033:28 still says "≤ 128 ожидающих" (the counter caps 128 *total* in-flight, so ≤ 124 waiters); stale PBKDF2 comments remain at `local_db.rs:95-97,263,284,307,375`; the stray "401 + Retry-After" doc is still attached to the bucket middleware (`auth.rs:458`) while `locked_response` is undocumented; `docs/progress/MASTER_TASK_LIST_2026-10-29.md:12` marks T02 "✅ … P0/P1 закрыты" (premature) and `:38` still requires "PBKDF2-хеш … лениво перехешируется" as an acceptance criterion that the binary does not meet. |
| P2-7 | `needs_rehash` is prefix-only | **OPEN** | `local_db.rs:336-341`: `!hash.starts_with("$argon2")`. A parameter upgrade (`m` bump) never triggers rehash; a corrupt `$argon2…` row is never repaired. Unchanged by the fix despite the commit message claiming "docs to fact". |
| P2-8 | `hash_password` `expect()` / unguarded `rehash_async` | **OPEN** | `local_db.rs:436-440` still `.expect("Argon2id hashing cannot fail")`; `local_db.rs:347-349` `rehash_async` is still `pub` and has no empty/128-byte guard (no production caller now, so latent). |
| P2-9 | Dead `ldap_sync_enabled` flag | **OPEN** | Never set to `true` (`crates/vks-admin/src/state.rs:216,322`); `handlers.rs:554` therefore always returns 501, and `MockLdapProvider::fetch()` at `:564-566` stays unreachable dead code. Unchanged. |
| P2-10 | Legacy >128-byte passwords lock out | **OPEN** | `api_v3/auth.rs:267-271` login → 422; `:363-365` change-password oversized old → 401. Unchanged (documented nowhere). |
| P2-11 | Commit hygiene | N/A | `8dfc449` is focused (auth + docs/tests); no complaint. |

## New findings introduced or exposed by this fix

### N1 (P1) — the hash-gate permit is released before the blocking Argon2 hash finishes on cancellation

`local_db.rs:318-330` (`authenticate_async`) and `:381-383`, `:285-293`, `:361-363` hold
`(_slot, _permit)` across `spawn_blocking(...).await`. `spawn_blocking` tasks cannot be
cancelled: if the request future is dropped while the hash runs (the same premise the P0-1 fix
relies on — client disconnect / task abort / timeout), the `SemaphorePermit` is dropped
immediately and a new request is admitted **while the detached hash is still allocating its
19 MiB**. The queue counter is correct (no permanent lease), but the "≤ 4 concurrent Argon2
hashes" invariant (`BACKEND_RULES:28-29`, ADR-0033:27-29) is violated under cancellation.

Reproduced with `/tmp/t02_time` (vks-auth by path, release, `VmHWM` from `/proc/self/status`):
4 permit holders + 4 new tasks, holders aborted after 10 ms:

```
mode=control VmHWM: base=23 MiB peak=80 MiB (delta 57 MiB)   # 4 hashes at a time
mode=cancel  VmHWM: base=22 MiB peak=156 MiB (delta 133 MiB)  # 7-8 hashes concurrently
(repeated 2×, identical)
```

An unauthenticated attacker who opens `/api/v3/login` requests and closes them *after*
admission (chained, a few sockets per ms) can detach hashes faster than they complete; the
global bucket caps the burst at ~200 requests, i.e. up to ~200 × 19 MiB ≈ 3.8 GiB transient —
the same OOM class as the original P1-3, reachable pre-auth. A naive "close all sockets at
once" storm only reaches ~2× over-concurrency (~150 MiB), so exploitability is
timing-dependent; the invariant breach is unconditional.

**Fix:** move an owned permit into the blocking closure (`acquire_owned()` and
`let _permit = permit;` inside `spawn_blocking`), or acquire the permit inside the blocking
task, so the permit is released only when the hash actually finishes. Add a regression test
that aborts in-flight hashes and asserts concurrency (e.g. memory/time bound).

### N2 (P2) — `/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status` lost rate limiting

`vks-server/src/lib.rs:155-193` builds `m5_api` as a separate router (merged at `:203`) with
only `require_moderator` + `token_middleware`; the bucket check was removed from
`token_middleware` (`api_v3/auth.rs:186-196`) and is now mounted only on
`api_v3::router` (`api_v3/mod.rs:505`). These three endpoints are therefore not covered by the
"global" bucket and are no longer limited at all (before `8dfc449` they were charged via
`token_middleware`). Authenticated (Moderator+) only, read-only payloads → P2, but it
contradicts the "весь `/api/v3`" claim and the commit message.

### N3 (P2) — `create_user_credentials` now bypasses the LocalDb login-uniqueness check

Sync `register` enforces case-insensitive login uniqueness (`local_db.rs:146-159`), but
`register_async` only checks `map.contains_key(id)` (`local_db.rs:385-388`), and the handler's
pre-check (`accounts.rs:135-137`) is TOCTOU. Switching the route from `register` to `register_async`
(`accounts.rs:139`) therefore allows two concurrent credential creations for different users
with the same login to insert duplicate usernames; `authenticate_async` (`local_db.rs:312-314`)
picks one by `HashMap` iteration order, so a login can authenticate as the wrong account.
Requires two concurrent `POST /api/v3/users/{id}/credentials` (manage privilege) → P2, but the
uniqueness check should be moved into `register_async`.

### N4 (P2, accepted-by-docs design risk) — single global key, charged pre-auth

`auth.rs:466-476` charges every `/api/v3` request (including unauthenticated `login`) against
one `"global"` key with no per-IP/per-user dimension: 200 requests (or a sustained 50 rps) from
one client make the whole API answer 429 for every user, including legitimate login and
static assets. This is now *documented* as deliberate (`BACKEND_RULES:38-40`), which answers
the original "decide the semantics" request, but the availability trade-off deserves an
explicit decision (per-IP dimension, or charge after auth for authenticated routes).

## Explicit counts

- **Open P0: 0.** (P0-1 fixed and verified.)
- **Open P1: 2.**
  1. P1-2 residual: lazy Argon2id migration is dead in the production wiring
     (`LoginOk::Local` unreachable; `main.rs:236` always sets `auth_provider`) — the live
     PBKDF2 rows for `admin` and `viewer_probe` will never be upgraded, while ADR-0033 and the
     module docs claim they will; bootstrap-operator fallback also still present.
  2. New N1: semaphore permit released before the blocking hash completes on cancellation →
     the "≤ 4 concurrent Argon2 / bounded memory" invariant is not cancellation-safe
     (measured 7-8 concurrent hashes, 133 MiB vs 57 MiB).
- **Fully fixed P0/P1: 3** — P0-1, P1-1, P1-3. (P1-4 is half-fixed: the `/api/v3` layer now
  covers login/ws/static/catch-all, but the m5 endpoints are a new open gap.)
- **Open P2: 11** (including two new: N2 m5 rate-limit regression, N3 register_async
  uniqueness; plus admin/profile Busy→5xx,
  lazy-rehash persist race + bootstrap fallback, prefix-only `needs_rehash`, `expect`/
  unguarded `rehash_async`, stale PBKDF2 docs + tracker overclaim, dead `ldap_sync_enabled`,
  legacy >128-byte lockout, Retry-After 1 s vs ~2.7 s worst-case drain, test gaps).

## Score: 8.0 / 10

Justification:

- **+** The P0 is genuinely fixed with a clean RAII design and a regression test; independently
  reproduced (0/20 `Busy` after 140 aborted waiters).
- **+** Busy → 429 now works through the *real* production provider path, with no lockout and
  no `login_fail` audit; independently reproduced. `create_user_credentials` and
  `change_password` are mapped; the deterministic bucket test removes the 14 % flake.
- **+** The cross-provider credential-overwrite vulnerability is gone and the CAS is correctly
  placed under the lock; docs improved (LDAP plan, bucket scope, ADR queue note, Argon2
  params); rustdoc/fmt/clippy clean.
- **−** The lazy-migration fix was implemented on a branch that cannot execute in the shipped
  binary — the same "dead in production" failure mode as the original P1-1 — so a stated T02
  acceptance criterion (PBKDF2 → Argon2id migration of the two live rows) is not met, and ADR/
  module docs overclaim it.
- **−** The cancellation-safety work stopped at the counter: the permit is still released before
  the detached hash finishes, so the memory bound the gate exists for is not guaranteed
  (measured).
- **−** New P2 regressions: three `/api/v3` endpoints lost rate limiting, and the credential
  route lost the LocalDb login-uniqueness check; two admin routes still answer 500 on
  saturation.

Not mergeable at ≥ 9.5 (two open P1). With the lazy rehash wired through verifier provenance
(the local factory already knows it — `vks_auth::provider::LocalAuth::verify` returns
`source: AuthSource::Local`, `provider.rs:160-166` — but the core-port adapter that
`/api/v3/login` actually uses discards it at `core_adapter.rs:24-32`) and the permit moved into
the blocking task, this would be a 9.5-class change.

## Commands run and results

All run from `/home/pro/Рабочий стол/mcu` with `export PATH="$HOME/.cargo/bin:$PATH"`, at
HEAD `d9aa3ce` (T02 files identical to `8dfc449`).

| Command | Result |
|---|---|
| `git show 8dfc449 --stat` / per-file `git show 8dfc449 -- <file>` | Reviewed all 12 changed files; `crates/vks-admin/src/api_v3/auth.rs`, `mod.rs`, `directory/accounts.rs`, `tests.rs`, `crates/vks-auth/src/{local_db,core_adapter}.rs`, `crates/vks-core/src/ports/auth.rs`, docs. |
| `git diff 8dfc449 d9aa3ce -- <T02 paths>` | Only WebRTC (`vks-server/src/lib.rs`, `main.rs`) and the tracker row; T02 source unchanged. |
| `cargo test -p vks-auth --locked` | **39 passed, 0 failed** (4.20 s) + 0 doc-tests. Includes `cancelled_waiters_release_their_queue_slots`, `cas_rehash_refuses_to_overwrite_a_newer_password`. |
| `cargo test -p vks-admin --lib --locked` | **337 passed, 0 failed** (29.64 s). |
| `cargo clippy -p vks-auth -p vks-admin --all-targets --locked -- -D warnings` | **exit 0**, no warnings (22.33 s). |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p vks-auth -p vks-admin --no-deps --locked` | **clean** (26.30 s). |
| `cargo fmt --check` | **no diff** printed. |
| `for i in {1..12}; cargo test -p vks-admin --lib --locked api_v3::tests::global_api_bucket_limits_bursts_with_429 -- --exact` | **12/12 pass** (previously ~14 % flaky). |
| `/tmp/xprovider` (path deps on `vks-admin`/`vks-auth`/`vks-core`, release) — production wiring `auth_provider = LocalAuth::with_db(same db)`, 4 permits + 128 queue saturated | `PROVIDER path, saturated, CORRECT password: HTTP 429 retry=Some("1") body={"error":{"code":429,"description":"Authentication is temporarily busy"}}`; 5 further attempts all 429, `lockout=None` each time; **audit entries after busy logins: `[]`**; LOCAL-DB path (provider None): HTTP 429 retry=Some("1"). **P1-1 fixed, empirically.** |
| `/tmp/xprovider` — lazy rehash on a legacy PBKDF2 row (the live shape), same login: | `local_only=false login HTTP 200; before=49d49c25.. after=49d49c25.. rehashed=false needs_rehash_after=true gen=1 mark=100` (production wiring: **not migrated**); `local_only=true login HTTP 200; before=49d49c25.. after=$argon2i.. rehashed=true needs_rehash_after=false gen=1 mark=100` (dead branch: migrated). **P1-2 residual proven.** |
| `/tmp/xprovider` — 140 queued waiters, all aborted, then 20 fresh hashes | `finished-at-abort=12/140; Busy probes after 140 aborted waiters: 0/20`. **P0-1 fixed, empirically.** |
| `/tmp/t02_time` — timing parity, release, 15 samples each | legacy PBKDF2 verify median **82.4 ms**; unknown-user Argon2 dummy median **32.2 ms**; ratio **2.56×**. |
| `/tmp/t02_time` — concurrency/memory after cancelling in-flight hashes (`VmHWM`) | control: base 23 MiB, peak 80 MiB (Δ57); cancel: base 22 MiB, peak 156 MiB (Δ133) — repeated twice, identical. **N1 proven.** |
| Live DB read-only: `cp vks_data.db /tmp/t02_rereview_db.db`; `python3 sqlite3 file:…?mode=ro&immutable=1`; `md5sum` before/after | 2 `user_credentials` rows, **both 64-hex PBKDF2 with 32-hex salt, generation 1** — `00000000-…-0001` (`admin`, operator) and `c92dced8-…` (`viewer_probe`, viewer). Original md5 unchanged (`6a2344…`). **Impact of the dead lazy migration is live.** |

### Limitations

- I did not start the server (no live HTTP run; the router was exercised in-process through
  `tower::oneshot`, as the repo's own tests do).
- N1's worst-case 3.8 GiB figure is an extrapolation from the measured 7-8 concurrent hashes
  and the 200-request bucket burst; I did not attempt the full timed-wave attack.
- The uncommitted-worktree situation changed during the review (another agent committed
  `d9aa3ce`); the T02 files I cite were verified identical between `8dfc449` and `d9aa3ce`.

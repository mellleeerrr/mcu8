# T02 re-review #2 — commit `5d46507`

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **Commit under review:** `5d465074770bf438d805d6bbfa2e09e932e226c8` "fix(auth): T02 final review —
  register_async uniqueness, N1 regression test, 422 for oversized old password"
  (2026-10-29 10:37:15 +0300). Chain: `eb514e2` → `8dfc449` → `ddd033b`
  (`T02_final.md`: 9.0/10, 0 P0/P1, 12 P2 + 4 P3) → `5d46507`. Note: `a33e195` and `08398b3`
  (T11a) sit between `ddd033b` and `5d46507`, so the diff `ddd033b..5d46507` also contains
  WebRTC/T11a work; only the T02-relevant files are judged here.
- **Prior reviews read:** `T02.md` (6.0), `T02_adversarial.md` (4.0), `T02_rereview.md` (8.0),
  `T02_final.md` (9.0).
- **Role:** fifth, independent final reviewer. **No source file in the live repository was
  modified.** The only file written in the live repo is this report. All builds, probes and
  temporary source experiments were done in a pristine `git archive 5d46507` copy under
  `/dev/shm/t02r2/{5d46507,work}`; temporary edits were reverted and the scratch trees are
  disposable.
- **HEAD note:** HEAD moved during the review from `5d46507` to `650a55f` (T09a recording,
  10:51). The clean-checkout test-build defect found at `5d46507` (P1 below) is **still present at
  `650a55f`** (see Item 5); `crates/vks-core/src/ports/storage.rs` remains uncommitted in the live
  worktree.
- **Live DB:** untouched. `md5sum vks_data.db` = `6a23442016492077f311becf14badace` (the value the
  previous four reviews recorded) and its mtime is still `2026-10-29 07:33:29`; no server was
  started and no SQLite connection was opened against it.

## Verdict

**T02's auth behaviour is fixed and independently verified, but the commit is not self-consistent
and cannot be marked done as-is.** Score: **8.0 / 10**.

Confirmed fixes at `5d46507`:

1. **`register_async` login uniqueness is real.** The check and the insert happen under one
   `users` lock (`local_db.rs:434-447`); a live-path probe over the production router with a real
   `LocalAuth` provider produced exactly one `201` + one `409` per concurrent duplicate pair
   (20/20 races, 20/20 direct `register_async` races), never two rows and never a `500`.
2. **Cancellation-safe permit is real.** The permit is owned by the blocking closure
   (`local_db.rs:116-128`): the pre-fix shape fails the invariant (observed **7** concurrent
   hashes), the fixed shape never exceeds **4**.
3. **Oversized old password is 422** at `PATCH /api/v3/password` (`auth.rs:365-367`), verified
   over HTTP; `Busy → 429` + `Retry-After: 1` now works on change-password, profile and
   update-credentials, and `PasswordTooLong → 422` on the new-password paths.
4. The test-only counters are `#[cfg(test)]`; the release build has no instrumentation code.

But two new findings block the 9.5 gate:

- **NEW P1 — the commit tree is red on a clean checkout.** `cargo test -p vks-admin --lib` and
  `cargo clippy --all-targets` do not compile at `5d46507`
  (`error[E0560]: RecordEntry has no field named bitrate_kbps/width/height`), and two recorder
  integration tests fail deterministically because their assertions belong to uncommitted T09a
  implementation. The live worktree only compiles because the relevant T09a changes are still
  uncommitted. This is not a T02 code defect, but it is a P1 for the commit under review.
- **NEW P2 — the advertised N1 regression test is a false negative.** Run as shipped against the
  pre-fix permit shape, `cancelled_hashes_never_exceed_the_permit_limit` passes **5/5** (it
  aborts before the spawned hashes ever start). With a 20 ms yield before the abort it fails
  3/3 on the old shape (observed 7) and passes 3/3 on the fixed shape. The test therefore does
  not currently guard the invariant it claims to guard.

Residual P2/P3 are unchanged in kind from `T02_final.md` (11 open P2, 4 P3); none is a
security/availability release blocker for the auth feature, but the tracker/docs still do not
match the code.

## Item 1 — (a) `register_async` uniqueness and `create_user_credentials` 409/422 — **FIXED**

### Code

| Evidence (`5d46507`) | What it shows |
|---|---|
| `crates/vks-auth/src/local_db.rs:428-452` | `register_async`: length check, then `run_gated(hash)`, then **one** `self.users.lock()` |
| `:434-438` | duplicate id check under that lock |
| `:439-444` | `username.trim().to_lowercase()` membership check over all stored usernames, same lock, immediately before `map.insert` — no await between check and insert |
| `crates/vks-admin/src/api_v3/directory/accounts.rs:135-137` | handler pre-check remains (fast path, TOCTOU) |
| `:139-148` | `register_async`; `Busy → 429`, `PasswordTooLong → 422`, any other error (incl. `AlreadyExists`) → **409**; the previous 500/404 hole is gone |

The hash is computed before the lock (a duplicate attempt still burns one Argon2 hash), but
uniqueness itself is genuinely atomic: there is no `.await` between the duplicate check and the
insert.

### Live-path probe (`/dev/shm/t02r2/work`, release, production wiring)

`AppState` + `vks_admin::api_v3::router` + `tower::oneshot`, `state.auth_provider =
LocalAuth::with_db(same Arc<LocalDb>)`, real JWT login (`admin/s3cret`), two directory users per
iteration, identical `POST /api/v3/users/{id}/credentials` payloads fired with `tokio::join!`:

```
seq_dup (same user twice):  first=201  second=409 {"code":409,"description":"Credentials already exist"}
race n=20 (two different users, same login):  20 × [201, 409], both_created=0, login_rows=20
       sample: i=0 [201,409] a_exists=true  b_exists=false
               i=2 [409,201] a_exists=false b_exists=true
direct LocalDb register_async race n=20:  ok=20, AlreadyExists=20, unexpected=0
case-insensitive duplicate: lower=201, upper=409 {"description":"Login already exists"}
```

No `500`, no two `201`s, no second LocalDb row for the same login. The commit-message claim is
now true.

Remaining gap: uniqueness has **no in-tree test** (the new tests in `5d46507` cover only the
cancellation issue); the 20/20 evidence above is external.

## Item 2 — (b) the new cancellation regression test — **TEST IS A FALSE NEGATIVE**

### The fix is correct

`local_db.rs:116-128`: `hash_permit().await?` returns `(QueueSlot, OwnedSemaphorePermit)`; the
owned permit **and** the queue slot are moved into the `spawn_blocking` closure and kept alive by
`let _gate = gate;`, so dropping the request future cannot release the permit while the detached
job runs. A `spawn_blocking` job cannot be cancelled, so this is the right shape.

### The shipped test does not exercise cancellation

`local_db.rs:762-801`: six `authenticate_async` tasks are spawned, then the first three handles
are aborted **with no yield between `spawn` and `abort`**, then a 50 ms sleep and a new six-task
storm follow. On the 4-worker test runtime the abort calls win the race: the three targets never
reach `hash_permit`/`run_gated`, no permit is ever released, and the storm runs against an
undisturbed gate. The assertion `MAX_CONCURRENT_HASHES <= AUTH_HASH_PERMITS` is then vacuously
true — even on the buggy implementation.

### Experiment (throwaway copy; permit moved out of the closure to model pre-`ddd033b`)

The test-only instrumentation was kept exactly as shipped; only `run_gated` was reshaped to hold
the permit in the request future (`let gate = ...; let result = spawn_blocking(...).await; let
_gate = gate; result`), which is the old failure mode. Restored afterwards.

| Implementation | Test variant | Runs | Result |
|---|---|---|---|
| fixed (`5d46507` as shipped) | as shipped | 5 | **5/5 pass**, max ≤ 4 |
| old (permit outside closure) | as shipped | 5 | **5/5 pass — false negative** |
| old (permit outside closure) | + `sleep(20 ms)` before `abort()` | 3 | **3/3 fail**, `observed 7` |
| fixed (`5d46507`) | + `sleep(20 ms)` before `abort()` | 3 | 3/3 pass, max ≤ 4 |

The last two rows prove both that the bug is real and that the fix works; the second row proves
the in-tree test does not catch it. **Recommendation:** wait until hashes are actually in flight
before aborting — e.g. poll `ACTIVE_HASHES` until it reaches `AUTH_HASH_PERMITS` (or sleep
10-20 ms), and additionally assert `ACTIVE_HASHES > 0` at abort time so the test cannot pass
vacuously. This is small and would make the claim in the commit message true.

Consequence for scoring: the code is cancellation-safe, but the commit has **no real in-tree
regression evidence** for the P1-class invariant, which is one of the explicit 9.5 conditions.

## Item 3 — (c) oversized old password and Busy/PasswordTooLong mappings — **PARTIAL**

`change_password` now returns **422 "Old password is too long"** (`auth.rs:365-367`) before the
Argon2 verify; over HTTP the probe returned:

```
PATCH /api/v3/password (old=129B)                    -> 422 {"description":"Old password is too long"}
PATCH /api/v3/password (new=129B)                    -> 422 {"description":"New password is too long"}
PATCH /api/v3/profile/credentials (old=129B)         -> 404 {"description":"Incorrect old password"}   <-- still wrong
PATCH /api/v3/profile/credentials (new=129B)         -> 422 {"description":"Password is too long"}
PATCH /api/v3/users/{id}/credentials (new=129B)      -> 422 {"description":"Password is too long"}
```

Busy under a saturated gate (200 in-flight hashes, real provider):

```
PATCH /api/v3/password                -> 429 retry=Some("1") "Authentication is temporarily busy"
PATCH /api/v3/profile/credentials     -> 429 retry=Some("1")
PATCH /api/v3/users/{id}/credentials  -> 429 retry=Some("1")
```

Route-by-route status at `5d46507`:

| Route / branch | Busy | PasswordTooLong | Other |
|---|---|---|---|
| `create_user_credentials` `accounts.rs:139-148` | **429** | **422** | 409 (duplicate) |
| `update_user_credentials` `:263-271` | **429** | **422** | 500 |
| `update_profile_credentials` new `:430-438` | **429** | **422** | 500 |
| `update_profile_credentials` old `:409-412` | **429** | **404 "Incorrect old password"** | — |
| `PATCH /api/v3/password` old `auth.rs:365-370` | **429** | **422** | 401 |
| `PATCH /api/v3/password` new `:377-393` | **429** | **422** | 500 |

So the two old-password sites are now asymmetric: `change_password` was fixed, `profile/credentials`
still answers 404 for an oversized old password (P2-6 residual). There is also no in-tree test
for the new 422.

## Item 4 — (d) instrumentation safety — **CLEAN**

- Every added item is gated: statics `local_db.rs:87-90`, guard `:92-99`, counter update
  `:119-124` are all `#[cfg(test)]`; the production closure is still `let _gate = gate; job()`.
- `cargo build -p vks-auth --release --locked` at pristine `5d46507`: **exit 0** (23.7 s).
- The release rlib object members contain **no** `MAX_CONCURRENT_HASHES` / `ACTIVE_HASHES`
  symbols (`ar x` + `strings` + `nm`; 0 occurrences in the `.o` member, 2 only in `lib.rmeta`).
  A two-line control crate showed the same rmeta behaviour for a disabled `#[cfg(test)]` static,
  so this is normal rustc metadata, not code. No runtime effect.
- Production builds (`cargo build -p vks-admin`, `-p vks-server`) at `5d46507` compile; the
  instrumentation does not change production behaviour.

The only caveat: the counters are process-global and never reset. With the correct implementation
that is harmless (max can never exceed 4); it would mask nothing but also means the test depends
on execution order only if the invariant is broken — acceptable.

## Item 5 — (e) NEW P1: the commit tree is not self-consistent

### P1-a: `vks-admin` tests/clippy do not compile at `5d46507`

```
$ cargo test -p vks-admin --lib --locked
error[E0560]: struct `RecordEntry` has no field named `bitrate_kbps`
  --> crates/vks-admin/src/api_v3/observability_tests.rs:165:17
error[E0560]: ... `width`   ... :166
error[E0560]: ... `height`  ... :167
error: could not compile `vks-admin` (lib test) due to 3 previous errors
```

`5d46507` commits the T09a test fixture fields (`observability_tests.rs:165-167`) but **not** the
`vks_core::ports::storage::RecordEntry` fields they refer to; those are still an uncommitted
modification in the live worktree. `cargo clippy -p vks-auth -p vks-admin -p vks-server
--all-targets -- -D warnings` fails with the same E0560. This is the required admin command of
this review, so the commit cannot pass its own acceptance gate.

### P1-b: two recorder integration tests fail deterministically at `5d46507`

```
$ cargo test -p vks-server --test network_integration --locked test_m5_recorder
test_m5_recorder_lifecycle_integration ... FAILED
  assertion `left != right` failed: must return the record id      (network_integration.rs:815)
test_m5_recorder_idempotent_stop_persistent_storage_and_playable_container ... FAILED
  assertion `left != right` failed: start returns the record id    (network_integration.rs:1576)
test result: FAILED. 0 passed; 2 failed
```

These assertions were added by `08398b3` (T11a), but the implementation that returns the record
id (`crates/vks-server/src/recorder.rs`) was only present as an uncommitted worktree change at
`5d46507` (`start_recording` still returns `Ok(room_uuid)` at `:63-130`). The failure is
deterministic (2/2 reruns). `650a55f` later commits the recorder side, so this symptom is fixed
at current HEAD, but `5d46507` remains a broken commit for bisect/CI.

### P1-c: the HEAD note

At review end HEAD is `650a55f` (T09a). It still does **not** contain the
`RecordEntry { bitrate_kbps, width, height }` fields and the required storage methods;
`git diff 650a55f -- crates/vks-core/src/ports/storage.rs` is non-empty (10 insertions / 12
deletions). Therefore `cargo test -p vks-admin --lib` **still fails on a clean checkout of
HEAD**; only the dirty live worktree compiles. The T09a commit must include the core-port change
(or the committed test fixtures must be reverted).

### Validation after reverting only the two T09a contaminants (throwaway copy)

With the three `RecordEntry` fixture fields and the two recorder-id assertions reverted to their
pre-T09a form, and no other change:

| Command | Result |
|---|---|
| `cargo test -p vks-admin --lib --locked` | **338 passed, 0 failed** |
| `cargo test -p vks-server --tests --locked` | **175 passed, 0 failed, 3 ignored** (lib 144, config 1, h265 1, network 22, operator 7) |
| `cargo clippy … --all-targets -- -D warnings` | **exit 0, no warnings** |
| `cargo test -p vks-auth --locked` | **40 passed, 0 failed** |

So the T02 changes themselves do not break the suites; the red state is entirely T09a packaging.
The `vks-server --tests` run on pristine `5d46507` additionally hit two **flaky, load-sensitive**
G.723/G.726 audio tests (142/144 in the lib target) while another agent was running
`cargo test --workspace`; both pass 3/3 in isolation and are not T02-related.

## Item 6 — remaining P2/P3: are any release blockers?

No remaining **T02-code** P0/P1. The P2s below are not release blockers for the auth feature, but
several must be closed before T02 is booked as "done".

| # | Issue | State | Blocker? |
|---|---|---|---|
| P2-3 | `persist_lazy_rehash` re-checks the row, then awaits `upsert_user_credentials`, then reads `generation`/`password_changed_at` (`auth.rs:530-561`) | **OPEN** — same class as before; a concurrent password change can persist a stale hash with the new generation/mark | No (legacy-row + narrow window), but should be fixed |
| P2-4 | Test gaps: no in-tree uniqueness test, no change-password 422 test, no real-`LocalAuth` Busy HTTP test, no storage-level lazy-rehash test | **OPEN/PARTIAL** | No |
| P2-5 | Legacy PBKDF2 verify still ~2.6× slower than the Argon2 dummy (`local_db.rs:348-372`); self-heals after first successful login | **OPEN** | No |
| P2-6 | `profile/credentials` oversized old password → 404 instead of 422 (`accounts.rs:409-412`); `Other` on update routes → 500 | **PARTIAL** | No |
| P2-7 | `hash_password` `.expect()` still present; `rehash_async` still `pub` without an empty/128-byte guard (`local_db.rs:495`, `:402`) | **OPEN** | No (latent) |
| P2-8 | `ldap_sync_enabled` still dead (`state.rs:216`, `:322`) | **OPEN** | No |
| P2-9 | Legacy >128-byte passwords still lock out on login (422, no rotation path/documentation); change-password now 422 on old | **OPEN** (improved) | No |
| P2-10 | Docs/tracker: T02 tracker row still says `fix 8dfc449 … re-review идёт`; `BACKEND_RULES:28-30` says "очередь ожидания ≤ 128" (actual: 128 total in-flight, ≤ 124 waiters); `auth.rs:463` still carries "401 Too many attempts…" on the **429** bucket middleware | **PARTIAL** | No code, but must fix for "done" |
| P2-11 | One process-global pre-auth bucket key (`auth.rs:471-481`) — accepted design risk | **OPEN** | No |
| P2-12 | No test asserts the m5 bucket (fixed at `vks-server/src/lib.rs:195`, no 429 test) | **OPEN** | No |
| NEW-P2 | Cancellation regression test is a false negative (Item 2) | **OPEN** | Blocks the "real regression evidence" condition |
| NEW-P1 | Clean-checkout test suite red (Item 5) | **OPEN at `5d46507` and `650a55f`** | **Yes — merge/CI blocker** |

P3 residuals unchanged: `change_password` returns 500 **after** mutating memory on a non-UUID id
(`auth.rs:405-409`, unreachable in production); `run_gated` maps `JoinError` to `AuthFailed`
(`local_db.rs:128`, now masked as 409 "Login already exists" by `accounts.rs:145-147`);
`bootstrap_operator` fallbacks remain at `users.rs:135` and `operator_policy.rs:67`;
`authenticates_locally()` is provider *type*, not store identity. The `BACKEND_RULES` stray
backtick is fixed.

## Item 7 — docs claims vs code

| Claim | Verdict |
|---|---|
| "PBKDF2 row is lazily rehashed after successful login" (`BACKEND_RULES:25-27`, ADR-0033, module docs) | **TRUE** at `5d46507` (provider-local path; verified in `T02_final.md`, unchanged since) |
| "≤ 128 всего" (ADR-0033:31) | **TRUE** |
| "очередь ожидания ≤ 128 запросов" (`BACKEND_RULES:28-30`) | **FALSE** — the counter caps 128 **total** in-flight (4 running + ≤124 waiting) |
| outermost bucket covers "**всего** `/api/v3`" (`BACKEND_RULES:38-41`) | **TRUE at `5d46507`** — m5 router has the layer at `vks-server/src/lib.rs:195` (it was false in `ddd033b`) |
| `auth.rs:463` "401 Too many attempts…" attached to `global_bucket_middleware` | **FALSE** — the middleware returns 429 "Too many requests" (`too_many_requests_response`, `auth.rs:483-489`) |
| `MASTER_TASK_LIST_2026-10-29.md:12` T02 status | **STALE** — still advertises `8dfc449` and "re-review идёт", although `ddd033b` and `5d46507` landed |
| T02 acceptance line 38 (lazy rehash, 4 permits, 429, ≤128 B, bucket) | Code now meets it; docs/tracker row above does not reflect it |
| Argon2 params / complexity-validation "plan only" / LDAP mock | **TRUE** (unchanged) |
| Stray backtick `BACKEND_RULES:32` | **FIXED** |

## Per-finding verdicts (normalized)

| # | Finding | Verdict | Evidence at `5d46507` |
|---|---|---|---|
| P0-1 | Waiter-counter leak on cancellation | **FIXED** | `QueueSlot` RAII `local_db.rs:64-82`; test `:710-734`; unchanged |
| P1-1 | Typed Busy → 429 through production provider, no lockout/audit | **FIXED** | probe: change-password/profile/update-credentials 429 + `Retry-After: 1` under 200 in-flight hashes |
| P1-2 | Local lazy PBKDF2→Argon2id migration dead in shipped binary | **FIXED** | `core_adapter.rs:63-65`; `api_v3/auth.rs:313-324`; proven in `T02_final.md`, unchanged |
| P1-3 | Gate bypass via sync `register` on request path | **FIXED** | `accounts.rs:139` uses `register_async`; sync `register` still `pub` (nit) |
| P1-4 / P2-1 / P2-12 | m5 `/api/v3` endpoints outside the bucket | **FIXED code, UNTESTED** | `vks-server/src/lib.rs:195`; no 429 test |
| N1 | Permit released before detached hash completes | **FIXED code** | `local_db.rs:116-128`; old shape observed 7 vs fixed ≤ 4 |
| N3 / P2-2 | `register_async` lost login uniqueness; duplicate → 500 | **FIXED** | `local_db.rs:439-444`; probe 20/20 races one 201+409, no 500 |
| P2-2 (old) | 128-byte status codes on credential routes | **PARTIAL** | change-password 422; profile old 404; update routes 429/422/500 |
| P2-3 | Lazy-rehash persist TOCTOU | **OPEN** | `auth.rs:530-561` |
| P2-4 | Missing tests | **PARTIAL** | cancellation test added but ineffective; m5/422/real-provider tests still missing |
| P2-5 | Legacy timing oracle | **OPEN** | ~2.6× (unchanged) |
| P2-6 | Docs/code mismatches | **PARTIAL** | backtick fixed, "всего" now true; queue wording + tracker + stray 401 still wrong |
| P2-7 | `needs_rehash` prefix-only | **FIXED** | parameter-aware (`local_db.rs:377-400`), 20/20 in `T02_final.md` |
| P2-8 | `expect()`/unguarded `rehash_async` | **OPEN** | `local_db.rs:495`, `:402` |
| P2-9 | Dead `ldap_sync_enabled` | **OPEN** | `state.rs:216,322` |
| P2-10 | Legacy >128-byte password lockout | **OPEN** (login/change 422 now, no rotation) | `auth.rs:267-271`, `:365-367` |
| P2-11 | Single global pre-auth bucket key | **OPEN** (documented) | `auth.rs:471-481` |
| NEW-1 | `change_password` 500 after in-memory mutation (non-UUID) | **OPEN** (P3, unreachable) | `auth.rs:405-409` |
| NEW-2 | `run_gated` JoinError → `AuthFailed` | **OPEN** (P3) | `local_db.rs:128`; masked as 409 in `accounts.rs:145-147` |
| NEW-3 | Residual `bootstrap_operator` fallbacks | **OPEN** (P3) | `users.rs:135`, `operator_policy.rs:67` |
| NEW-4 | Provenance by provider type, not store identity | **OPEN** (P3) | `api_v3/auth.rs:292,313-324` |
| NEW-5 | Stray `BACKEND_RULES` backtick | **FIXED** | `BACKEND_RULES:32` |
| **NEW-P1** | **Clean checkout does not compile/run its tests** | **OPEN — blocker** | E0560 `observability_tests.rs:165-167`; recorder assertions `network_integration.rs:815,1576` |
| **NEW-P2** | **Shipped N1 regression test is a false negative** | **OPEN** | passes 5/5 on the old permit shape; fails 3/3 after a pre-abort yield |
| N4 | Single global key, pre-auth | **OPEN** (accepted) | same as P2-11 |

## Explicit counts

- **Open P0: 0.**
- **Open P1 in T02 auth code: 0.** (P0-1, P1-1, P1-2, P1-3, N1, N3 are fixed and verified.)
- **New P1 in the commit under review: 1** — the tree at `5d46507` is not self-consistent:
  `vks-admin` tests/clippy fail to compile (E0560) and two recorder integration tests fail
  deterministically; still broken at `650a55f` because `vks-core` storage changes are uncommitted.
- **New P2 from `5d46507`: 1** — the cancellation regression test does not catch the bug it targets.
- **Open P2: 11** — persist TOCTOU, test gaps (incl. uniqueness/422/m5), timing oracle,
  profile-old-password 404, `expect`/unguarded `rehash_async`, dead `ldap_sync_enabled`,
  >128-byte lockout, docs/tracker, global-key trade-off, m5 bucket test, ineffective
  cancellation test.
- **Open P3: 4** — non-UUID 500-after-mutation, `JoinError`→`AuthFailed` (and 409 masking),
  `bootstrap_operator` fallbacks, provenance by provider type.

## Score: 8.0 / 10

- **+** All prior P0/P1 auth findings are fixed. Uniqueness and the 422 are proven over the real
  router with the real provider; the permit fix is proven against a faithful pre-fix build
  (7 concurrent hashes vs ≤ 4); instrumentation is test-only and absent from the release code;
  40 + 338 + 175 tests pass and clippy is clean once the unrelated T09a contaminants are removed.
- **−** The commit as pushed cannot pass its own required commands on a clean checkout
  (`cargo test -p vks-admin --lib`, `clippy --all-targets`, two recorder integration tests);
  the live tree only looks green because uncommitted T09a code is present, and HEAD `650a55f`
  still needs the `vks-core` storage change.
- **−** The promised N1 regression test is a false negative until it waits for in-flight hashes
  before aborting; the 9.5 gate requires real regression evidence.
- **−** The tracker still advertises `8dfc449`, `BACKEND_RULES` still misstates the queue bound
  and carries a stale "401 Too many attempts" doc line on the 429 middleware.
- **−** P2 residuals unchanged: lazy-rehash persist window, profile-old-password 404, legacy
  timing/lockout, dead LDAP flag, no m5 bucket test.

This is a 9.5-class **auth change** wrapped in a commit that is not mergeable as-is. Re-cut `5d46507` without the
T09a hunks (or commit the T09a core port), add one yield to the cancellation test, and correct
the tracker/queue/401 docs; the score would then be ≥ 9.5 with no open P0/P1.

## Commands run and results

All from a pristine `git archive 5d46507` tree at `/dev/shm/t02r2/5d46507` unless marked
"patched copy", with `export PATH="$HOME/.cargo/bin:$PATH"`, `CARGO_TARGET_DIR` on `/dev/shm`,
`--locked`. The patched copy is `git archive 5d46507` + only the two T09a contaminants reverted
(3 fixture fields, 2 recorder assertions); no T02 file was changed.

| Command | Result |
|---|---|
| `cargo test -p vks-auth --locked` | **40 passed, 0 failed** (10.45 s), includes the new cancellation test |
| `cargo test -p vks-admin --lib --locked` | **FAIL (rc=101)** — E0560 ×3, test target does not compile |
| `cargo test -p vks-server --tests --locked` | **FAIL (rc=101)** — lib 142/144 (2 load-sensitive G.723/G.726 tests), cargo stops before integration |
| `cargo clippy -p vks-auth -p vks-admin -p vks-server --all-targets --locked -- -D warnings` | **FAIL (rc=101)** — same E0560 |
| `cargo build -p vks-auth --release --locked` | **exit 0** (23.72 s) |
| `cargo build -p vks-admin --locked` / `-p vks-server --locked` | **exit 0** (production code compiles) |
| **patched copy:** `cargo test -p vks-admin --lib` | **338 passed, 0 failed** |
| **patched copy:** `cargo test -p vks-server --tests` | **175 passed, 0 failed, 3 ignored** |
| **patched copy:** clippy `--all-targets -- -D warnings` | **exit 0**, no warnings |
| `cargo test -p vks-auth cancelled_hashes_never_exceed_the_permit_limit` (fixed) | **5/5 pass** |
| same test, permit moved outside the closure (old shape) | **5/5 pass — false negative** |
| same test + 20 ms pre-abort sleep, old shape | **3/3 fail**, `observed 7` |
| same test + 20 ms pre-abort sleep, fixed | **3/3 pass** |
| in-process probe, production router + real `LocalAuth` | 20/20 duplicate races `[201, 409]`, both_created 0; direct race ok 20 / dup 20; change-password old/new oversize 422; profile old oversize 404; profile/update Busy 429 `Retry-After: 1` |
| `cargo test -p vks-server --lib --locked -- gateway_audio_bridges_g72` (isolated) | **3/3 green**, 144 filtered — the two failures are load flakes, not regressions |
| live DB `md5sum` / `stat` | `6a2344…` and mtime 07:33:29, unchanged; no connection opened |

### Limitations

- No live server/socket was started; HTTP behaviour was exercised in-process through
  `tower::oneshot`, as the repo's own tests do.
- The old-shape experiment is a faithful source model of the pre-`ddd033b` `run_gated`, not a
  `git revert` (the test-only instrumentation has to stay to make the assertion possible).
- The two audio-test failures were observed while another agent ran `cargo test --workspace`; they
  pass 3/3 in isolation and are unrelated to T02, but a full-suite run on a clean `5d46507` also
  fails earlier on the E0560 compile error, so the command is red regardless.
- `650a55f` was inspected statically (`git show`/`git diff`); its test suite was not rebuilt.

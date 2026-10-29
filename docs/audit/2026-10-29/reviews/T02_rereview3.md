# T02 re-review #3 — commit `9c17173`

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **Commit under review:** `9c17173eba1ff23d3dfd05c4e03b466b0fffd5a0` "fix(auth): T02 re-review #2 —
  make cancellation test real, clean docs" (2026-10-29 11:03:44 +0300). Chain since the prior
  review: `5d46507` → `650a55f` (T09a recorder/storage) → `8af77db` (T11a + `vks-core`
  `RecordEntry` fields) → `9c17173`.
- **Prior reviews read:** `T02.md` (6.0), `T02_adversarial.md` (4.0), `T02_rereview.md` (8.0),
  `T02_final.md` (9.0), `T02_rereview2.md` (8.0).
- **Role:** sixth, independent, narrow final re-review. No source file in the live repository was
  modified. All builds, probes and the permit experiment were run in a pristine `git archive HEAD`
  tree under `/dev/shm/t02r3`; the temporary probe test file existed only in that scratch tree and
  was removed. The only file written in the live repo is this report.
- **HEAD:** `9c17173` throughout. The earlier clean-checkout P1 is genuinely closed: the archive
  contains `vks_core::ports::storage::RecordEntry { bitrate_kbps, width, height }` and the
  recorder implementation, so no dirty-worktree dependency remains.
- **Live DB:** untouched. `md5sum vks_data.db` = `6a23442016492077f311becf14badace` and mtime
  `2026-10-29 07:33:29.801017417 +0300` are unchanged; no server was started and no SQLite
  connection was opened against it.
- **Live-worktree note:** the worktree was clean when this review started (`git status --short`
  empty). Mid-review another concurrent agent modified several unrelated T09a/T04 files
  (`observability.rs`, `observability_tests.rs`, `recorder.rs`, `resource_tests.rs`,
  `resources.rs`, `grid.rs`, `grids.rs`) and added `docs/audit/.../T09a.md`; these are not T02,
  were not made by this review, and do not change HEAD or the archive evidence below.

## Verdict

**Score: 9.0 / 10. T02 cannot be marked done yet.**

Demands (1) and (2) from `T02_rereview2.md` are now met with real evidence: the clean checkout is
green and the cancellation test is a genuine regression test. Demand (3) is **not met**: two of the
three named docs were not touched by `9c17173` and still contradict the code.

- **Open P0: 0. Open P1: 0** (including T02 auth code and the previous clean-checkout P1).
- **Open P2: 10. Open P3: 4.**
- No new P0/P1 was found. The only blocker to "done" is the docs/claims gate plus the pre-existing
  P2 residuals; the auth code itself is 9.5-class.

Confirmed from this review:

1. **Clean-tree gates are green.** `git archive HEAD`, no source edits:
   `vks-auth` 40/40, `vks-admin --lib` 344/344, `vks-storage` 24/24, `vks-server --tests`
   175 passed / 3 ignored, clippy `--workspace --all-targets -D warnings` exit 0. No flake was
   hit even while another agent was compiling/running its own suite.
2. **The cancellation test is real.** With only the permit placement reverted (instrumentation
   unchanged), `cancelled_hashes_never_exceed_the_permit_limit` fails **3/3**, observed **6**
   concurrent hashes; restored fixed code passes **3/3**. The commit message says 7; this host
   observed 6, which still violates the ≤ 4 invariant and fails the test.
3. **Auth behaviour is still 9.5-class.** 20/20 concurrent duplicate registrations produced
   exactly `{201, 409}` (0 double-201, 0 500); Busy → 429 + `Retry-After: 1` and recovery to 200;
   oversized login/old/new password → 422; a PBKDF2 row logged in through the production
   `LocalAuth` path, migrated to `$argon2id$`, kept generation 5 and mark 11, and persisted the
   upgraded credentials to storage. Release object code contains no test-only counters.
4. **Docs do not match reality.** `9c17173` changed only `local_db.rs`, the prior review file and
   the tracker row. `BACKEND_RULES:28-30` still says "≤ 4 hashes + wait queue ≤ 128" (actual:
   128 total in flight, i.e. ≤ 4 running + ≤ 124 waiting), and `auth.rs:463` still documents the
   429 bucket middleware as "401 Too many attempts…". The tracker row was updated but still ends at
   `5d46507` and does not name `9c17173` (or `8af77db`, which closed the packaging P1).

## Item 1 — clean checkout is self-contained (demand 1) — **PASS**

Tree: `/dev/shm/t02r3/head` = `git archive HEAD | tar -x`, `CARGO_TARGET_DIR` on `/dev/shm`,
`--locked`, no source edit. All commands were run sequentially from that tree.

| Command | Result | rc |
|---|---|---|
| `cargo test -p vks-auth --locked` | **40 passed, 0 failed** (includes the cancellation test) | 0 |
| `cargo test -p vks-admin --lib --locked` | **344 passed, 0 failed** (32.81 s) | 0 |
| `cargo test -p vks-storage --locked` | **24 passed, 0 failed** (23 lib + 1 `dedup_lock`) | 0 |
| `cargo test -p vks-server --tests --locked` | **175 passed, 0 failed, 3 ignored** — lib 144, config 1, h265 1, network 22, operator 7 | 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **Finished, no warnings** (61 s) | 0 |

The previous `E0560 RecordEntry` failure is gone: `crates/vks-core/src/ports/storage.rs:131` in
the archive has `bitrate_kbps` (and `width`/`height`), and the recorder id assertions of
`network_integration.rs` now pass. No unrelated flake occurred during this run. This closes
`T02_rereview2` NEW-P1.

## Item 2 — cancellation test is a real regression test (demand 2) — **PASS**

Shipped test (`local_db.rs:762-804`) now sleeps **20 ms** after spawning the six
`authenticate_async` tasks and before aborting the first three (`local_db.rs:780`), then sleeps
50 ms before the six-task storm. That is the pre-abort wait required by the prior review.

### Experiment

A second copy of the same archive was edited **only** in `run_gated`: the `(QueueSlot,
OwnedSemaphorePermit)` pair stays in the request future and is released when the future is aborted
(`let gate = hash_permit().await?; let result = spawn_blocking(...).await; let _gate = gate;
result`). The `ACTIVE_HASHES` / `MAX_CONCURRENT_HASHES` instrumentation inside the closure was kept
byte-identical. No other change.

| Implementation | Runs | Result |
|---|---|---|
| pre-fix permit placement (permit outside the closure) | 3 | **3/3 FAILED**, `at most 4 concurrent hashes allowed, observed 6` |
| shipped fixed placement (file restored, sha256 equal to archive) | 3 | **3/3 pass**, max ≤ 4 |

Evidence: `test result: FAILED. 0 passed; 1 failed` at `local_db.rs:802` on all three buggy runs;
`test result: ok. 1 passed` on all three fixed runs. The old shape is now caught, so the claim in
the commit message is true in substance. Minor doc nit: the message says "observed 7"; this host
observed 6 in all three runs. The test still has no explicit `ACTIVE_HASHES > 0` assertion at abort
time, but the 20 ms wait plus the observed 3/3 failure makes it non-vacuous in practice.

## Item 3 — auth end-to-end behaviour (demand 3 evidence, behaviour itself) — **PASS**

Probe: real `vks_admin::api_v3::router`, `AppState` with `LocalAuth::with_db` (production provider
wiring), real JWT issuer, `tower::oneshot`; `MemoryStorage` for persistence. Run from the scratch
tree; the probe test file was deleted afterwards. Raw probe output:

```
DUP_RACES n=20 exact=[201,409]_in_any_order=true both_created=0 any_500=0 sets=[[201, 409], ... 20×]
LOGIN_OVERSIZE status=422 body={"error":{"code":422,"description":"Password is too long"}}
PATCH_OLD_OVERSIZE status=422 body={"error":{"code":422,"description":"Old password is too long"}}
PATCH_NEW_OVERSIZE status=422 body={"error":{"code":422,"description":"New password is too long"}}
LEGACY_LOGIN status=200 token_present=true
LEGACY_MIGRATION before_prefix=49d49c25f597 after_prefix=$argon2id$v= needs_rehash_after=false
  gen_before=5 gen_after=5 mark_before=11 mark_after=11 stored=Some((5, Some(11), true))
BUSY_LOGIN status=429 retry_after=Some("1") body={"error":{"code":429,"description":"Authentication is temporarily busy"}}
POST_BUSY_LOGIN status=200
```

- **Uniqueness:** 20 iterations, two different directory users racing the same login; every
  iteration yielded exactly one 201 and one 409, never two 201 and never 500; the winning 201
  always created exactly one LocalDb row.
- **Oversized passwords:** login 422, change-password old 422, change-password new 422.
- **Backpressure:** with the real hash gate saturated (200 in-flight calls), login returned
  **429 + `Retry-After: 1`**; after the gate drained, the same login returned 200. (Only the login
  route was exercised in this narrow pass; the change-password/profile routes share the typed
  `Busy` mapping.)
- **Legacy migration:** PBKDF2 fixture logged in through `LocalAuth`, in-memory hash became
  `$argon2id$…`, `needs_rehash` false, generation stayed 5, `password_changed_at` stayed 11, and
  `MemoryStorage` received `generation=5`, `password_changed_at=Some(11)`, Argon2 hash. The JWT
  carried generation 5, so no token invalidation was caused by the migration.
- **Release instrumentation:** `cargo build -p vks-auth -p vks-admin --release --locked` exit 0
  (2 m 19 s). In the release `libvks_auth` object member the only related string is the production
  `HASH_WAITERS`; `ACTIVE_HASHES`, `MAX_CONCURRENT_HASHES`, `ActiveHashGuard` appear **0 times in
  object code** (they occur once each only in `lib.rmeta`, which is compiler metadata, as observed
  by the prior review). Test instrumentation does not affect the release build.

## Item 4 — docs match reality (demand 3) — **FAIL**

`git show --name-only 9c17173` is exactly:

```
crates/vks-auth/src/local_db.rs
docs/audit/2026-10-29/reviews/T02_rereview2.md
docs/progress/MASTER_TASK_LIST_2026-10-29.md
```

`BACKEND_RULES.md` and `api_v3/auth.rs` are not in the commit. At HEAD:

| Claim / place | HEAD reality | Verdict |
|---|---|---|
| Commit message: "BACKEND_RULES queue wording: 128 total in flight" | `docs/rules/BACKEND_RULES.md:28-30` still reads "одновременно ≤ 4 Argon2-хеша (семафор) + очередь ожидания ≤ 128 запросов" | **FALSE / unchanged.** `AUTH_QUEUE_CAPACITY = 128` counts running + waiting (`HASH_WAITERS` is incremented before the semaphore), so max total in flight is 128, not 4 + 128. ADR-0033:31 says "≤ 128 всего", which is true. |
| Commit message: "429 bucket middleware comment no longer says 401" | `crates/vks-admin/src/api_v3/auth.rs:463` still says "401 `Too many attempts…` + `Retry-After`" directly above `global_bucket_middleware`; that middleware returns `error_json(429, "Too many requests")` (`:483-489`) | **FALSE / unchanged.** `git blame` still attributes line 463 to `ddd033b`. |
| Tracker T02 line | `docs/progress/MASTER_TASK_LIST_2026-10-29.md:12`: `✅ fix \`8dfc449\`..\`5d46507\` (ревью 6.0/4.0 → 8.0/8.0; финальные P1 закрыты)` | **PARTIAL/STALE.** The row was touched, but the range ends at `5d46507`, before `8af77db` (which closed the clean-checkout P1) and before `9c17173` (the commit that makes the cancellation test real). It advertises "final P1 closed" for a range that does not contain the closing commit and does not mention HEAD. |
| ADR-0033 queue/total bound | `docs/decisions/0033-argon2id-password-hashing.md:31` "≤ 128 всего" | **TRUE.** |
| "PBKDF2 lazily rehashed without generation bump" (`BACKEND_RULES:25-27`) | verified again through production `LocalAuth` | **TRUE.** |
| Outermost bucket covers all `/api/v3` (`BACKEND_RULES:39-43`) | `vks-server/src/lib.rs:200` installs the middleware on the merged `/api/v3` router | **TRUE.** |

Because item (3) of the explicit "before done" list is only one-third satisfied, the docs demand
is not met. This is a P2-level issue, not a code/security P1.

## Item 5 — remaining P2/P3 (no new P0/P1)

| # | Severity | Issue | State | Evidence |
|---|---|---|---|---|
| P2-3 | P2 | `persist_lazy_rehash` re-checks the row, awaits `upsert_user_credentials`, then reads `generation`/`password_changed_at`; a concurrent password change can persist a stale hash with the newer mark | OPEN | `api_v3/auth.rs:530-561` unchanged since `5d46507` |
| P2-4 | P2 | Test gaps: no in-tree uniqueness, change-password 422, real-provider Busy HTTP, or storage-level lazy-rehash test; cancellation test lacks an explicit non-vacuity assertion | OPEN/PARTIAL | probe evidence is external only; `local_db.rs:762-804` |
| P2-5 | P2 | Legacy PBKDF2 verify still ~2.6× slower than the Argon2 dummy; self-heals after first success | OPEN | `local_db.rs:348-372` unchanged |
| P2-6 | P2 | `profile/credentials` oversized old password → 404 instead of 422; `Other` on update routes → 500 | OPEN | `directory/accounts.rs:409-412` (`Err(_) => 404`) |
| P2-7 | P2 | `hash_password().expect()` still present; public `rehash_async` still lacks an empty/128-byte guard | OPEN | `local_db.rs:495`, `:402-404` |
| P2-8 | P2 | `ldap_sync_enabled` is never set true (read at `handlers.rs:554`), so `sync-ldap` always returns 501 even with a real LDAP provider | OPEN | `state.rs:228,336`; no assignment anywhere |
| P2-9 | P2 | Legacy >128-byte passwords still lock out on login (422, no rotation path/documentation) | OPEN (improved) | `api_v3/auth.rs:267-271` |
| P2-10 | P2 | Docs/tracker mismatches above: queue wording, 401 comment, stale tracker range | OPEN | Item 4 |
| P2-11 | P2 | One process-global pre-auth bucket key (`"global"`), accepted design risk | OPEN (accepted) | `api_v3/auth.rs:471-481` |
| P2-12 | P2 | No test asserts the m5 `/api/v3` bucket returns 429 | OPEN | layer at `vks-server/src/lib.rs:200`, no test |
| P3-1 | P3 | `change_password` returns 500 **after** mutating memory on a non-UUID id; unreachable in production | OPEN | `api_v3/auth.rs:405-408` |
| P3-2 | P3 | `run_gated` maps `JoinError` to `AuthFailed`, which `create_user_credentials` masks as 409 "Login already exists" | OPEN | `local_db.rs:128`; `accounts.rs:145-147` |
| P3-3 | P3 | Residual `bootstrap_operator` fallback ids | OPEN | `users.rs:135`; `vks-server/src/operator_policy.rs:67` |
| P3-4 | P3 | Provenance decided by provider *type* (`authenticates_locally()`), not store identity | OPEN | `api_v3/auth.rs:292,313-324` |

No new P0/P1 was introduced by `9c17173`. The cancellation false negative (NEW-P2 in the prior
review) and the clean-checkout red tree (NEW-P1) are both closed.

## Explicit counts

- **Open P0: 0.**
- **Open P1: 0** — no T02 auth P1 and no clean-checkout P1.
- **Open P2: 10** — persist TOCTOU, test gaps, timing oracle, profile-old 404, `expect`/unguarded
  `rehash_async`, dead LDAP flag, >128-byte lockout, docs/tracker, global-key trade-off, m5 bucket
  untested.
- **Open P3: 4** — non-UUID 500-after-mutation, `JoinError`→`AuthFailed` (and 409 masking),
  bootstrap fallbacks, provenance by provider type.
- **New in this review: 0 P0, 0 P1, 0 code P2.** The remaining P2/P3 are the pre-existing set;
  the docs requirement is P2-10.

## Score: 9.0 / 10

- **+** Clean checkout green on all five required commands with zero edits; previous packaging P1
  genuinely closed.
- **+** The N1 regression test now fails 3/3 on the old permit placement (observed 6) and passes
  3/3 on the fix — real evidence, not a false negative.
- **+** Auth behaviour is 9.5-class end-to-end: 20/20 uniqueness races, typed Busy 429 with
  `Retry-After`, 422 length handling, production-path PBKDF2 migration without generation/mark
  changes, and clean release instrumentation.
- **−** Demand (3) fails: `BACKEND_RULES:28-30` and the 429 comment at `auth.rs:463` were not
  changed at all by `9c17173`, and the tracker row stops at `5d46507` and omits the commits that
  actually closed the P1 / made the test real. The commit message's "clean docs" claim is therefore
  false.
- **−** Ten pre-existing P2 residuals remain, several of which (lazy-rehash persistence window,
  profile-old 404, missing tests) are the same class the T02 reviews have flagged since `T02_final`.

The code and regression evidence meet the ≥ 9.5 bar; the explicit completion criteria do not.
Fix the two doc lines and the tracker row, and this becomes a 9.5 with no open P0/P1.

## Commands run and results

From `/dev/shm/t02r3/head` (pristine `git archive 9c17173`), `CARGO_TARGET_DIR=/dev/shm`,
`--locked`, unless noted.

| Command | Result |
|---|---|
| `cargo test -p vks-auth --locked` | **40 passed, 0 failed** |
| `cargo test -p vks-admin --lib --locked` | **344 passed, 0 failed** |
| `cargo test -p vks-storage --locked` | **24 passed, 0 failed** |
| `cargo test -p vks-server --tests --locked` | **175 passed, 0 failed, 3 ignored** |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **rc=0, no warnings** |
| buggy-permit copy: `cargo test -p vks-auth cancelled_hashes_never_exceed_the_permit_limit` ×3 | **3/3 FAILED**, observed 6 |
| restored fixed file: same test ×3 | **3/3 pass** |
| `cargo test -p vks-admin --test t02_rr3_probe --nocapture` (scratch probe, removed afterwards) | 20/20 `{201,409}`, 422×3, 429+Retry-After, legacy gen/mark unchanged, persistence ok |
| `cargo build -p vks-auth -p vks-admin --release --locked` | **rc=0** (2 m 19 s) |
| release rlib `strings` (object member vs `.rmeta`) | object: `HASH_WAITERS` only; test counters only in rmeta |
| `md5sum`/`stat` live `vks_data.db` | `6a2344…`, mtime `07:33:29`, unchanged |

### Limitations

- No live server/socket was started; HTTP behaviour was exercised in-process through
  `tower::oneshot`, as the repo's own tests do.
- The old-permit experiment is a faithful source model of the pre-fix `run_gated`, not a
  `git revert` (the test-only instrumentation must stay for the assertion to work).
- The Busy probe exercised the login route (the shared typed-`Busy` path); the prior review already
  exercised change-password/profile/update-credentials.
- The probe was an external integration test in the scratch tree; it is not committed, so the
  in-tree test-gap finding (P2-4) remains.

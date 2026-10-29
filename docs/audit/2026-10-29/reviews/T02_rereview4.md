# T02 re-review #4 — narrow final certification at `661a2cb`

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`, **HEAD `661a2cb`**
  (`661a2cb4c85a2bf73a961e84040c37b8d82f7a29`, “fix(layout): T06 review pass …”).
- **Scope:** narrow certification of T02 (auth) after `c340581` claimed to close the three doc
  demands that blocked `T02_rereview3.md` (9.0/10, 0 open P0/P1).
- **Prior reports read:** `T02.md` (6.0), `T02_adversarial.md` (4.0), `T02_rereview.md` (8.0),
  `T02_final.md` (9.0), `T02_rereview2.md` (8.0), `T02_rereview3.md` (9.0).
- **Method:** pristine `git archive 661a2cb` tree at `/dev/shm/t02r4/head` (not `/tmp`), shared
  `CARGO_TARGET_DIR=/dev/shm/t02r4/target`; no source edit in that tree. Two disposable copies
  were used only for the behaviour probe and the permit experiment; the whole `/dev/shm/t02r4`
  tree was removed after the run. The only file written in the live repo is this report.
- **Live DB:** untouched. `md5sum vks_data.db` = `6a23442016492077f311becf14badace`, size
  `3248128`, mtime `2026-10-29 07:33:29.801017417 +0300` — identical before and after; no server
  was started and no SQLite connection was opened against it.

## Verdict

**Score: 9.5 / 10. T02 is DONE.**

- **Open P0: 0. Open P1: 0. Open P2: 9. Open P3: 4.**
- All three doc demands are fixed at `661a2cb` (Item 1), the clean checkout is green on all five
  required commands (Item 2), and T02 behaviour is unchanged since `9c17173` (Item 3).
- **New in this review: 0 P0, 0 P1, 0 P2, 0 P3.** The former doc blocker `P2-10` is closed.
  The remaining 9 P2 / 4 P3 are the pre-existing residuals listed in Item 4.

## Item 1 — the three doc demands are fixed — **PASS**

`c340581` (“docs(auth): T02 re-review #3 — fix queue wording, 429 comment, tracker row”) changes
exactly four files: `crates/vks-admin/src/api_v3/auth.rs`, `docs/rules/BACKEND_RULES.md`,
`docs/progress/MASTER_TASK_LIST_2026-10-29.md`, and the prior review file. Current `661a2cb`
lines:

| Demand | Current line(s) at `661a2cb` | Verdict |
|---|---|---|
| BACKEND_RULES queue wording: 128 **total** in flight, not 4 + a separate 128-slot queue | `docs/rules/BACKEND_RULES.md:28-31`: “одновременно ≤ 4 Argon2-хеша (семафор), всего в полёте ≤ 128 запросов (4 выполняются + ≤124 ждут; permit удерживается внутри blocking-задачи, отмена не раздувает конкурентность) → при переполнении `429`/`Retry-After: 1`.” | **FIXED.** Matches `local_db.rs`: `AUTH_QUEUE_CAPACITY = 128` (`:42`), `QueueSlot::acquire` increments `HASH_WAITERS` before the semaphore and rejects at `>= 128` (`:70-72`), so running + waiting ≤ 128 with running ≤ 4. |
| `auth.rs` “401 Too many attempts” comment above the 429 middleware | `crates/vks-admin/src/api_v3/auth.rs:463`: “`/// 429 `Too many requests…` + `Retry-After` (sec, minimum 1).`” — immediately above `global_bucket_middleware`, which returns `too_many_requests_response()` (`429`, `Retry-After: 1`, `:472-489`). | **FIXED.** The comment now names the status the middleware actually emits. |
| Tracker T02 row omits `8af77db` / `9c17173` | `docs/progress/MASTER_TASK_LIST_2026-10-29.md:12`: “`✅ fix \`8dfc449\`..\`9c17173\` (ревью 6.0/4.0 → 8.0/8.0/9.0; P0/P1 закрыты, clean checkout зелёный)`” | **FIXED.** The range now ends at `9c17173`; `git merge-base --is-ancestor 8af77db 9c17173` is true, and `8af77db` (the commit that closed the clean-checkout P1 by adding the `RecordEntry` fields) is inside the advertised range. It is covered by the range endpoint rather than named separately; that is accurate, not stale. |

`git diff 9c17173 661a2cb -- crates/vks-auth crates/vks-admin/src/api_v3/auth.rs` contains
**one line only**: the comment change above (1 insertion / 1 deletion). No auth logic changed
between the commit that earned 9.0 and HEAD.

## Item 2 — clean checkout is green — **PASS**

Tree: `/dev/shm/t02r4/head` = `git archive 661a2cb | tar -x`; `--locked`; no source edits. Files
were verified against `git show 661a2cb:…` hashes (`local_db.rs` =
`64c2c0a2…e9b3`, `auth.rs` = `52bd4bef…41ec`); `find -newer Cargo.toml` returned no Rust file.
All commands were run sequentially in that tree.

| Command | Result | rc |
|---|---|---|
| `cargo test -p vks-auth` | **40 passed, 0 failed** (9.09 s; includes `cancelled_hashes_never_exceed_the_permit_limit`) | **0** |
| `cargo test -p vks-admin --lib` | **347 passed, 0 failed** (33.74 s; prior 344, +3 are the T06 layout/observability tests, not T02) | **0** |
| `cargo test -p vks-storage` | **24 passed, 0 failed** (23 lib + 1 `dedup_lock`) | **0** |
| `cargo test -p vks-server --tests` | **176 passed, 0 failed, 3 ignored** — lib 144, config 1, h265 1, network 23, operator 7 (prior 175; network +1 from T06, unrelated to T02) | **0** |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **Finished, 0 warnings** (1 m 17 s) | **0** |

No flake occurred, so no isolated rerun was needed. `grep -c "warning:" gates.log` = 0.

## Item 3 — T02 behaviour unchanged since `9c17173` — **PASS**

### 3.1 Structural invariance

`git diff 9c17173 661a2cb -- crates/vks-auth crates/vks-admin/src/api_v3/auth.rs` is the
single comment line quoted in Item 1; `crates/vks-auth` has **zero** changes. The auth code path
(`register_async`, `run_gated`, login/change-password handlers, lazy rehash) is byte-identical to
the commit whose behaviour was probed in `T02_rereview3.md`.

### 3.2 HTTP probe on the pristine archive

Scratch integration test against the real `vks_admin::api_v3::router` with
`LocalAuth::with_db` (production provider wiring), real `JwtIssuer`, `MemoryStorage`; external to
the gate tree and removed after the run. Raw output:

```
DUP_RACES n=20 exact=[201,409]_every_iteration=true
DUP_SETS [[201, 409], [201, 409], … 20×]
DUP_ROWS_CREATED all=true
BUSY status=429 Too Many Requests retry_after=Some("1") body={"error":{"code":429,"description":"Authentication is temporarily busy"}}
POST_BUSY status=200 OK token=true
LOGIN_OVERSIZE status=422 Unprocessable Entity body={"error":{"code":422,"description":"Password is too long"}}
PATCH_OLD_OVERSIZE status=422 Unprocessable Entity body={"error":{"code":422,"description":"Old password is too long"}}
LEGACY_LOGIN status=200 OK token_present=true
LEGACY_MIGRATION before_prefix=49d49c25f5978462 after_prefix=$argon2id$ needs_rehash_after=false gen_before=5 gen_after=5 mark_after=11 token_gen=5 stored=(gen=5, mark=Some(11), argon=true)
PATCH_NEW_OVERSIZE status=422 Unprocessable Entity body={"error":{"code":422,"description":"New password is too long"}}
REGISTER_OVERSIZE status=422 Unprocessable Entity body={"error":{"code":422,"description":"Password is too long"}}
test result: ok. 3 passed; 0 failed; 0 ignored
```

- **Concurrent duplicate registration:** 20 independent races (two different directory users,
  same login, real `POST /api/v3/users/{id}/credentials` through the router). Every iteration
  produced exactly one `201` and one `409`, in both orders; never two `201`, never `500`; the
  winning `201` always left exactly one LocalDb row. This matches `register_async`’s
  check-and-insert under one lock (`local_db.rs:435-447`) and the endpoint’s `409` mapping
  (`directory/accounts.rs:139-147`).
- **Busy → 429:** with the real hash gate saturated (200 direct `authenticate_async` calls),
  login returned `429` + `Retry-After: 1` with the typed “temporarily busy” body; after the gate
  drained the same login returned `200` with a token. No lockout was recorded
  (`auth.rs:333-337`).
- **Oversized → 422:** login, change-password old, change-password new, and credential
  registration all returned `422` with the matching descriptions (`auth.rs:267-271,365-366,
  377-381`; `directory/accounts.rs:143`).
- **PBKDF2 lazy migration:** a legacy PBKDF2 row (`49d49c25…`, salt `000102…0f`,
  generation 5, mark 11) logged in through the production `LocalAuth` provider path; the stored
  hash became `$argon2id$…`, `needs_rehash` became false, generation stayed **5**, mark stayed
  **11**, the JWT carried generation **5**, and `MemoryStorage` received Argon2 credentials with
  `generation=5` and `password_changed_at=Some(11)`. No token invalidation was caused by the
  upgrade (`auth.rs:512-561`).

### 3.3 Cancellation regression test is still real

The shipped test (`local_db.rs:762-804`) sleeps 20 ms before aborting three of six in-flight
logins, then 50 ms before storming the gate, and asserts `MAX_CONCURRENT_HASHES <= 4`.

A disposable copy of the archive was changed **only** in `run_gated`: the `(QueueSlot,
OwnedSemaphorePermit)` was moved back outside the `spawn_blocking` closure (permit dropped when
the request future is aborted), leaving the `ACTIVE_HASHES`/`MAX_CONCURRENT_HASHES`
instrumentation byte-identical. Results:

| Implementation | Runs | Result |
|---|---|---|
| permit outside the blocking task (pre-fix shape) | 3 | **3/3 FAILED** — observed **7**, **6**, **7** concurrent hashes; `at most 4 concurrent hashes allowed` |
| shipped code restored (`sha256` equal to archive) | 3 | **3/3 pass**, rc 0 each |

The test therefore fails on the old permit placement and passes on the fixed one — a genuine
regression test, not a false negative.

## Item 4 — no new P0/P1; remaining P2/P3

**New in this review: 0 P0, 0 P1, 0 P2, 0 P3.** `P2-10` (docs/tracker) is now closed. The rest
are the pre-existing residuals; counts: **9 P2, 4 P3**.

| # | Severity | Issue | State at `661a2cb` | Evidence |
|---|---|---|---|---|
| P2-3 | P2 | `persist_lazy_rehash` re-checks the row, awaits `upsert_user_credentials`, then reads `generation`/`password_changed_at`; a concurrent password change can persist a stale hash with the newer mark | OPEN | `api_v3/auth.rs:530-561` |
| P2-4 | P2 | In-tree test gaps: no HTTP uniqueness race, no change-password 422 test, no real-`LocalAuth` Busy HTTP test, no storage-level lazy-rehash test; the cancellation test has no explicit `ACTIVE_HASHES > 0` assertion at abort time | OPEN (probe evidence is external only) | `local_db.rs:762-804`; `tests.rs:399` uses a mock Busy provider, not the real gate |
| P2-5 | P2 | Legacy PBKDF2 verify is still slower than the Argon2 dummy; self-heals after the first success | OPEN | `local_db.rs:501-511` |
| P2-6 | P2 | `profile/credentials` oversized old password → `404` instead of `422`; `Other` on update routes → `500` | OPEN | `directory/accounts.rs:409-412` (`Err(_) => 404`) |
| P2-7 | P2 | `hash_password(...).expect(...)` still present; public `rehash_async` still has no empty/128-byte guard | OPEN | `local_db.rs:495`, `:402-404` |
| P2-8 | P2 | `ldap_sync_enabled` is never set true (read at `handlers.rs:554`), so `sync-ldap` always returns 501 even with a real LDAP provider | OPEN | `state.rs:228,336`; no assignment anywhere |
| P2-9 | P2 | Legacy >128-byte passwords still lock out on login (`422`, no rotation path/documentation) | OPEN (improved) | `api_v3/auth.rs:267-271` |
| P2-11 | P2 | One process-global pre-auth bucket key (`"global"`), accepted design risk | OPEN (accepted) | `api_v3/auth.rs:471-481` |
| P2-12 | P2 | No test asserts the **m5** `/api/v3` merged router returns 429 from the shared bucket; the api_v3-level burst test exists but does not exercise the m5 mount | OPEN | `vks-server/src/lib.rs:198-208`; `api_v3/tests.rs:351` |
| P3-1 | P3 | `change_password` returns 500 **after** mutating memory on a non-UUID id; unreachable in production | OPEN | `api_v3/auth.rs:405-408` |
| P3-2 | P3 | `run_gated` maps `JoinError` to `AuthFailed`, which `create_user_credentials` masks as `409` “Login already exists” | OPEN | `local_db.rs:128`; `directory/accounts.rs:145-147` |
| P3-3 | P3 | Residual `bootstrap_operator` fallback ids | OPEN | `users.rs:135`; `vks-server/src/operator_policy.rs:67` |
| P3-4 | P3 | Provenance decided by provider **type** (`authenticates_locally()`), not store identity | OPEN | `api_v3/auth.rs:292` |

`P2-10` (queue wording, 401 comment, stale tracker range) is **CLOSED** by `c340581`.

## Score

**9.5 / 10.**

- **+** All five required clean-tree gates are green with zero source edits and exit code 0 on
  every command.
- **+** The three doc demands are actually fixed: the queue semantics now read “128 total in
  flight (4 running + ≤124 waiting)”, the middleware comment says 429, and the tracker range ends
  at `9c17173` (covering `8af77db`).
- **+** T02 auth source is byte-identical to `9c17173` apart from that comment; the end-to-end
  probe reproduces 20/20 `[201,409]` races, Busy 429 + `Retry-After: 1`, four 422 length paths,
  and a generation/mark-preserving PBKDF2 migration persisted to storage.
- **+** The cancellation test remains a real regression test: 3/3 failures on the old permit
  placement (observed 6–7 versus the ≤ 4 invariant) and 3/3 passes on the shipped code.
- **−** Nine pre-existing P2 residuals (lazy-rehash persistence window, in-tree test gaps,
  PBKDF2 timing, profile-old 404, `expect`/unguarded `rehash_async`, dead `ldap_sync_enabled`,
  >128-byte legacy lockout, global bucket key, untested m5 bucket) remain; none is a P0/P1 and
  none blocks certification.
- **−** Four P3 residuals remain (non-UUID 500-after-mutation, `JoinError`→`AuthFailed`/409
  masking, bootstrap fallbacks, provenance by provider type).

## Commands and environment

| Command | Result | rc |
|---|---|---|
| `git archive 661a2cb \| tar -x -C /dev/shm/t02r4/head` | pristine tree; no `.git`; hashes equal to `git show` | 0 |
| `cargo test -p vks-auth --locked` | 40 passed / 0 failed | 0 |
| `cargo test -p vks-admin --lib --locked` | 347 passed / 0 failed | 0 |
| `cargo test -p vks-storage --locked` | 23 lib + 1 `dedup_lock` = 24 passed | 0 |
| `cargo test -p vks-server --tests --locked` | 176 passed / 0 failed / 3 ignored | 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 warnings | 0 |
| probe test `cargo test -p vks-admin --test t02_rr4_probe -- --nocapture --test-threads=1` | 3/3 passed (scratch copy only) | 0 |
| buggy permit: same cancellation test ×3 | 3/3 failed, observed 7/6/7 | 101 |
| restored fixed file: same test ×3 | 3/3 passed | 0 |
| `md5sum` / `stat` live `vks_data.db` | `6a2344…`, size `3248128`, mtime `07:33:29.801017417`, unchanged | — |

### Limitations and notes

- No live server or socket was started; HTTP behaviour was exercised in-process through
  `tower::oneshot`, as the repo’s own tests do.
- The old-permit experiment is a faithful source model of the pre-fix `run_gated`, not a
  `git revert` (the test-only instrumentation must stay for the assertion to work).
- The probe and experiment trees were separate disposable copies of the archive; the gate tree
  itself was never edited and was verified against the archive hashes. All scratch trees were
  removed.
- While this review ran, another concurrent agent modified unrelated files in the live worktree
  (`client/vks-client-sdk/*`, `crates/vks-proto/proto/signaling.proto`,
  `crates/vks-server/src/lib.rs`, `crates/vks-server/tests/network_integration.rs`,
  `crates/vks-signaling/src/handler.rs`, plus the pre-existing `docs/.../pitfalls/T06.md`).
  Those are not T02, were not made by this review, and do not change HEAD (`661a2cb`) or the
  archive evidence above.
- This review wrote only `docs/audit/2026-10-29/reviews/T02_rereview4.md`.

# T02 final adversarial review — commit `ddd033b`

- **Repo:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **Commit under review:** `ddd033b` "fix(auth): T02 re-review — cancellation-safe hash gate, real
  LocalAuth lazy migration" (2026-10-29 09:17:03 +0300). Chain: `eb514e2` (feature) → `8dfc449`
  (first fixes) → `ddd033b` (second fixes). HEAD at review time: `a33e195` (WebRTC/T11a, *after*
  `ddd033b`).
- **Prior reviews read:** `docs/audit/2026-10-29/reviews/T02.md` (6.0),
  `T02_adversarial.md` (4.0), `T02_rereview.md` (8.0 — 2 open P1 + 11 P2).
- **Role:** fourth, independent adversarial reviewer. **No source file was modified.** The only
  file written in the repository is this report. All harnesses are throwaway projects in `/tmp`
  (`/tmp/t02_gate`, `/tmp/t02_gate_old`, `/tmp/t02_probe`) and link the workspace crates by path.
- **Live DB:** untouched. Read only from `cp vks_data.db /tmp/t02_final_db.db` with
  `file:…?mode=ro&immutable=1`; md5 of the original `6a23442016492077f311becf14badace` before and
  after. No server was started.
- **Concurrent worktree activity (not mine, not reviewed):** at the start of this review the tree
  was clean apart from untracked audit files; during the review another agent started editing
  `crates/vks-core/src/ports/storage.rs`, `crates/vks-storage/src/core_adapter.rs`,
  `crates/vks-admin/src/{state,rooms}.rs`, `crates/vks-recorder/*`, `crates/vks-signaling/*`,
  `crates/vks-server/src/lib.rs`, and at ~09:26 the worktree **stopped compiling**
  (`error[E0599]: no variant … StorageError::Internal`, `crates/vks-storage/src/core_adapter.rs:32`
  — an in-flight `vks-core` trait edit). Therefore **every required command was run on a pristine
  `git archive ddd033b` tree** (`/tmp/mcu_ddd033b`, target dir on the home disk). I verified
  byte-identity of all T02 files between `ddd033b` and the live worktree:
  `crates/vks-auth` (whole crate), `crates/vks-admin/src/api_v3/{auth.rs,directory/accounts.rs,tests.rs}`,
  `crates/vks-auth/src/core_adapter.rs`, `crates/vks-core/src/ports/auth.rs` — all identical. The
  harnesses that link the live-worktree `vks-auth` are therefore valid for `ddd033b`.
- **HEAD note:** `a33e195` is unrelated to T02 except one hunk in `crates/vks-server/src/lib.rs`
  that adds `global_bucket_middleware` to the m5 router (see P2-1). T02 sources/docs are unchanged
  between `ddd033b` and `a33e195`; both states are reported where they differ.

## Verdict

**No blocking P0/P1 remains.** Score: **9.0 / 10**.

Both prior open P1s are **fixed for real and independently proven**:

1. **N1 (permit released before the detached Argon2 finishes)** — `run_gated` moves an
   `OwnedSemaphorePermit` **and** the `QueueSlot` into the `spawn_blocking` closure, and all four
   async hash/verify entry points use it. Measured: after aborting in-flight hashes the fixed build
   is indistinguishable from the no-cancellation control (**+57 MiB peak in control, cancel and
   storm**), while `8dfc449` reaches **+133 MiB ≈ 7 concurrent hashes** in the same scenario.
2. **P1-2 (lazy PBKDF2→Argon2id migration dead in the shipped binary)** — `LocalAuth` now advertises
   `authenticates_locally() == true` through the core port, and `/api/v3/login` runs the migration
   on the provider branch. Reproduced against the **production wiring** (`auth_provider =
   LocalAuth::with_db(same db)`, real storage): legacy PBKDF2 row → login 200, row becomes
   Argon2id, `generation`/`password_changed_at` unchanged, `user_credentials` upserted with the PHC
   string and the same generation/mark. A remote provider (`authenticates_locally() == false`)
   leaves the row untouched and persists nothing.

Residual issues are P2/P3 only (12 open P2, 4 new P3). 0.5 point withheld for: (a) **no in-tree
regression test for the P1-class cancellation invariant** the commit fixes (`run_gated` is untested;
the existing cancellation test only covers `QueueSlot`); (b) **two commit-message claims are not
true** (register_async keeps login uniqueness; oversized old password → 422); (c) the **m5
rate-limit regression is still present in `ddd033b`** (fixed only in the later, unrelated
`a33e195`) and the tracker still advertises the old commit. The mechanical 9.5 gate (0 open P0/P1,
no new P1) *is* met.

## Item 1 — N1: cancellation-safe hash gate (**FIXED, verified**)

### Code

| Evidence (`ddd033b`) | What it shows |
|---|---|
| `crates/vks-auth/src/local_db.rs:55-58` | gate is an `Arc<Semaphore>`, so an **owned** permit can be moved into another task |
| `:84-88` | `hash_permit()` → `(QueueSlot, OwnedSemaphorePermit)` via `acquire_owned()` |
| `:94-106` | `run_gated`: `let gate = hash_permit().await?;` then `spawn_blocking(move \|\| { let _gate = gate; job() })` — **no `.await` between acquiring the permit and moving it into the closure**, so the permit cannot be dropped on cancellation while the job runs; the named `let _gate = gate` keeps it alive for the whole closure body |
| `:309, :335, :393, :410` | all four async hashing entry points (`set_password_async`, `authenticate_async`, `rehash_async_if_unchanged`, `register_async`) use `run_gated`; `grep` finds no remaining `spawn_blocking` in `vks-auth` |
| `:158-180, :284-301, :188-208` | only *synchronous* `register`/`set_password`/`authenticate` remain un-gated; their only production callers are startup (`vks-server/src/operator_policy.rs:51,63,82`) and tests — acceptable (runs before serving) |
| `:65-82` | the `QueueSlot` is also inside the closure, so the 128-slot counter tracks detached hashes too |

### Harness `/tmp/t02_gate` (release, links `vks-auth`; identical source in worktree and `ddd033b`)

Modes: `control` = 4 concurrent `authenticate_async` (no cancellation); `cancel` = 4 holders + 4
queued waiters, holders aborted after 10 ms; `storm` = 4 holders + 24 waiters, holders aborted.
Peak = `VmHWM` delta from `/proc/self/status`; ~19 MiB per in-flight Argon2 hash. 3 runs per mode,
idle host, no builds running:

```
FIXED  (ddd033b)                        OLD (8dfc449)
control Δ57 MiB  (3-4 hashes)           control Δ57 MiB
cancel  Δ57 MiB  waiters first 82-133ms cancel  Δ133 MiB (~7 hashes)  waiters first 94-99ms
storm   Δ57 MiB                         storm   Δ133 MiB (~7 hashes)
3/3 runs identical                      3/3 runs identical
```

Interpretation: in the old build, aborting the four holders released their permits immediately, the
queued waiters were admitted while the four **detached** Argon2 tasks were still allocating, and the
peak grew 2.3× over the control (133 vs 57 MiB ≈ 7 concurrent hashes). In the fixed build the
permits stay with the detached closures: waiters had to wait for those hashes to finish (first
waiter later than in the old build) and the peak equals the control in every run. The "≤ 4
concurrent Argon2 / bounded memory" invariant is now cancellation-safe.

Residuals (not P1): no in-tree test asserts this (the commit added no memory/concurrency test; see
P2-4); `run_gated` maps a `JoinError` (panic / runtime shutdown) to `LocalDbError::AuthFailed`
(`:105`), i.e. a broken hash turns into 401 "Login rejected" instead of 5xx (P3, NEW-2).

## Item 2 — P1-2: real LocalAuth lazy migration (**FIXED, verified end-to-end**)

### Wiring chain (all present in `ddd033b`)

| Link | Evidence |
|---|---|
| default port | `crates/vks-core/src/ports/auth.rs:70-75` — `fn authenticates_locally(&self) -> bool { false }` (remote providers keep the default) |
| local impls | `crates/vks-auth/src/core_adapter.rs:63-65` `LocalAuth → true`; `:109-111` `AuthProviderEnum → matches!(self, Self::Local)` (LDAP/OIDC false) |
| shipped factory | `crates/vks-server/src/main.rs:226` `create_auth_provider(&auth_cfg, Some(local_db.clone()))`; `:236` `admin_state.auth_provider = Some(auth_provider)` — always installed, and `vks-auth/src/provider.rs:132-134` builds `LocalAuth::with_db(same Arc)` for `provider_type="local"` |
| login | `crates/vks-admin/src/api_v3/auth.rs:292` carries `local: provider.authenticates_locally()`; `:313-324` runs `lazy_upgrade_if_needed` only when `local`; `:325-330` keeps the provider-less branch |
| upgrade | `:512-525` `needs_rehash` gate → `rehash_async_if_unchanged(id, Some(current_hash), password)` (CAS) → `persist_lazy_rehash`; all errors are warn-only, the login never fails |
| persist | `:530-561` refuses non-UUID ids (`:532-537`), re-checks the current row hash immediately before the upsert (`:540-550`), writes the PHC with the **current** `generation`/`password_changed_at` (`:555-556`) |

### Harness `/tmp/t02_probe` (`src/bin/login.rs`, production wiring: `AppState` + `api_v3::router` +
`tower::oneshot`, real `LocalAuth` provider over the same `LocalDb`, real `MemoryStorage`; the legacy
row has the **exact hash/salt shape of the live DB admin row**, `password_changed_at=100`,
`generation=1`):

```
kind=local        authenticates_locally=true  status=200 token=true migrated=true needs_rehash_after=false gen=1 mark=100
                  persisted=present phc=true gen=1 mark=Some(100) salt_empty=true legacy_pw_still_ok=true
kind=local_enum   authenticates_locally=true  status=200 migrated=true persisted=present gen=1 mark=100  (AuthProviderEnum path)
kind=remote       authenticates_locally=false status=200 migrated=false needs_rehash_after=true persisted=absent
```

- The shipped path now migrates; generation and change-mark are untouched (no session revocation, as
  ADR-0033 requires); the persisted row is a real `$argon2id$v=19$m=19456,t=2,p=1$…` PHC with an
  empty salt column and the unchanged generation/mark.
- LDAP/OIDC negative control: a provider with the default `authenticates_locally() == false` logs in
  successfully and **neither** rewrites the local row nor writes to storage.
- Non-UUID id (`/login nonuuid`, local row id `u-admin`): `status=200 migrated_in_memory=true
  rows_in_storage=0 bootstrap_operator_row=absent` — the old "persist under
  `UserId::bootstrap_operator()`" corruption is gone on this path.
- In-tree test `crates/vks-admin/src/api_v3/tests.rs:471-500` now exercises the real `LocalAuth`
  provider (helper `test_state_custom`, `:682-700`), 10/10 repeated runs green. Weakness: that test
  never sets `state.storage`, so `persist_lazy_rehash` is still not covered in-tree (my probe covers
  it).

Residuals: P2-3 (persist TOCTOU window), P3-3 (fallbacks elsewhere), P3-4 (provenance by provider
*type*, not store identity).

## Item 3 — `needs_rehash` m/t/p parsing (**FIXED, 20/20 cases correct**)

`crates/vks-auth/src/local_db.rs:354-373` — splits the PHC, requires `""`/`"argon2id"`, parses the
parameter field order-independently, and compares `m/t/p` with `ARGON2_*` (any missing/unparsable
parameter parses as 0 → rehash).

Harness `/tmp/t02_probe` (`src/bin/needs_rehash.rs`) — 20 constructed rows, 0 mismatches:

| Case | needs_rehash | Verdict |
|---|---|---|
| real PHC produced by `hash_password` | false | correct (round-trip) |
| `p=1,t=2,m=19456` (reordered) | false | correct |
| `m=19456,t=2,p=1,keyid=…` (extra field) | false | correct |
| `argon2i` / `argon2d` rows, same params | true | correct (policy is argon2id) |
| missing `p` / missing all params | true | correct |
| `m=65536` / `t=3` / `p=4` | true | correct (parameter upgrade triggers migration) |
| `m=abc` (unparsable) | true | correct |
| legacy PBKDF2 hex, empty, `$argon2`, no version field, uppercase algorithm | true | correct |
| params ok but salt/hash truncated or corrupt body | false | **known limitation**, moot: `verify_password` fails on such a row, so `lazy_upgrade_if_needed` is never reached (it only runs after a successful verify); such a row cannot be "repaired by login" and is not a live defect. |

## Item 4 — remaining P2s

### P2-1. m5 `/api/v3` endpoints and the "global" bucket — **OPEN in `ddd033b`, fixed at HEAD `a33e195`, untested**

- `ddd033b`: `crates/vks-server/src/lib.rs:156-192` builds `m5_api`
  (`/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status`) as a **separate** router
  merged at `:203`, with only `require_moderator` + `token_middleware` (`:191-192`). The bucket
  lives only on `vks_admin::api_v3::router` (`api_v3/mod.rs:505`), so in `ddd033b` these three
  endpoints are **not rate limited at all** (they were before `8dfc449`, via `token_middleware`).
- HEAD `a33e195` adds `.layer(from_fn_with_state(…, global_bucket_middleware))` to `m5_api`
  (`lib.rs:195` in HEAD), which closes the gap. No test asserts it: `grep TOO_MANY_REQUESTS
  crates/vks-server/tests/*.rs` → none; `network_integration.rs:2161-2196` only checks 401/403/200.
- Docs: `docs/rules/BACKEND_RULES.md:38-41` claims the bucket is the outermost layer of
  "**всего** `/api/v3`" — false in `ddd033b`, true at HEAD. `api_v3/auth.rs:466-470` repeats the
  "whole /api/v3" claim.

### P2-2. `create_user_credentials` login uniqueness (N3) — **OPEN, and the commit message is wrong**

`crates/vks-admin/src/api_v3/directory/accounts.rs:139` calls `db.register_async`, which only checks
`map.contains_key(id)` (`crates/vks-auth/src/local_db.rs:405-422`, check at `:413`); the
case-insensitive login-uniqueness check exists only in the sync `register`
(`local_db.rs:170-173`). The handler's pre-check (`accounts.rs:135-137`) is TOCTOU. Two concurrent
`POST /api/v3/users/{id}/credentials` (manage privilege) can therefore insert duplicate logins, and
`authenticate_async` picks one by `HashMap` order. The commit message claims "keeps LocalDb
login-uniqueness semantics through register_async" — it does not.

### P2-3. Lazy-rehash persist race — **PARTIAL**

In-memory CAS is correct (`local_db.rs:386-402`, compare at `:396`) and `persist_lazy_rehash`
re-checks the row hash (`api_v3/auth.rs:540-550`). But the re-check is a plain read followed by an
**awaited** upsert, and `generation`/`password_changed_at` are read *after* the check (`:555-556`).
A password change landing between the check and the upsert can still persist the stale hash together
with the new generation/mark (rollback that survives a restart). Window is now small and legacy-row
only; still the same bug class the prior review reported.

### P2-4. Test gaps — **PARTIAL**

Added: real-provider migration test (`tests.rs:471-500`), deterministic bucket test
(`:351-370`), mock-provider Busy test (`:399-413`). Still missing: a cancellation test for the
permit-in-closure invariant (N1), a real-`LocalAuth` + saturated-gate HTTP test (the Busy test uses a
mock and never reaches `core_adapter:37`), an m5-bucket test, a change-password 128-byte test, any
storage-level lazy-rehash assertion, and `test_state()` still defaults to `auth_provider = None`
(`tests.rs:666-679`) so most of the suite still exercises the branch production does not use.

### P2-5. Unknown-user dummy timing parity — **PARTIAL (self-healing)**

`local_db.rs:343-346` uses the same Argon2id work as a real verify for unknown users, but legacy
PBKDF2 rows still take longer to verify. Measured here (release, 15 samples, median):
**Argon2id verify 29.4 ms, unknown-user dummy 31.5 ms, legacy PBKDF2 verify 83.1 ms → 2.64×** for
the two live legacy accounts. Because migration now actually runs, each of those accounts drops out
of the oracle after its first successful login; the residual exposure is "before first login".

### P2-6. Credential-route status codes — **PARTIAL (commit message overclaims 422)**

| Route | Busy | PasswordTooLong | Other |
|---|---|---|---|
| `create_user_credentials` (`accounts.rs:139-148`) | 429 (`:142`) | 422 | 409 |
| `update_user_credentials` (`:263-271`) | 429 (`:265`) | 422 (`:266`) | 500 |
| `update_profile_credentials` new password (`:430-438`) | 429 (`:432`) | 422 (`:433`) | 500 |
| `update_profile_credentials` **old** password (`:409-411`) | 429 (`:411`) | **404 "Incorrect old password"** (oversized old → `AuthFailed`) | — |
| `PATCH /api/v3/password` old (`api_v3/auth.rs:365-367`) | n/a | **401 "Login rejected"** | — |
| `PATCH /api/v3/password` new (`:380-382`) | 429 (`:370`,`:393`) | 422 | 500 |

The commit message says "oversized old password -> 422"; the two old-password sites still return 404
and 401. `BACKEND_RULES`/task tracker document no such choice.

### P2-7. `hash_password` `expect()` / unguarded `rehash_async` — **OPEN**

`local_db.rs:463-467` still `.expect("Argon2id hashing cannot fail")` (a panic inside
`spawn_blocking` becomes `JoinError` → `AuthFailed` → HTTP 401, masking the bug). `rehash_async`
(`:379-381`) is still `pub` with no empty/`MAX_PASSWORD_BYTES` guard (no production caller besides
`rehash_async_if_unchanged`).

### P2-8. Dead `ldap_sync_enabled` flag — **OPEN**

Never set to true (`crates/vks-admin/src/state.rs:216,322`); `handlers.rs` therefore always returns
501 and the `MockLdapProvider::fetch()` branch stays unreachable dead code.

### P2-9. Legacy >128-byte passwords lock out — **OPEN**

`login` → 422 (`api_v3/auth.rs:267-271`) and `change_password` oversized old → 401 (`:365-367`);
there is still no rotation path and no documentation of the policy.

### P2-10. Docs/tracker still not to fact — **PARTIAL**

Now true: lazy migration (§1.3/ADR-0033/module docs), no fake complexity validation, Argon2 params.
Still wrong or stale:

- `docs/progress/MASTER_TASK_LIST_2026-10-29.md:12` — T02 row still says
  "✅ fix `8dfc449` … P0/P1 закрыты; re-review идёт", i.e. it still advertises the **previous**
  commit and asserts a closure that was false at `8dfc449`; `ddd033b` did not update it.
- `docs/rules/BACKEND_RULES.md:32` — the line `ddd033b` added ends with a **stray backtick**:
  `… ограничение только 128 байт.`` (markdown defect introduced by this commit).
- `docs/rules/BACKEND_RULES.md:28-30` — "очередь ожидания ≤ 128 запросов": the counter caps 128
  *total in-flight* (4 running + ≤124 waiting). ADR-0033 was corrected to "≤ 128 всего"; this file
  was not.
- `docs/rules/BACKEND_RULES.md:38-41` — "**всего** `/api/v3`" is false in `ddd033b` (m5).
- `crates/vks-admin/src/api_v3/auth.rs:463` — `ddd033b` touched this doc line but it is still
  attached to `global_bucket_middleware` (which returns **429** "Too many requests", not
  "401 Too many attempts…").

### P2-11. Single global key charged pre-auth — **OPEN (accepted design risk)**

`api_v3/auth.rs:471-481`, `RateLimiter::new(200, 50.0)`, key `"global"`, no per-IP/user dimension;
documented as deliberate (`BACKEND_RULES:38-40`). A single client can make the whole API answer 429,
including legitimate logins. Deserves an explicit decision; unchanged.

### P2-12. No test for the m5 bucket fix (HEAD) — **OPEN**

See P2-1.

## Item 5 — NEW defects introduced by `ddd033b`

### NEW-1 (P3) — `change_password` returns 500 *after* applying the password change in memory

`crates/vks-admin/src/api_v3/auth.rs:391-409`: `set_password_async` succeeds (in-memory hash and
generation updated, `state.users` generation synced at `:400-404`), then a non-UUID local id hits
`:406-409` and returns **500 "Password changed in memory only"**. The response contradicts the
applied state, and the early return skips `state.notify_revocation` (`:436`) and the
`password.changed` audit entry, so other nodes keep accepting pre-change tokens. Unreachable in
production (all production ids are UUIDs: `BOOTSTRAP_ADMIN_USER_ID`, `UserId::new()`), and strictly
better than the previous silent write under `bootstrap_operator` — but the fail-loud path should
roll back or still notify/audit.

### NEW-2 (P3) — `run_gated` maps `JoinError` to `AuthFailed`

`local_db.rs:100-105`: a panic in the blocking job (including `hash_password`'s `expect`) or runtime
shutdown becomes `LocalDbError::AuthFailed` → 401/404/409 depending on the caller. A broken hash
should be 5xx, not "wrong password". Latent.

### NEW-3 (P3) — "no bootstrap fallback" claim is only true for the two `auth.rs` sites

The commit says non-UUID ids are refused instead of written under `bootstrap_operator`. Two sites
still do exactly that: `crates/vks-admin/src/users.rs:135` (`bootstrap_operator_async`) and
`crates/vks-server/src/operator_policy.rs:78` (rotation), both
`UserId::parse(&…).unwrap_or_else(|_| UserId::bootstrap_operator())`. Latent for UUID ids, but the
claim is broader than the code.

### NEW-4 (P3) — provenance is provider *type*, not store identity

`authenticates_locally()` guarantees "this provider verifies against *a* LocalDb", not "against
`state.local_db`". A `LocalAuth` built over a different `LocalDb` (factory misconfiguration, tests,
future embeddings) would let a login verified in store A trigger a CAS rehash of a same-named row in
store B with the submitted password. The shipped factory always passes the same `Arc`
(`main.rs:226`), so this is a latent trust-boundary coupling, not a live defect; the robust form is
to carry the verified user id/hash from `verify` (or a `db_user_by_name` equality check on ids).

### NEW-5 (P3, docs) — stray backtick in `BACKEND_RULES.md:32`

Introduced by this commit's documentation hunk; cosmetic but in the exact paragraph edited to be
"to fact".

No new P0/P1 was found. `run_gated` error mapping, the non-UUID 500, and the residual fallbacks are
P3; all are unreachable in the shipped configuration or fail closed.

## Per-finding verdicts (normalized across the three prior reviews)

Legend: **Fixed** = defect gone, fix verified here; **Partial** = headline mitigated, residual of the
same class remains; **Open** = not addressed.

| # | Finding | Verdict | Evidence (`ddd033b`) |
|---|---|---|---|
| P0-1 | Waiter-counter leak on future cancellation | **FIXED** | `local_db.rs:53,65-82` (`HASH_WAITERS` + `QueueSlot`/`Drop`); regression test `:675-703`; prior measurement 0/20 `Busy` after 140 aborts; unchanged and still correct under the new `run_gated` (the slot now lives inside the closure). |
| P1-1 | Typed `Busy` → 429 through the production provider path, no lockout/audit; change-password & credential routes | **FIXED** | `vks-core/src/ports/auth.rs:26-28`; `core_adapter.rs:37`; `api_v3/auth.rs:294,333-338`; `:370,:393`; `accounts.rs:142,265,411,432`; test `tests.rs:399-413`; independently reproduced with **real `LocalAuth` + saturated gate**: 5×429, `Retry-After: 1`, `blocked_for=None`, `audit_entries=0`. |
| P1-2 | Lazy rehash only for LocalDb-verified logins, CAS, no bootstrap fallback | **FIXED** (provenance residual P3-4) | `core_adapter.rs:63-65,109-111`; `vks-core/src/ports/auth.rs:70-75`; `api_v3/auth.rs:292,313-324,512-561`; `local_db.rs:386-402`; probe: local/local_enum migrate + persist (gen 1/mark 100), remote does not; non-UUID persist refused (`rows_in_storage=0`). |
| P1-3 | Gate bypass via sync `register` on a request path | **FIXED** (nit) | `accounts.rs:139` `register_async`; sync `register` still `pub` (only `#[doc(hidden)]`, `local_db.rs:157`) but its only callers are startup/tests. |
| P1-4 | "Global" bucket scope + docs | **PARTIAL** | `api_v3/mod.rs:505` covers the whole `api_v3::router` incl. login/ws/static/catch-all; **m5 endpoints still outside** in `ddd033b` (`vks-server/src/lib.rs:156-192`); fixed at HEAD `a33e195` (`lib.rs:195`), untested; docs claim "всего /api/v3". |
| N1 | Permit released before detached hash completes | **FIXED** | `local_db.rs:94-106` (+`:309,335,393,410`); harness: cancel/storm peak 57 vs 133 MiB in `8dfc449`; no in-tree test (P2-4). |
| P2-1 | Flaky 429 test | **FIXED** | `tests.rs:351-370` with `install_test_bucket` (`mod.rs:126-128`, `RateLimiter::new(5,0.0)`); 10/10 repeated runs green (no flake). |
| P2-2 | 128-byte status codes on credential routes | **PARTIAL** | Fixed on 4 sites; old-password oversize still 404 (`accounts.rs:409-411`) and 401 (`api_v3/auth.rs:365-367`); commit message overclaims 422. |
| P2-3 | Lazy-rehash TOCTOU / stale persistence | **PARTIAL** | CAS `local_db.rs:396`; persist re-check `auth.rs:540-550`; check→awaited-upsert window and post-check mark/generation read (`:555-556`) remain. |
| P2-4 | Missing tests | **PARTIAL** | Added: real-provider migration, deterministic bucket, mock Busy. Still missing: N1 cancellation/concurrency test, real-`LocalAuth` Busy HTTP test, m5 bucket test, storage-level lazy-rehash test, change-password limit test. |
| P2-5 | Unknown-user timing parity | **PARTIAL** | Argon2 dummy `local_db.rs:343-346`; measured 31.5 ms dummy vs 83.1 ms legacy PBKDF2 (2.64×); self-heals after first login because migration now works. |
| P2-6 | Docs/code mismatches | **PARTIAL** | Lazy migration, params, complexity claim now true; tracker stale, stray backtick, queue wording, "всего /api/v3", stray 401 doc on the bucket middleware (P2-10). |
| P2-7 | `needs_rehash` prefix-only | **FIXED** | `local_db.rs:354-373`; 20-case probe 0 mismatches (reorder, argon2i/d, missing, bumped, malformed). |
| P2-8 | `hash_password` `expect()` / unguarded `rehash_async` | **OPEN** | `local_db.rs:465`, `:379-381`. |
| P2-9 | Dead `ldap_sync_enabled` | **OPEN** | `state.rs:216,322`. |
| P2-10 | Legacy >128-byte password lockout | **OPEN** | `api_v3/auth.rs:267-271`, `:365-367`. |
| P2-11 | Commit hygiene | N/A | `ddd033b` is focused (9 files, auth+docs+tests); message overclaims two items (see P2-2/P2-6). |
| N2 | m5 endpoints lost the limiter | **OPEN in `ddd033b`**, fixed at HEAD `a33e195` | `vks-server/src/lib.rs:156-192` / HEAD `:195`; no test. |
| N3 | `register_async` lost login uniqueness | **OPEN** | `accounts.rs:139`; `local_db.rs:405-422` (check at `:413` is id-only). |
| N4 | Single global key, charged pre-auth | **OPEN** (documented) | `api_v3/auth.rs:471-481`. |

## Explicit counts

- **Open P0: 0.**
- **Open P1: 0.** (P0-1, P1-1, P1-2, P1-3 and N1 are fixed and independently verified; P1-4 is
  reduced to a P2 residual — m5 endpoints — which is fixed at HEAD.)
- **New P1 introduced by `ddd033b`: 0.**
- **Open P2: 12** — m5 limiter (P2-1/N2), `register_async` uniqueness (P2-2/N3), persist TOCTOU
  (P2-3), test gaps incl. N1 (P2-4), legacy timing oracle (P2-5), old-password status codes (P2-6),
  `expect()`/unguarded `rehash_async` (P2-7), dead `ldap_sync_enabled` (P2-8), >128-byte lockout
  (P2-9), tracker/docs (P2-10), global-key availability trade-off (P2-11), m5-bucket test (P2-12).
- **New P3 from `ddd033b`: 4** — change_password 500 after in-memory change, `JoinError`→401,
  residual `bootstrap_operator` fallbacks, provenance by provider type (+ the docs backtick folded
  into P2-10).

## Score: 9.0 / 10

- **+** The two open P1s are fixed with correct, minimal changes and are **independently proven**:
  memory/concurrency harness (57 vs 133 MiB, cancel == control) and a full production-wiring login
  probe (migration + persistence + generation/mark, LDAP negative control, no operator-id write).
- **+** `needs_rehash` is now parameter-correct (20/20 edge cases), the m/t/p upgrade path is real,
  credential routes got 429/422 on the main paths, the tracker/docs got more honest, and the gate
  fix covers every async hash path.
- **+** 39 + 338 + 175 tests green, clippy `-D warnings` clean, rustdoc `-D warnings` clean, fmt
  clean; new tests are not flaky (10/10, 10/10, 5/5, 5/5).
- **−** The P1-class cancellation invariant has **no in-tree regression test** (the exact class of
  bug that regressed twice already); the mock-provider Busy test never reaches `core_adapter`.
- **−** Two commit-message claims are false (register_async uniqueness; oversized old password →
  422) and the tracker still advertises `8dfc449` with "P0/P1 закрыты" — the same
  claim-vs-code failure mode flagged in every prior round.
- **−** The m5 limiter regression is still present in `ddd033b` (fixed only in the later,
  unrelated `a33e195`, without a test), and `persist_lazy_rehash` still has a check→upsert window
  that can roll a concurrent password change back in storage.

The mechanical 9.5 gate (zero open P0/P1, no new P1) is met; the withheld 0.5 is for the three
quality items above, not for any remaining security- or availability-critical defect.

## Commands run and results

All from a pristine `git archive ddd033b` tree at `/tmp/mcu_ddd033b` (the shared worktree did not
compile during the review because of another agent's in-flight `vks-core`/`vks-storage` edits),
`CARGO_TARGET_DIR` on the home disk, `export PATH="$HOME/.cargo/bin:$PATH"`, `--locked --offline`:

| Command | Result |
|---|---|
| `cargo test -p vks-auth --locked` | **39 passed, 0 failed** (4.70 s) + 0 doc-tests |
| `cargo test -p vks-admin --lib --locked` | **338 passed, 0 failed** (33.61 s) |
| `cargo test -p vks-server --tests --locked` | **175 passed, 0 failed, 3 ignored** — lib 144, `config_failclosed` 1, `h265_rtp` 1, `network_integration` 22, `operator_policy` 7 |
| `cargo clippy -p vks-auth -p vks-admin -p vks-server --all-targets --locked -- -D warnings` | **exit 0, 0 warnings** |
| `RUSTDOCFLAGS="-D warnings" cargo doc -p vks-auth -p vks-admin --no-deps --locked` | **clean** |
| `cargo fmt --check -p vks-auth -p vks-admin` | **exit 0** |
| `for i in {1..10} … api_v3::tests::global_api_bucket_limits_bursts_with_429 -- --exact` | **10/10 pass** (no wall-clock flake) |
| `for i in {1..10} … api_v3::tests::local_provider_login_lazily_migrates_legacy_pbkdf2_row -- --exact` | **10/10 pass** |
| `for i in {1..5} … provider_busy_returns_429_with_retry_after_and_does_not_lock_out` / `cancelled_waiters_release_their_queue_slots` | **5/5 and 5/5 pass** |
| `git diff ddd033b ../<worktree> -- <T02 files>` / `diff -r /tmp/mcu_ddd033b/crates/vks-auth crates/vks-auth` | all T02 files **identical**; only `vks-server/src/lib.rs` differs between `ddd033b` and HEAD (T11a m5 layer) |
| `/tmp/t02_gate` (release, 3 runs/mode): `control`, `cancel`, `storm` on `ddd033b` vs `/tmp/t02_gate_old` on `8dfc449` (old tree from `git archive 8dfc449`) | fixed: **Δ57 MiB in all three modes**, waiters delayed behind detached hashes; old: **Δ57 control / Δ133 cancel / Δ133 storm** (≈7 concurrent hashes). Reproduced 3/3 each. |
| `/tmp/t02_probe` `login` (production wiring: real `LocalAuth` + `MemoryStorage`, live-DB-shaped PBKDF2 row) | `local`: 200, migrated, gen 1, mark 100, **persisted PHC gen 1 mark 100**; `local_enum`: same; `remote` (`authenticates_locally=false`): 200, **not migrated, nothing persisted** |
| `/tmp/t02_probe` `login nonuuid` | 200, migrated in memory, **0 storage rows**, bootstrap-operator row **absent** |
| `/tmp/t02_probe` `login busy` (128 in-flight hashes, real `LocalAuth`) | 5×**429**, `Retry-After: 1`, `blocked_for=None`, **audit entries 0** |
| `/tmp/t02_probe` `needs_rehash` | **20/20 cases correct**, 0 mismatches |
| `/tmp/t02_probe` `timing` (release, 15 samples, median) | Argon2id verify **29.4 ms**, unknown-user dummy **31.5 ms**, legacy PBKDF2 **83.1 ms** (ratio **2.64×**) |
| Live DB read-only (`cp` → `/tmp/t02_final_db.db`, `mode=ro&immutable=1`) | **2 `user_credentials` rows, both 64-hex PBKDF2 with a 32-hex salt, generation 1**: `00000000-…-0001` (admin) and `c92dced8-…` (viewer_probe). Original md5 `6a2344…` unchanged. The live shape is exactly what the fixed migration path upgrades. |

### Limitations

- No live server was started; HTTP behaviour was exercised in-process (`tower::oneshot`) as the
  repo's own tests do.
- The m5 limiter is evaluated statically for `ddd033b` and HEAD; I did not re-run the T11a commit's
  test suite.
- My N1 exploitability statement is limited to the invariant breach and the measured 2.3× peak:
  I did not attempt a timed end-to-end socket-level attack against a running server.
- The concurrency harness links the worktree `vks-auth`, which I verified byte-identical to
  `ddd033b`; the old-build comparison uses `git archive 8dfc449` in `/tmp`.

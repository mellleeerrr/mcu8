# Cleanup Audit — 2026-10-23

## Scope
Read-only/low-risk audit of temporary artifacts, conflicting outputs, and dependency/config cleanliness for the legacy PNM and MCU workspace.

## Findings
1. `.work-tmp/` is gitignored (~46 MB) and contains agents, E2E probes, gates/logs. It is volatile and may be removed only when live task phases no longer need the evidence.
2. Live PNM runtime `client/pnm-frontend/deployed/pnm` must remain symlink-free; current `find` audit did not find newly imported symlinks.
3. `codian-layouts.js` carried an uncommitted Round6 change (constructor section move + preset guard); its hash was out of sync in `MANIFEST.sha256`. Resolved to the live file hash `913a1f…`.
4. `index.html` and `theme-toggle.js` hashes now match committed content; `sha256sum -c` is clean.
5. Temporary files (`code.txt`, `ex.txt`) are no longer present at repository root; other generated artifacts remain under ignored directories.
6. Build-generated `target/` and SQLite files are ignored and must not be committed.

## Recommendations
- Do not delete `docs/`, `tests/`, `deploy/`, `scripts/`, `.github/`, `client/pnm-frontend/deployed/pnm`, or active `.work-tmp` evidence while Round6/7 tasks are in progress.
- Remove volatile artifacts only after confirming they are not referenced by evidence indexes.
- Keep manifest update mandatory for every runtime edit.
- Keep changes to `codian-layouts.js` in its own commit/task rather than bundling with theme tasks.

## Commands used
- `git status --short`
- `git diff -- client/pnm-frontend/deployed/pnm/codian-layouts.js`
- `find ... -name '*.bak*' ...`
- `sha256sum -c ...`

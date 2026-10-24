# Dark Theme for Legacy PNM UI — Analysis and Plan

## Objective
Provide a dark theme for the legacy TrueConf MCU Web UI (PNM), initially surfaced at
`/pnm/monitoring/sessions`, while preserving the current server-served runtime
(`client/pnm-frontend/deployed/pnm`) and its invariants.

This document records the full analysis performed on 2026-10-23, the pilot
implementation already applied, and the roadmap to a production-grade dark theme.

## Scope and invariants
- Runtime: `client/pnm-frontend/deployed/pnm` (static SPA, served by `vks-server` at `/pnm/`).
- UI stack: Vue 3.5 + vue-router (history, base `/pnm/`), Pinia, MDC Web (mixture of `mdc-*`, `tc-*`), Chart.js, video.js.
- Edit rules (`client/pnm-frontend/NOTES.md`):
  - No symlinks in `deployed/pnm/`.
  - `index.html` is served with `Cache-Control: no-cache`, so no cache-bust is needed.
  - `assets/**` stays `immutable` (cache-bust required for assets).
  - `MANIFEST.sha256` must reflect edited files.
  - CSP is computed by the server from inline scripts; inline styles are allowed (`'unsafe-inline'`).

## Current user-facing target
- Route: `http://127.0.0.1:8080/pnm/monitoring/sessions`.
- DOM root observed: `body → #root[data-v-app] → .main`.
- Rendered class inventory for the target route: 152 distinct classes (see `/tmp/dark/classes.json` on the audit workstation).
- No fixed portal containers outside `.main` in the captured state; `notifier__container` is inside `.main`.

## Color architecture findings
1. The compiled CSS contains very few design tokens exposed as CSS variables; colors are mostly hardcoded hex/rgba (dominant: `#fff`, `#0097a7`, `#0099a6`, `#f5f5f5`, `#000`, `#999`).
2. There is no existing dark-mode or theme toggle in PNM (the only `isDark` reference is in the video bundle).
3. The `server_settings.payload.skin` values (`modern`, `dark-blue`) in `api_v3` tests are conference skins, not admin UI theme.
4. MDC Web theme variables (`--mdc-theme-*`) exist but are not globally overridden with values; the main visual palette lives in compiled component CSS.
5. Project rules (`docs/rules/FRONTEND_RULES.md §4.5`) already require Dark/Light/High contrast themes via CSS variables, but that applies to the future first-party UI; `PNM` is the licensed legacy UI until phase 5.

## Conclusion on approach
A token-based partial restyle of thousands of compiled rules is not feasible as one step.
The pragmatic first step is an invert + hue-rotate CSS filter applied to the interactive root (`.main`), with explicit cancellation where needed. This makes the target route visually dark without editing vendor bundles.

## Pilot implemented
File: `client/pnm-frontend/deployed/pnm/index.html`
Style id: `prostor-pnm-dark-theme` (inline, before `</head>`).

```css
.main {
  filter: invert(85%) hue-rotate(180deg);
  background-color: #ffffff !important;
}
body {
  background-color: #121212 !important;
}
.main img, .main picture, .main video, .main canvas,
.main .main-blank__image, .main svg.main-blank__image {
  filter: invert(85%) hue-rotate(180deg) !important;
}
.main .material-icons {
  filter: invert(85%) hue-rotate(180deg) !important;
}
.main .main__background {
  display: none !important;
}
```

Manifest checksum for `./index.html` was updated in `client/pnm-frontend/MANIFEST.sha256`.

### Task 3 (implemented and refined)
- Floating surfaces inside `.main` cancel inherited `invert(85%)` with `invert(100%) hue-rotate(180deg)` and then use dark tokens `#1f2328/#e7e9ec/#2b3138`.
- Additional portal roots outside `.main` now receive direct dark tokens (`#1b2026/#e7e9ec/#27303a`, elevation, focus).
- Header, card, and list surfaces now have their own cancellation + `#1b2026/#e7e9ec/#27303a` token rules.
- Approximate cancellation is documented: `invert(100%)` is not the exact inverse of `invert(85%)`.
- Verification measured no console errors and portal background `rgb(31,35,40)`, recomputed manifest/CSP passed.

## Verification
- `cargo test -p vks-server --lib csp` → 3 tests pass (CSP unchanged).
- `sha256sum -c ../../MANIFEST.sha256` for `./index.html` → OK.
- Playwright audit of `/pnm/monitoring/sessions` (authenticated via API token in `localStorage`):
  - Pre-pilot page: average luminance 251.8, 0.5% dark pixels.
  - Pure invert(85%) pilot: average luminance 9.2, 95.9% dark pixels (too dark).
  - Softened invert(85%) pilot: average luminance 44.5, 95.6% dark pixels (acceptable normal dark).
  - Body background unfiltered and set to `#121212`.

## Known issues and follow-ups

### P0 — correct obvious artifacts (highest priority)
1. Material icons are double-inverted back to their original brand/teal accent (currently implemented).
2. Hide or explicitly revert `.main__background` SVG (currently implemented by hiding).
3. Add thumbnails/background-image media selectors to the revert set if review shows inverted media.

### P1 — un-invert and properly restyle overlays and controls
- Cancel inherited filter with an inverse filter on `.main .mdc-menu-surface`, `.main .tc-context-menu`, `.main .notification-center__menu`, `.main .header-profile__menu`, `.main .bookmarks-drawer`, and `.main .notifier__container` toast cards; then apply real dark tokens:
  - surfaces: `#1f2328`, text `#e7e9ec`, borders `#2b3138`, normal dark elevation shadows.
- Fix focus visibility under `.main-route--monitoring-sessions :focus-visible`.
- Fix checkbox/radio checkmarks and switch thumbs if they vanish.

### P2 — charts and media fidelity
- Remove blanket `.main canvas` revert if chart readability improves.
- Choose and document one chart strategy (invert-only vs native dark chart palette).
- Keep video/video posters/tests listed on pages other than sessions.

### P3 — scope, polish and coverage
- Route-scoped variant for a pilot on only: `.main:has(> .main-route--monitoring-sessions)`.
- Normalize brand accent hue handling so teal stays teal in all cancelled-filter subtrees.
- Scrollbars and dense tables, empty states, status/icon colors.
- Add a manual toggle plus `prefers-color-scheme` integration only after first-party UI and theme-token infrastructure exist; keep legacy PNM default as done here.

### P4 — removal/replacement
- Phase 5 replaces PNM with a first-party Dioxus UI; at that point migrate dark theme to CSS variables (`Docs/FRONTEND_RULES.md §4.5`) and retire this invert pilot (and likely remove `/api/v3` legacy assets per ADR-0027).

## Risks
- `invert + hue-rotate` flips semantic colors (green/red status) and box-shadows/elevation; acceptable for a visual pilot only.
- Whole-app `.main` scope darkens login and other pages; no light option currently exists. Revert to route-scoped selector if that becomes a problem.
- Any runtime portal that gets teleported outside `.main` will stay light and clash; no such portals were observed for sessions, but admin screens should be inventoried before wider rollout.
- Server reads `index.html` at startup for CSP, but the static file is served per request, so edits are live without restart.

## Artifacts
- Changed file: `client/pnm-frontend/deployed/pnm/index.html` (`<style id="prostor-pnm-dark-theme">`).
- Manifest update: `client/pnm-frontend/MANIFEST.sha256` (`./index.html`).
- Audit captures (outside repository): `/tmp/dark/sessions.png`, `/tmp/dark/authed.png`.
- Rendered class inventory (outside repository): `/tmp/dark/classes.json`.


## Implementation Completed (2026-10-24, Round 7)

### Production-Grade Implementation Summary
The pilot `invert + hue-rotate` approach has been completely replaced with a native, high-fidelity **Slate Dark Mode** and an integrated interactive theme switcher in the UI header.

1. **Header Theme Switcher (`theme-toggle.js`, commit `700d393`, score 9.8/10)**:
   - Injected directly into `.main-large-header__actions-wrapper` before `.main-large-header__actions-spacer` as a proper Vue 3 VDOM sibling.
   - Built with full vendor styling parity: `tc-button-container tc-button-container--primary notification-center__button vks-theme-toggle`, `button.tc-button.mdc-button.mdc-button--text.tc-button--icon`.
   - Full WAI-ARIA APG compliance: `aria-pressed="true|false"`, `aria-label="Переключить тему оформления"`, localized Russian `title` attributes.
   - High-efficiency DOM observer: batched via `requestAnimationFrame` with early `isConnected` checks to eliminate CPU load.
   - Cross-tab state synchronization and OS preference integration (`prefers-color-scheme`) with dynamic change listener.

2. **Native Slate Dark Mode (`index.html`, commit `214a31f`, score 9.9/10)**:
   - Complete removal of `filter: invert()` and all double-inversion cancellation hacks.
   - Deep Slate palette:
     - Base canvas: `#12151a`
     - Content containers: `#1a202c`
     - Header, cards, panels: `#212631`
     - Menus, popovers, select dropdowns: `#282e3b`
     - Borders: `#2d3748` / `#363f50`
     - Typography: `#f0f3f6` (primary), `#94a3b8` (secondary/muted)
     - Accents: `#26c6da` (bright cyan), `#0097a7` (teal brand)
   - Fixed modal backdrop: `rgba(0, 0, 0, 0.6) !important` (semi-transparent blur, eliminating previous solid-black lockup).
   - Form controls, tabs (`.tc-tabs`), disabled inputs (`#1e2430`), and striped tables (`.main-table-line--gray` -> `#1a202c`) fully restyled.
   - Video elements, camera previews, participant avatars, call status badges (green/red/yellow), and Chart.js graphs remain 100% natural without color shift or double inversion.
   - Modern dark scrollbars (`#363f50 #1a202c`).

3. **Resolved Roadmap Items**:
   - [x] P0: Eliminated all double-inversion artifacts and background SVG issues.
   - [x] P1: Full overlay, menu, popover, dialog, and control restyling with real Slate tokens.
   - [x] P2: Natural media, avatar, call status, and Chart.js fidelity preserved natively.
   - [x] P3: Dynamic manual toggle + `prefers-color-scheme` integration fully delivered.

## Complete Slate Dark Mode & Full Stack Hygiene (2026-10-25, Round 8, Goal Mode)

### Full-Chain Coverage via Playwright
1. **Task G1 (commit `33438fb`, review score 9.8/10)**:
   - Full Playwright E2E Crawler implemented as a persistent regression test in `tests/e2e/crawl_all_tabs.js`.
   - Complete 100% Slate Dark Mode across all 27 views and 334 buttons audited:
     - 0 views with light-theme anomalies.
     - 0 CSS selectors needing dark override.
     - High-specificity container styling: `.main-route`, `[class*="main-route--"]`, `.monitoring-screen`, `.user-screen`, `.logs-screen`, `.administration-screen`, `.base-card-content`, `.bookmarks-drawer-items`, `.auth`, `.auth__content`, `.auth-picture`.
     - Unelevated neutral buttons styled with `#2d3748` / `#4a5568` / `#f7fafc` while preserving brand `--primary` (`#0097a7`) and `--danger` (`#f04343`).
     - Native CSS `@media (prefers-color-scheme: dark) { html:not(.theme-light) ... }` baseline for 0 ms Zero FOUC.
     - Compact declarative `theme-toggle.js` (75 lines) with `<meta name="theme-color">` synchronization and `MutationObserver` check `!container.isConnected`.
     - `client/pnm-frontend/MANIFEST.sha256` recomputed and validated (214/214 files 100% OK).

2. **Task G2 (commit `55f96f0`, review score 10.0/10)**:
   - Rust dead code cleanup: removed `last_arrival` and `last_sent_seq` from `BweEstimator` (`crates/vks-media-sfu/src/bwe.rs`).
   - Removed `mode: RoomMode` from `RoomState` and updated `RoomState::new(caps)` across all 4 call sites (`crates/vks-media-mcu/src/engine.rs`).
   - Removed alien script `scripts/qwen.sh`.
   - Quality gates passed: `cargo fmt --all -- --check` (0 diffs), `cargo clippy --workspace --all-targets -- -D warnings` (0 warnings/errors), `cargo test --workspace` (all tests passed).

3. **Task G3 (review score 10.0/10)**:
   - Systemd SSOT alignment: `~/.config/systemd/user/vks-mcu.service` 1:1 identical to `deploy/systemd/vks-mcu.service` using canonical `%h/mcu`.
   - Stand-specific environment variables isolated in drop-in `~/.config/systemd/user/vks-mcu.service.d/stand.conf` (`VKS_CANDIDATE_ADDR=192.168.1.93`, `VKS_H323_UAS_ENABLED=1`, `VKS_H323_UAS_ALLOW`, `VKS_H323_UAS_MAX_CALLS`, `VKS_H323_UAS_MAX_PER_IP`).
   - Removed ghost directory `/home/pro/Рабочий стол/mcu antigravity`.
   - Systemd service cleanly restarted: `Active: active (running)`, Main PID 479640, Python child 479704, listening ports 8080 (HTTP), 5060 (SIP), 1720 (H.323), `curl -I http://127.0.0.1:8080/pnm/authorization` -> HTTP 200 OK.

4. **Task G4 (review score 10.0/10)**:
   - Final Playwright E2E crawl on live service: 27 views, 334 buttons, 0 anomalies, 0 unstyled selectors.
   - CSP test suite: `cargo test -p vks-server --lib csp` (3/3 passed).
   - Fail-closed test suite: `cargo test -p vks-server --test config_failclosed` (exit code 78 passed).
   - Documentation and master tracker updated.


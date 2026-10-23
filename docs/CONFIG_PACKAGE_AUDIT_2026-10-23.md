# Config & Package Inventory Audit (read-only) — 2026-10-23

## Workspace
- Rust workspace with 16 members (`Cargo.toml`): 15 server/krate modules + `client/vks-client-sdk`.
- Version `0.1.0`, edition 2021, rust-version 1.94.
- No deprecated kreaty discovered in the manifest; previous cleanup commits removed utoipa, parking_lot, rvoip.

## Key dependencies (keep)
- tokio 1 full, axum 0.8 ws/multipart, tower-http 0.6 (fs only), figment 0.10, prost 0.13, serde 1, uuid 1, ulid 1, sha2/base64, proptest 1.
- Runtime/rust flags rely on `unwind`; remove only with ADR.

## Static/runtime alignment
- `configs/default.toml`: `listen_addr = 0.0.0.0:8080`, `frontend_dir = "client/pnm-frontend/deployed/pnm"`.
- Actual server running on :8080 and PNM asset paths use `/pnm` and `/mcu`; paths in UI are absolute as expected.
- `candidate_addr` intentionally empty for dev and set via env in production.

## Recommendations (audit only)
- Keep workspace structure and versions; no version upgrade in this task.
- Any config removal should be gated by targeted tests; default config contains intentional insecure-dev values.
- `.work-tmp`, `target`, DB files stay ignored.
- `deny.toml` should be checked by its own tooling (cargo-deny) before removing dependencies.

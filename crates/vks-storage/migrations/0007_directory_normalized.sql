-- T10b: directory / address book moved out of the PNM configuration document.
--
-- Portable SQLite + PostgreSQL:
--   * TEXT ids only (0001 `users` uses UUID/BLOB; new rows are strings);
--   * BIGINT time in unix seconds (NOT milliseconds) bound by the application;
--   * no JSON1/`::jsonb` expressions: legacy JSON is parsed in Rust and written
--     through bound parameters (broken/future records survive by design);
--   * no FOREIGN KEY in v1: built-in role ids are not rows, `core_users`/
--     `users` layouts differ between backends, and SQLite FKs are OFF by
--     default while PostgreSQL enforces them (T10 P0-6/P1-2);
--   * `schema_state` is the idempotent migration marker.

-- Idempotent data-migration marker. `state_key = 'directory'`, `version = 1`.
CREATE TABLE IF NOT EXISTS schema_state (
    state_key  TEXT PRIMARY KEY,
    version    BIGINT NOT NULL,
    updated_at BIGINT NOT NULL
);

-- Profile per directory user (full DirectoryUser JSON, forward-compatible).
CREATE TABLE IF NOT EXISTS directory_profiles (
    user_id      TEXT PRIMARY KEY,
    profile_json TEXT NOT NULL,
    updated_at   BIGINT NOT NULL
);

-- Custom/override groups (built-in groups do not exist; `groups` are all rows).
CREATE TABLE IF NOT EXISTS directory_groups (
    id         TEXT PRIMARY KEY,
    group_json TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);

-- Custom/override roles only. Built-in Administrator/Operator ids stay code
-- constants and are NOT seeded here.
CREATE TABLE IF NOT EXISTS directory_roles (
    id        TEXT PRIMARY KEY,
    role_json TEXT NOT NULL,
    updated_at BIGINT NOT NULL
);

-- Derived membership from DirectoryUser.groups (rewritten from one snapshot).
CREATE TABLE IF NOT EXISTS directory_group_members (
    group_id TEXT NOT NULL,
    user_id  TEXT NOT NULL,
    PRIMARY KEY (group_id, user_id)
);

-- Derived assignments from DirectoryUser.roles. role_id may be a built-in id
-- (Administrator/Operator) -> intentionally no FK.
CREATE TABLE IF NOT EXISTS directory_user_roles (
    user_id TEXT NOT NULL,
    role_id TEXT NOT NULL,
    PRIMARY KEY (user_id, role_id)
);

-- Global book (`owner_id = ''`) and personal copies (`owner_id = user_id`).
-- PK (owner_id, id) keeps a personal copy independent from the global row:
-- attaching a global address copies it under the same id without stealing it,
-- and deleting the global row keeps personal copies (current API semantics).
CREATE TABLE IF NOT EXISTS directory_addresses (
    owner_id      TEXT NOT NULL DEFAULT '',
    id            TEXT NOT NULL,
    uri           TEXT NOT NULL,
    uri_canonical TEXT NOT NULL,
    display_name  TEXT NOT NULL,
    payload_json  TEXT NOT NULL,
    address_json  TEXT NOT NULL,
    created_at    BIGINT NOT NULL,
    PRIMARY KEY (owner_id, id)
);

CREATE INDEX IF NOT EXISTS idx_dir_group_members_user ON directory_group_members(user_id);
CREATE INDEX IF NOT EXISTS idx_dir_user_roles_user    ON directory_user_roles(user_id);
CREATE INDEX IF NOT EXISTS idx_dir_addresses_owner    ON directory_addresses(owner_id);

-- One global record per canonical URI. NOT a conflict target: application code
-- upserts by the full (owner_id, id) PK (T10 P2-1).
CREATE UNIQUE INDEX IF NOT EXISTS ux_dir_addresses_global_canonical
    ON directory_addresses(uri_canonical) WHERE owner_id = '';

-- No personal canonical unique index in v1: legacy personal books may contain
-- duplicate canonical URIs. The migration deduplicates them in Rust before
-- insert instead of silently dropping rows.

-- 0001_initial.sql — первичная схема vks-storage (ADR-0008).
--
-- Совместима с PostgreSQL и SQLite (sqlx runtime-migrate).
-- UUID хранится как TEXT (portable); room mode — 'sfu'|'mcu'.
-- audit_log — append-only (только INSERT), retention 2y (BACKEND_RULES §5.3).

CREATE TABLE IF NOT EXISTS users (
    id         UUID PRIMARY KEY,
    login      TEXT NOT NULL UNIQUE,
    email      TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rooms (
    id         UUID PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    mode       TEXT NOT NULL CHECK (mode IN ('sfu', 'mcu')),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS room_mode_override (
    room_id UUID PRIMARY KEY REFERENCES rooms (id) ON DELETE CASCADE,
    mode    TEXT NOT NULL CHECK (mode IN ('sfu', 'mcu'))
);

CREATE TABLE IF NOT EXISTS layouts (
    id        UUID PRIMARY KEY,
    room_id   UUID NOT NULL REFERENCES rooms (id) ON DELETE CASCADE,
    width     INTEGER NOT NULL,
    height    INTEGER NOT NULL,
    tiles     TEXT NOT NULL  -- JSON: [{"x":..,"y":..,"width":..,"height":..}]
);

CREATE TABLE IF NOT EXISTS api_keys (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    hashed_value TEXT NOT NULL,
    prefix       TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS audit_log (
    id         UUID PRIMARY KEY,
    created_at TEXT NOT NULL,
    event      TEXT NOT NULL,
    subject_id UUID,
    payload    TEXT
);

CREATE INDEX IF NOT EXISTS idx_layouts_room ON layouts (room_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_user ON api_keys (user_id);
CREATE INDEX IF NOT EXISTS idx_audit_event ON audit_log (event);

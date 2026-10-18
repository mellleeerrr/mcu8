-- 0003_core_users_and_audit.sql — персистентность пользователей ядра и аудит-лога (M0-M1).

CREATE TABLE IF NOT EXISTS core_users (
    id         TEXT PRIMARY KEY,
    username   TEXT NOT NULL,
    role       TEXT NOT NULL,
    muted      BOOLEAN NOT NULL,
    deleted    BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS core_audit_log (
    id         TEXT PRIMARY KEY,
    ts_ms      BIGINT NOT NULL,
    user_name  TEXT NOT NULL,
    action     TEXT NOT NULL,
    room       TEXT,
    ip         TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_core_audit_ts ON core_audit_log (ts_ms);
CREATE INDEX IF NOT EXISTS idx_core_users_username ON core_users (username);

-- 0002_core_persistence.sql — хранение сущностей ядра и системных настроек (M1.5).

CREATE TABLE IF NOT EXISTS core_layouts (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    version        INTEGER NOT NULL,
    assigned_rooms TEXT NOT NULL,
    is_default     BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS core_api_keys (
    id      TEXT PRIMARY KEY,
    scopes  TEXT NOT NULL,
    owner   TEXT NOT NULL,
    revoked BOOLEAN NOT NULL
);

CREATE TABLE IF NOT EXISTS server_settings (
    id         TEXT PRIMARY KEY CHECK (id = 'current'),
    data_json  TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

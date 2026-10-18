-- 0004_user_credentials.sql — персистентность учетных данных и отметок смены паролей (M1.3).

CREATE TABLE IF NOT EXISTS user_credentials (
    user_id             TEXT PRIMARY KEY,
    password_hash_hex   TEXT NOT NULL,
    salt_hex            TEXT NOT NULL,
    password_changed_at BIGINT,
    generation          BIGINT NOT NULL DEFAULT 1
);

-- Durable recording metadata (T09a). Media bytes stay in the container store.
CREATE TABLE IF NOT EXISTS video_records (
    id TEXT PRIMARY KEY,
    room_id TEXT NOT NULL,
    created_ms BIGINT NOT NULL,
    duration_sec BIGINT NOT NULL,
    size_bytes BIGINT NOT NULL,
    bitrate_kbps BIGINT NOT NULL DEFAULT 0,
    width BIGINT NOT NULL DEFAULT 0,
    height BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_video_records_room ON video_records(room_id, created_ms);

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tasks (
    uuid TEXT PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS artifacts (
    uuid TEXT PRIMARY KEY,
    task_uuid TEXT NOT NULL REFERENCES tasks(uuid) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK(length(type) > 0),
    name TEXT,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS artifacts_task_uuid_idx
    ON artifacts(task_uuid, created_at, uuid);
CREATE INDEX IF NOT EXISTS artifacts_task_type_idx
    ON artifacts(task_uuid, type);

CREATE TABLE IF NOT EXISTS annotations (
    uuid TEXT PRIMARY KEY,
    artifact_uuid TEXT NOT NULL REFERENCES artifacts(uuid) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('comment', 'question', 'scratch', 'good')),
    start_offset INTEGER,
    end_offset INTEGER,
    selected_text TEXT,
    body TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT,
    CHECK((start_offset IS NULL) = (end_offset IS NULL)),
    CHECK(start_offset IS NULL OR (start_offset >= 0 AND end_offset >= start_offset))
);

CREATE INDEX IF NOT EXISTS annotations_artifact_uuid_idx
    ON annotations(artifact_uuid, created_at, uuid);

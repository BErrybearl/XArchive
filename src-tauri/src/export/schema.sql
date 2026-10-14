BEGIN IMMEDIATE;
CREATE TABLE export_directories (
    export_id INTEGER PRIMARY KEY,
    target_user_id TEXT NOT NULL REFERENCES users(user_id),
    output_root TEXT NOT NULL,
    directory TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX export_latest ON export_directories(target_user_id, export_id DESC);
CREATE TABLE export_files (
    export_id INTEGER NOT NULL REFERENCES export_directories(export_id),
    entity_id INTEGER NOT NULL REFERENCES media_entities(entity_id),
    relative_path TEXT NOT NULL,
    byte_length INTEGER NOT NULL CHECK (byte_length > 0),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'written')),
    PRIMARY KEY (export_id, entity_id),
    UNIQUE (export_id, relative_path)
);
PRAGMA user_version = 5;
COMMIT;

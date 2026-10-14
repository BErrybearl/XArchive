BEGIN IMMEDIATE;
CREATE TABLE archive_runs (
 target_user_id TEXT PRIMARY KEY REFERENCES users(user_id),
 last_full_at INTEGER, last_incremental_at INTEGER, full_scope TEXT, summary TEXT NOT NULL
);
PRAGMA user_version=3;
COMMIT;

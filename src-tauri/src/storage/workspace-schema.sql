BEGIN IMMEDIATE;
CREATE TABLE account_profiles (
 screen_name TEXT PRIMARY KEY COLLATE NOCASE,
 target_user_id TEXT UNIQUE REFERENCES users(user_id),
 settings_json TEXT NOT NULL
);
CREATE TABLE workspace_state (
 id INTEGER PRIMARY KEY CHECK(id=1), selected_account TEXT NOT NULL,
 last_settings_json TEXT NOT NULL
);
ALTER TABLE archive_runs ADD COLUMN full_settings_json TEXT;
-- File identity may have references from several accounts. Deduplication remains per account.
CREATE TABLE media_sources_v4 (
 source_id INTEGER PRIMARY KEY, target_user_id TEXT NOT NULL, tweet_id TEXT NOT NULL,
 media_key TEXT NOT NULL CHECK(length(media_key)>0), entity_id INTEGER NOT NULL,
 original_filename TEXT NOT NULL, original_url TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('image','video','gif')),
 UNIQUE(target_user_id,tweet_id,media_key),
 FOREIGN KEY(target_user_id,tweet_id) REFERENCES archive_tweets(target_user_id,tweet_id) ON DELETE RESTRICT,
 FOREIGN KEY(entity_id) REFERENCES media_entities(entity_id) ON DELETE RESTRICT
);
INSERT INTO media_sources_v4 SELECT * FROM media_sources;
DROP TABLE media_sources;
ALTER TABLE media_sources_v4 RENAME TO media_sources;
CREATE INDEX sources_by_entity ON media_sources(entity_id);
PRAGMA user_version=4;
COMMIT;

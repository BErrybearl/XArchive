-- Initial metadata schema for a new, empty archive only.
-- The Rust storage layer must check existing tables and user_version before use.
PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE users (
    user_id TEXT PRIMARY KEY CHECK (length(user_id) > 0),
    screen_name TEXT NOT NULL CHECK (length(screen_name) > 0),
    display_name TEXT
);

CREATE TABLE archives (
    target_user_id TEXT PRIMARY KEY REFERENCES users(user_id) ON DELETE RESTRICT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE tweets (
    tweet_id TEXT PRIMARY KEY CHECK (length(tweet_id) > 0),
    author_user_id TEXT NOT NULL REFERENCES users(user_id) ON DELETE RESTRICT,
    text TEXT,
    published_at TEXT
);

CREATE TABLE archive_tweets (
    target_user_id TEXT NOT NULL REFERENCES archives(target_user_id) ON DELETE RESTRICT,
    tweet_id TEXT NOT NULL REFERENCES tweets(tweet_id) ON DELETE RESTRICT,
    PRIMARY KEY (target_user_id, tweet_id)
);

CREATE TABLE media_entities (
    entity_id INTEGER PRIMARY KEY,
    target_user_id TEXT NOT NULL REFERENCES archives(target_user_id) ON DELETE RESTRICT,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64 AND sha256 NOT GLOB '*[^0-9a-f]*'),
    byte_length INTEGER NOT NULL CHECK (byte_length > 0),
    relative_path TEXT NOT NULL UNIQUE CHECK (
        relative_path LIKE 'media/%'
        AND instr(relative_path, '..') = 0
        AND instr(relative_path, char(92)) = 0
    ),
    mime_type TEXT NOT NULL,
    saved_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (target_user_id, entity_id)
);

-- Candidate lookup only; matching hashes still require byte comparison.
-- Hash collisions can have separate entity rows and paths.
CREATE INDEX media_candidates ON media_entities(target_user_id, sha256, byte_length);

CREATE TABLE media_sources (
    source_id INTEGER PRIMARY KEY,
    target_user_id TEXT NOT NULL,
    tweet_id TEXT NOT NULL,
    media_key TEXT NOT NULL CHECK (length(media_key) > 0),
    entity_id INTEGER NOT NULL,
    original_filename TEXT NOT NULL,
    original_url TEXT NOT NULL,
    media_type TEXT NOT NULL CHECK (media_type IN ('image', 'video', 'gif')),
    UNIQUE (target_user_id, tweet_id, media_key),
    FOREIGN KEY (target_user_id, tweet_id) REFERENCES archive_tweets(target_user_id, tweet_id) ON DELETE RESTRICT,
    FOREIGN KEY (target_user_id, entity_id) REFERENCES media_entities(target_user_id, entity_id) ON DELETE RESTRICT
);

CREATE INDEX sources_by_entity ON media_sources(entity_id);
CREATE INDEX tweets_by_author ON tweets(author_user_id);

PRAGMA user_version = 1;
PRAGMA application_id = 1480675907;
COMMIT;

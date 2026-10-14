BEGIN IMMEDIATE;
CREATE TABLE archive_checkpoints (
 target_user_id TEXT NOT NULL REFERENCES users(user_id), mode TEXT NOT NULL,
 cursor TEXT, exhausted INTEGER NOT NULL DEFAULT 0 CHECK (exhausted IN (0,1)),
 scan_newest INTEGER, completed_at INTEGER, completed_newest INTEGER,
 PRIMARY KEY(target_user_id,mode)
);
CREATE TABLE archive_retries (
 target_user_id TEXT NOT NULL, tweet_id TEXT NOT NULL, media_key TEXT NOT NULL,
 media_type TEXT NOT NULL CHECK(media_type IN ('image','video','gif')),
 payload TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
 reason TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','failed')),
 PRIMARY KEY(target_user_id,tweet_id,media_key),
 FOREIGN KEY(target_user_id,tweet_id) REFERENCES archive_tweets(target_user_id,tweet_id)
);
PRAGMA user_version=2;
COMMIT;

BEGIN IMMEDIATE;
ALTER TABLE media_entities ADD COLUMN kind TEXT NOT NULL DEFAULT 'image';
ALTER TABLE media_entities ADD COLUMN ext TEXT NOT NULL DEFAULT 'bin';
ALTER TABLE media_entities ADD COLUMN naming_account TEXT NOT NULL DEFAULT '';
ALTER TABLE media_sources ADD COLUMN sequence INTEGER NOT NULL DEFAULT 0;
UPDATE media_sources SET sequence=(SELECT count(*) FROM media_sources x WHERE x.target_user_id=media_sources.target_user_id AND x.tweet_id=media_sources.tweet_id AND x.source_id<=media_sources.source_id);
UPDATE media_entities SET kind=coalesce((SELECT media_type FROM media_sources s WHERE s.entity_id=media_entities.entity_id ORDER BY source_id LIMIT 1),'image');
CREATE TABLE storage_layout (singleton INTEGER PRIMARY KEY CHECK(singleton=1), layout TEXT NOT NULL CHECK(layout IN ('A','C')));
INSERT INTO storage_layout VALUES(1,'C');
CREATE TABLE media_rename_log (
    entity_id INTEGER PRIMARY KEY REFERENCES media_entities(entity_id) ON DELETE CASCADE,
    old_path TEXT NOT NULL,
    new_path TEXT NOT NULL,
    kind TEXT NOT NULL,
    ext TEXT NOT NULL,
    naming_account TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('planned','renamed','done'))
);
PRAGMA user_version = 6;
COMMIT;

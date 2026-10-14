import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { test } from 'node:test';

const schema = readFileSync(new URL('../src-tauri/src/storage/schema.sql', import.meta.url), 'utf8');

function createSample() {
  const db = new DatabaseSync(':memory:');
  db.exec(schema);
  db.exec(`
    INSERT INTO users VALUES ('100', 'sample_target', NULL), ('200', 'sample_author', NULL);
    INSERT INTO archives (target_user_id) VALUES ('100');
    INSERT INTO tweets VALUES ('18446744073709551601', '100', 'sample one', NULL),
                              ('18446744073709551602', '200', 'sample two', NULL);
    INSERT INTO archive_tweets VALUES ('100', '18446744073709551601'), ('100', '18446744073709551602');
  `);
  return db;
}

function addEntity(db, digest, path) {
  return db.prepare(`INSERT INTO media_entities
    (target_user_id, sha256, byte_length, relative_path, mime_type)
    VALUES ('100', ?, 4, ?, 'image/jpeg')`).run(digest, path).lastInsertRowid;
}

function addSource(db, tweet, key, entity) {
  return db.prepare(`INSERT INTO media_sources
    (target_user_id, tweet_id, media_key, entity_id, original_filename, original_url, media_type)
    VALUES ('100', ?, ?, ?, 'same-name.jpg', 'https://example.invalid/sample.jpg', 'image')`).run(tweet, key, entity);
}

test('multiple tweet and author sources point to one media entity without losing metadata', () => {
  const db = createSample();
  try {
    const entity = addEntity(db, 'a'.repeat(64), 'media/100/aa/a.jpg');
    addSource(db, '18446744073709551601', 'image:0', entity);
    addSource(db, '18446744073709551602', 'image:0', entity);
    const rows = db.prepare(`SELECT s.tweet_id, t.author_user_id, s.original_filename, s.entity_id
      FROM media_sources s JOIN tweets t ON s.tweet_id = t.tweet_id ORDER BY s.tweet_id`).all();
    assert.equal(rows.length, 2);
    assert.deepEqual(rows.map((row) => row.author_user_id), ['100', '200']);
    assert.equal(rows[1].tweet_id, '18446744073709551602');
    assert.equal(rows[0].entity_id, rows[1].entity_id);
    assert.equal(db.prepare('SELECT COUNT(*) AS count FROM media_entities').get().count, 1);
    assert.throws(() => addSource(db, rows[0].tweet_id, 'image:0', entity), /UNIQUE/);
  } finally { db.close(); }
});

test('same original filenames and equal hash candidates can preserve distinct media entities', () => {
  const db = createSample();
  try {
    const first = addEntity(db, 'a'.repeat(64), 'media/100/aa/a.jpg');
    const second = addEntity(db, 'a'.repeat(64), 'media/100/aa/a-collision.jpg');
    addSource(db, '18446744073709551601', 'image:0', first);
    addSource(db, '18446744073709551602', 'image:0', second);
    assert.notEqual(first, second);
    assert.equal(db.prepare('SELECT COUNT(*) AS count FROM media_entities').get().count, 2);
    assert.equal(db.prepare("SELECT COUNT(*) AS count FROM media_sources WHERE original_filename = 'same-name.jpg'").get().count, 2);
  } finally { db.close(); }
});

test('cross-account source references and paths outside the media directory are rejected', () => {
  const db = createSample();
  try {
    const entity = addEntity(db, 'a'.repeat(64), 'media/100/aa/a.jpg');
    db.exec("INSERT INTO archives (target_user_id) VALUES ('200'); INSERT INTO archive_tweets VALUES ('200', '18446744073709551601');");
    assert.throws(() => db.prepare(`INSERT INTO media_sources
      (target_user_id, tweet_id, media_key, entity_id, original_filename, original_url, media_type)
      VALUES ('200', '18446744073709551601', 'image:0', ?, 'sample.jpg', 'https://example.invalid/sample', 'image')`).run(entity), /FOREIGN KEY/);
    for (const path of ['../private/sample.jpg', 'media/../../sample.jpg', 'C:\\sample.jpg', 'media/100\\sample.jpg']) {
      assert.throws(() => addEntity(db, 'b'.repeat(64), path), /CHECK/);
    }
  } finally { db.close(); }
});

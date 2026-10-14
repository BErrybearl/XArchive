use super::*;
use rusqlite::params;

fn isolated() -> tempfile::TempDir {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/legacy-tests");
    fs::create_dir_all(&directory).unwrap();
    tempfile::tempdir_in(directory).unwrap()
}
/// Synthetic v4 columns and aliases from reference/python/source/xdl/{store,exact_dedupe}.py.
/// No legacy implementation is executed and no binary fixture is checked in.
fn fixture(file: &Path) {
    let db = Connection::open(file).unwrap();
    db.execute_batch(r#"CREATE TABLE users(id INTEGER PRIMARY KEY,user_id TEXT UNIQUE NOT NULL,screen_name TEXT,display_name TEXT);
        CREATE TABLE tweets(tweet_id TEXT PRIMARY KEY,user_id TEXT NOT NULL,text TEXT,created_at TEXT);
        CREATE TABLE media(id INTEGER PRIMARY KEY,user_id TEXT NOT NULL,tweet_id TEXT NOT NULL,filename TEXT NOT NULL,media_type TEXT NOT NULL,url TEXT,data BLOB NOT NULL,downloaded_at TEXT,sha256 TEXT,dhash INTEGER,UNIQUE(tweet_id,filename));
        CREATE TABLE media_aliases(user_id TEXT NOT NULL,tweet_id TEXT NOT NULL,filename TEXT NOT NULL,media_type TEXT NOT NULL,url TEXT,downloaded_at TEXT,sha256 TEXT NOT NULL,canonical_id INTEGER NOT NULL REFERENCES media(id),PRIMARY KEY(tweet_id,filename));
        CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT);
        INSERT INTO users(user_id,screen_name,display_name) VALUES('a','alpha','合成甲'),('b','beta','合成乙');
        INSERT INTO tweets VALUES('t1','a','共享内容一','2026-01-01'),('t2','a','共享内容二','2026-01-02'),('t3','a','同名不同内容','2026-01-03'),('t4','b','跨账号来源','2026-01-04'),('t5','a','纯文字','2026-01-05');
        INSERT INTO settings VALUES('schema_version','4');
        INSERT INTO settings VALUES('pending:media:a','[{"tweet_id":"retry1","author_id":"a","author_screen_name":"alpha","author_full_name":"合成甲","url":"https://example.invalid/retry.jpg","filename":"retry.jpg","media_type":"photo","tweet_text":"未下载","tweet_created_at":"2026-01-06"}]');"#).unwrap();
    let shared = vec![0x73u8; 64 * 1024 * 3 + 17];
    let other = vec![0x74u8; shared.len()];
    let hash = digest(&shared[..]).unwrap().0;
    db.execute("INSERT INTO media(id,user_id,tweet_id,filename,media_type,url,data,sha256) VALUES(1,'a','t1','same.jpg','photo','https://example.invalid/1',?1,?2)",params![shared,hash]).unwrap();
    db.execute("INSERT INTO media(id,user_id,tweet_id,filename,media_type,url,data,sha256) VALUES(2,'a','t3','same.jpg','photo','https://example.invalid/3',?1,?2)",params![other,digest(&other[..]).unwrap().0]).unwrap();
    db.execute("INSERT INTO media_aliases(user_id,tweet_id,filename,media_type,url,sha256,canonical_id) VALUES('a','t2','alias.jpg','photo','https://example.invalid/2',?1,1),('b','t4','same.jpg','photo','https://example.invalid/4',?1,1)",[hash]).unwrap();
}
fn run(root: &Path, file: &Path) -> Report {
    import(root, file, &AtomicBool::new(false), |_| {}).unwrap()
}

#[test]
fn retweet_authors_are_metadata_only_and_accounts_follow_source_targets() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let db = Connection::open(&file).unwrap();
    db.execute_batch(
        "INSERT INTO users(user_id,screen_name,display_name) VALUES('c','original','原帖作者');
        UPDATE tweets SET user_id='c' WHERE tweet_id IN ('t1','t2');
        INSERT INTO tweets VALUES('text-c','c','作者的文字','2026-01-06');",
    )
    .unwrap();
    drop(db);
    let root = dir.path().join("data");
    let report = run(&root, &file);
    assert!(report.passed, "{report:?}");
    let accounts = storage::list_archived_accounts(&root).unwrap();
    assert_eq!(
        accounts
            .iter()
            .map(|a| a.user_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
    let store = Store::open(&root).unwrap();
    let c = store.connection();
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM archive_tweets WHERE target_user_id='c'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(c.query_row("SELECT count(*) FROM archive_tweets WHERE target_user_id='a' AND tweet_id IN ('t1','t2','t5')",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM tweets WHERE author_user_id='c'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM archive_tweets WHERE tweet_id='text-c'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn exact_sources_account_isolation_retry_and_repeat() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let root = dir.path().join("data");
    let report = run(&root, &file);
    assert!(report.passed, "{report:?}");
    assert_eq!(
        (
            report.counts.old_tweets,
            report.counts.old_media_rows,
            report.counts.old_sources
        ),
        (5, 2, 4)
    );
    assert_eq!(
        (
            report.counts.imported_tweets,
            report.counts.expected_media,
            report.counts.imported_media,
            report.counts.imported_sources
        ),
        (5, 3, 3, 4)
    );
    assert_eq!(report.counts.digests_checked, 4);
    assert_eq!(report.pending.len(), 1);
    assert_eq!(report.pending[0].items[0]["filename"], "retry.jpg");
    assert!(Path::new(&report.snapshot).starts_with(root.join("temporary")));
    assert!(Path::new(&report.report_path).is_file());
    let db = Connection::open_with_flags(
        root.join("archive.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let ids: Vec<(String, String, i64)> = db
        .prepare("SELECT target_user_id,tweet_id,entity_id FROM media_sources ORDER BY tweet_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(ids[0].2, ids[1].2);
    assert_ne!(ids[0].2, ids[2].2);
    assert_ne!(ids[0].2, ids[3].2);
    drop(db);
    let again = run(&root, &file);
    assert!(again.passed, "{again:?}");
    let status = storage::status(&root).unwrap();
    assert_eq!((status.tweets, status.media, status.sources), (5, 3, 4));
}

#[test]
fn stopped_import_resumes_from_committed_sources() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let root = dir.path().join("data");
    let cancel = AtomicBool::new(false);
    let report = import(&root, &file, &cancel, |p| {
        if p.stage == "导入媒体与来源" && p.done == 1 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap();
    assert!(report.stopped);
    assert!(!report.passed);
    assert_eq!(storage::status(&root).unwrap().sources, 1);
    let store = Store::open(&root).unwrap();
    assert!(store.inspect().unwrap().missing_files.is_empty());
    drop(store);
    let report = run(&root, &file);
    assert!(report.passed, "{report:?}");
    assert_eq!(storage::status(&root).unwrap().sources, 4);
}

#[test]
fn readonly_original_unchanged_and_no_sidecars_created() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let before = storage::hash_file(&file).unwrap();
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&file, permissions).unwrap();
    let report = run(&dir.path().join("data"), &file);
    assert!(report.passed, "{report:?}");
    assert_eq!(before, storage::hash_file(&file).unwrap());
    assert_eq!(report.source_sha256_before, report.source_sha256_after);
    assert!(!dir.path().join("legacy.db-wal").exists());
    assert!(!dir.path().join("legacy.db-shm").exists());
    // TempDir cleanup needs to remove the synthetic readonly file on Windows.
    let mut permissions = fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(file, permissions).unwrap();
}

#[test]
fn backup_includes_committed_wal_without_checkpointing_original() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let writer = Connection::open(&file).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;INSERT INTO tweets VALUES('wal','a','合成 WAL','2026-01-07');").unwrap();
    let main = storage::hash_file(&file).unwrap();
    let wal = dir.path().join("legacy.db-wal");
    let wal_before = storage::hash_file(&wal).unwrap();
    let report = run(&dir.path().join("data"), &file);
    assert!(report.passed, "{report:?}");
    assert_eq!(report.counts.old_tweets, 6);
    assert_eq!(main, storage::hash_file(&file).unwrap());
    assert_eq!(wal_before, storage::hash_file(&wal).unwrap());
    drop(writer);
}

#[test]
fn corrupted_media_and_metadata_report_unpassed() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let root = dir.path().join("data");
    assert!(run(&root, &file).passed);
    let db = Connection::open(root.join("archive.sqlite3")).unwrap();
    let name: String = db
        .query_row(
            "SELECT relative_path FROM media_entities ORDER BY entity_id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let media = root.join(name);
    let bytes = fs::read(&media).unwrap();
    let mut corrupt = bytes.clone();
    corrupt[0] ^= 1;
    fs::write(&media, corrupt).unwrap();
    db.execute(
        "UPDATE media_sources SET original_url='changed' WHERE tweet_id='t3'",
        [],
    )
    .unwrap();
    drop(db);
    let report = run(&root, &file);
    assert!(!report.passed);
    assert!(report.mismatches.iter().any(|s| s.contains("摘要")));
    assert!(report.mismatches.iter().any(|s| s.contains("来源元信息")));
    let saved: serde_json::Value =
        serde_json::from_reader(File::open(report.report_path).unwrap()).unwrap();
    assert_eq!(saved["passed"], false);
}

#[test]
fn missing_alias_blob_and_bad_legacy_hash_are_not_success() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let db = Connection::open(&file).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    db.execute(
        "UPDATE media_aliases SET canonical_id=999 WHERE tweet_id='t4'",
        [],
    )
    .unwrap();
    db.execute("UPDATE media SET sha256=?1 WHERE id=1", ["0".repeat(64)])
        .unwrap();
    drop(db);
    let report = run(&dir.path().join("data"), &file);
    assert!(!report.passed);
    assert!(!report.missing.is_empty());
    assert!(report.mismatches.iter().any(|s| s.contains("旧库摘要")));
}

#[test]
fn malformed_pending_and_changed_tweet_fail_verification() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    let root = dir.path().join("data");
    assert!(run(&root, &file).passed);
    let db = Connection::open(&file).unwrap();
    db.execute(
        "UPDATE settings SET value='not-json' WHERE key LIKE 'pending:%'",
        [],
    )
    .unwrap();
    db.execute("UPDATE tweets SET text='changed' WHERE tweet_id='t1'", [])
        .unwrap();
    drop(db);
    let report = run(&root, &file);
    assert!(!report.passed);
    assert!(report.mismatches.iter().any(|s| s.contains("推文元信息")));
    assert!(report.mismatches.iter().any(|s| s.contains("待重试")));
    assert_eq!(report.pending[0].items, "not-json");
}

#[test]
fn development_destination_rejects_outside_runtime_and_traversal() {
    let dir = isolated();
    let file = dir.path().join("legacy.db");
    fixture(&file);
    assert!(
        import(
            &dir.path().join("../escaped"),
            &file,
            &AtomicBool::new(false),
            |_| {}
        )
        .is_err()
    );
    if cfg!(debug_assertions) {
        assert!(
            import(
                &std::env::temp_dir().join("xarchive-not-allowed"),
                &file,
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
    }
}

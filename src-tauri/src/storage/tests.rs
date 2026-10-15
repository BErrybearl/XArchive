use super::*;

#[cfg(windows)]
#[test]
fn friendly_windows_paths_preserve_drive_unc_and_ordinary_paths() {
    for (input, expected) in [
        (r"\\?\C:\archive\媒体", r"C:\archive\媒体"),
        (r"\\?\UNC\server\share\archive", r"\\server\share\archive"),
        (r"C:\archive", r"C:\archive"),
        (r"\\server\share", r"\\server\share"),
        (r"\\?\Volume{abc}\archive", r"\\?\Volume{abc}\archive"),
    ] {
        assert_eq!(friendly_path(Path::new(input)), Path::new(expected));
    }
}

#[cfg(windows)]
#[test]
fn cloud_and_dedup_tags_are_allowed_while_redirecting_tags_are_rejected() {
    for tag in [0x9000_001a, 0x9000_101a, 0x9000_f01a, 0x8000_0013] {
        assert!(!redirecting_tag(tag));
    }
    for tag in [0xa000_000c, 0xa000_0003, 0x8000_001b] {
        assert!(redirecting_tag(tag));
    }
}

#[cfg(windows)]
#[test]
fn junction_and_its_children_are_rejected_without_touching_target() {
    let dir = isolated_directory();
    let target = dir.path().join("target");
    let junction = dir.path().join("junction");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("kept"), b"keep").unwrap();
    let output = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(reparse_tag(&junction).unwrap(), 0xa000_0003);
    let refused = check_path(&junction).is_err() && check_path(&junction.join("kept")).is_err();
    fs::remove_dir(&junction).unwrap();
    assert!(refused);
    assert_eq!(fs::read(target.join("kept")).unwrap(), b"keep");
    assert!(check_path(&target).is_ok());
}

pub(super) fn sample<'a>(tweet_id: &'a str, author_id: &'a str) -> MediaSource<'a> {
    MediaSource {
        sequence: None,
        target: User {
            id: "19000000000000000001",
            screen_name: "target",
            display_name: None,
        },
        author: User {
            id: author_id,
            screen_name: "author",
            display_name: Some("示例作者"),
        },
        tweet_id,
        text: Some("合成测试，非真实推文"),
        published_at: Some("2026-10-11T00:00:00Z"),
        media_key: "photo-1",
        original_filename: "same-name.jpg",
        original_url: "https://example.invalid/same-name.jpg",
        media_type: "image",
        mime_type: "image/jpeg",
    }
}

pub(super) fn isolated_directory() -> tempfile::TempDir {
    let project = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let tests = project.join(".runtime").join("storage-tests");
    fs::create_dir_all(&tests).unwrap();
    tempfile::tempdir_in(tests).unwrap()
}

pub(super) fn migration_fixture(root: &Path, version: i64) -> Connection {
    let c = Connection::open(root.join(DATABASE)).unwrap();
    c.execute_batch(include_str!("schema.sql")).unwrap();
    for (from, schema) in [
        (1, include_str!("archive-schema.sql")),
        (2, include_str!("runs-schema.sql")),
        (3, include_str!("workspace-schema.sql")),
        (4, include_str!("../export/schema.sql")),
    ] {
        if version > from {
            c.execute_batch(schema).unwrap();
        }
    }
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         INSERT INTO users VALUES('keep','kept','保留账号');
         INSERT INTO archives(target_user_id) VALUES('keep');
         INSERT INTO tweets VALUES('tweet','keep','保留文本','2026-10-14');
         INSERT INTO archive_tweets VALUES('keep','tweet');
         INSERT INTO media_entities(entity_id,target_user_id,sha256,byte_length,relative_path,mime_type)
         VALUES(7,'keep','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',4,'media/kept.bin','image/jpeg');
         INSERT INTO media_sources VALUES(9,'keep','tweet','photo',7,'kept.jpg','https://example.invalid/kept','image');",
    ).unwrap();
    if version >= 4 {
        c.execute_batch(
            "INSERT INTO account_profiles VALUES('kept','keep','{}');
            INSERT INTO workspace_state VALUES(1,'kept','{}');",
        )
        .unwrap();
    }
    c
}

#[test]
fn migration_chain_v1_through_v5_to_v6_preserves_data_and_backs_up_once() {
    for version in 1..=5 {
        let d = isolated_directory();
        let c = migration_fixture(d.path(), version);
        // Read-only paths leave the old index untouched.
        let read = Store::open_readonly(d.path()).unwrap();
        assert_eq!(
            read.connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
        drop(read);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(
            s.connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert_eq!(s.connection.query_row("SELECT t.text FROM tweets t JOIN media_sources m USING(tweet_id) WHERE m.source_id=9 AND m.entity_id=7", [], |r| r.get::<_, String>(0)).unwrap(), "保留文本");
        assert_eq!(
            s.connection
                .query_row(
                    "SELECT relative_path FROM media_entities WHERE entity_id=7",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "media/kept.bin"
        );
        s.connection
            .prepare("SELECT full_settings_json FROM archive_runs")
            .unwrap();
        s.connection
            .prepare(
                "SELECT * FROM account_profiles,workspace_state,export_directories,export_files",
            )
            .unwrap();
        assert_eq!(
            count(
                &s.connection,
                "SELECT count(*) FROM pragma_foreign_key_check"
            )
            .unwrap(),
            0
        );
        if version >= 4 {
            assert_eq!(
                s.connection
                    .query_row("SELECT selected_account FROM workspace_state", [], |r| {
                        r.get::<_, String>(0)
                    })
                    .unwrap(),
                "kept"
            );
        }
        let backups: Vec<_> = fs::read_dir(d.path().join("temporary"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        let b = Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(
            b.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
        assert_eq!(
            count(&b, "SELECT count(*) FROM media_sources WHERE source_id=9").unwrap(),
            1
        );
        drop(b);
        drop(s);
        drop(Store::open(d.path()).unwrap());
        assert_eq!(
            fs::read_dir(d.path().join("temporary"))
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "sqlite3"))
                .count(),
            1
        );
        drop(c);
    }
}

#[test]
fn migration_chain_failure_rolls_back_every_step_and_keeps_original_wal_backup() {
    for version in 1..=5 {
        // Fail at intermediate steps and at the v6 layout step, rolling back
        // ALTER TABLE as well as all previous steps in the same transaction.
        for collision in ["account_profiles", "export_files", "storage_layout"] {
            if version >= 4 && collision == "account_profiles"
                || version >= 5 && collision == "export_files"
            {
                continue;
            }
            let d = isolated_directory();
            let c = migration_fixture(d.path(), version);
            c.execute_batch(&format!("CREATE TABLE {collision}(existing TEXT);"))
                .unwrap();
            let before: Vec<(String, String)> = c
                .prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(Store::open(d.path()).is_err());
            assert_eq!(
                c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                version
            );
            let after: Vec<(String, String)> = c
                .prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(after, before);
            assert_eq!(
                count(&c, "SELECT count(*) FROM media_sources WHERE source_id=9").unwrap(),
                1
            );
            let backups: Vec<_> = fs::read_dir(d.path().join("temporary"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(backups.len(), 1);
            let b =
                Connection::open_with_flags(&backups[0], OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(
                b.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                version
            );
            assert_eq!(
                b.query_row("SELECT text FROM tweets WHERE tweet_id='tweet'", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
                "保留文本"
            );
        }
    }
}

#[test]
fn new_and_existing_empty_databases_get_complete_v6_without_backup() {
    for existing in [false, true] {
        let d = isolated_directory();
        if existing {
            drop(Connection::open(d.path().join(DATABASE)).unwrap());
        }
        let s = Store::open(d.path()).unwrap();
        assert_eq!(
            s.connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        s.connection
            .prepare(
                "SELECT * FROM account_profiles,workspace_state,export_directories,export_files",
            )
            .unwrap();
        assert_eq!(fs::read_dir(d.path().join("temporary")).unwrap().count(), 0);
    }
}

#[test]
fn unknown_or_future_database_versions_are_rejected_before_any_write() {
    for (application_id, version) in [
        (APPLICATION_ID, 0),
        (APPLICATION_ID, 7),
        (APPLICATION_ID, 99),
        (42, 3),
    ] {
        let d = isolated_directory();
        let c = Connection::open(d.path().join(DATABASE)).unwrap();
        c.execute_batch(include_str!("schema.sql")).unwrap();
        c.pragma_update(None, "application_id", application_id)
            .unwrap();
        c.pragma_update(None, "user_version", version).unwrap();
        drop(c);
        let before = fs::read(d.path().join(DATABASE)).unwrap();
        assert!(Store::open(d.path()).is_err());
        assert!(Store::open_readonly(d.path()).is_err());
        assert_eq!(before, fs::read(d.path().join(DATABASE)).unwrap());
        assert!(!d.path().join("temporary").exists());
        assert!(!d.path().join("media").exists());
    }
}

#[test]
fn exact_bytes_keep_every_source_and_restart_idempotently() {
    let directory = isolated_directory();
    let root = directory.path().join("data");
    assert!(!status(&root).unwrap().initialized);
    assert!(
        !root.exists(),
        "read-only status must not create a directory"
    );
    let mut store = Store::open(&root).unwrap();
    let bytes = vec![0x51; BUFFER_SIZE * 2 + 7];
    let first = store
        .save_media(&sample("20000000000000000001", "author-a"), &bytes[..])
        .unwrap();
    let second = store
        .save_media(&sample("20000000000000000002", "author-b"), &bytes[..])
        .unwrap();
    assert!(!first.reused);
    assert!(second.reused);
    assert_eq!(first.entity_id, second.entity_id);
    drop(store);
    let mut store = Store::open(&root).unwrap();
    let again = store
        .save_media(&sample("20000000000000000002", "author-b"), &bytes[..])
        .unwrap();
    assert!(again.source_already_present);
    let info = status(&root).unwrap();
    assert_eq!((info.tweets, info.media, info.sources), (2, 1, 2));
    let authors = count(
        &store.connection,
        "SELECT count(DISTINCT author_user_id) FROM tweets",
    )
    .unwrap();
    assert_eq!(authors, 2);
    assert_eq!(
        store.inspect().unwrap(),
        IntegrityReport {
            missing_files: vec![],
            changed_files: vec![],
            untracked_files: vec![],
            temporary_files: 0,
        }
    );
    assert_eq!(fs::read_dir(root.join("media")).unwrap().count(), 1);
}

#[test]
fn same_name_and_hash_candidate_do_not_merge_different_bytes_or_accounts() {
    let directory = isolated_directory();
    let mut store = Store::open(directory.path()).unwrap();
    let first = store
        .save_media(&sample("tweet-a", "author-a"), &b"first"[..])
        .unwrap();
    // Simulate a hash collision in the candidate index. A hash alone must not deduplicate.
    let digest = hex_digest(&Sha256::digest(b"other"));
    store
        .connection
        .execute("UPDATE media_entities SET sha256=?1", [digest])
        .unwrap();
    let second = store
        .save_media(&sample("tweet-b", "author-b"), &b"other"[..])
        .unwrap();
    assert!(!second.reused);
    assert_ne!(first.entity_id, second.entity_id);
    let mut another_account = sample("tweet-c", "author-c");
    another_account.target.id = "another-account";
    let third = store.save_media(&another_account, &b"other"[..]).unwrap();
    assert!(!third.reused);
    assert_ne!(second.entity_id, third.entity_id);
    let info = status(directory.path()).unwrap();
    assert_eq!((info.media, info.sources), (3, 3));
    assert_eq!(store.inspect().unwrap().untracked_files.len(), 0);
}

#[test]
fn interrupted_input_and_failed_commit_never_leave_missing_indexed_files() {
    struct BrokenInput(bool);
    impl Read for BrokenInput {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("synthetic interruption"));
            }
            self.0 = true;
            buffer[..4].copy_from_slice(b"part");
            Ok(4)
        }
    }
    let directory = isolated_directory();
    let mut store = Store::open(directory.path()).unwrap();
    assert!(
        store
            .save_media(&sample("broken", "author"), BrokenInput(false))
            .is_err()
    );
    assert_eq!(store.inspect().unwrap().temporary_files, 0);
    assert_eq!(status(directory.path()).unwrap().sources, 0);
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER synthetic_failure BEFORE INSERT ON media_sources
         BEGIN SELECT RAISE(ABORT, 'synthetic commit failure'); END;",
        )
        .unwrap();
    assert!(
        store
            .save_media(&sample("failed", "author"), &b"kept for inspection"[..])
            .is_err()
    );
    let report = store.inspect().unwrap();
    assert!(report.missing_files.is_empty());
    assert_eq!(report.untracked_files.len(), 1);
    assert_eq!(status(directory.path()).unwrap().media, 0);
    assert_eq!(status(directory.path()).unwrap().tweets, 0);
    // A later successful write does not silently remove the earlier untracked file.
    store
        .connection
        .execute_batch("DROP TRIGGER synthetic_failure")
        .unwrap();
    store
        .save_media(&sample("next", "author"), &b"successful"[..])
        .unwrap();
    assert_eq!(store.inspect().unwrap().untracked_files.len(), 1);
}

#[test]
fn unknown_database_and_escaped_paths_are_rejected_without_mutation() {
    let directory = isolated_directory();
    let database = directory.path().join(DATABASE);
    let old = Connection::open(&database).unwrap();
    old.execute_batch(
        "CREATE TABLE legacy_data(value TEXT); INSERT INTO legacy_data VALUES ('keep');",
    )
    .unwrap();
    drop(old);
    let original = fs::read(&database).unwrap();
    assert!(Store::open(directory.path()).is_err());
    assert!(status(directory.path()).is_err());
    assert_eq!(original, fs::read(&database).unwrap());
    assert!(!directory.path().join("media").exists());
    for relative in [
        "../outside",
        "media/../../outside",
        "C:\\outside",
        "media\\outside",
        "media/a:stream",
    ] {
        assert!(media_path(directory.path(), relative).is_err());
    }
    assert!(validate_location(Path::new("relative-directory")).is_err());
    if cfg!(debug_assertions) {
        assert!(validate_location(Path::new(env!("CARGO_MANIFEST_DIR"))).is_err());
        assert!(validate_location(directory.path()).is_ok());
    }
}

#[test]
fn intact_changed_source_is_preserved_and_missing_candidate_is_repaired() {
    let directory = isolated_directory();
    let mut store = Store::open(directory.path()).unwrap();
    let source = sample("tweet", "author");
    store.save_media(&source, &b"original"[..]).unwrap();
    assert!(store.save_media(&source, &b"changed!"[..]).is_err());
    assert_eq!(status(directory.path()).unwrap().sources, 1);
    let path: String = store
        .connection
        .query_row("SELECT relative_path FROM media_entities", [], |row| {
            row.get(0)
        })
        .unwrap();
    let media = media_path(directory.path(), &path).unwrap();
    assert_eq!(fs::read(&media).unwrap(), b"original");
    fs::write(&media, b"changed!").unwrap();
    assert_eq!(store.inspect().unwrap().changed_files, vec![path.clone()]);
    fs::remove_file(media).unwrap();
    assert_eq!(store.inspect().unwrap().missing_files, vec![path]);
    let repaired = store
        .save_media(&sample("another-tweet", "author"), &b"original"[..])
        .unwrap();
    assert!(!repaired.reused);
    assert_eq!(status(directory.path()).unwrap().sources, 2);
    assert_eq!(status(directory.path()).unwrap().media, 1);
    assert!(store.inspect().unwrap().missing_files.is_empty());
    assert!(store.inspect().unwrap().changed_files.is_empty());
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT count(*) FROM media_sources WHERE entity_id=?1",
                [repaired.entity_id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
}

#[test]
fn shared_missing_entity_is_repaired_by_non_owner_without_losing_any_source() {
    for changed_length in [false, true] {
        let dir = isolated_directory();
        let mut store = Store::open(dir.path()).unwrap();
        let first = sample("tweet", "author");
        let original = store.save_media(&first, &b"original"[..]).unwrap();
        store.connection().execute_batch("INSERT INTO users VALUES('other','other',NULL);
            INSERT INTO archives(target_user_id) VALUES('other');
            INSERT INTO archive_tweets SELECT 'other',tweet_id FROM archive_tweets;
            INSERT INTO media_sources(target_user_id,tweet_id,media_key,entity_id,original_filename,original_url,media_type)
            SELECT 'other',tweet_id,media_key,entity_id,original_filename,original_url,media_type FROM media_sources;").unwrap();
        let relative: String = store
            .connection()
            .query_row("SELECT relative_path FROM media_entities", [], |r| r.get(0))
            .unwrap();
        let path = media_path(store.root(), &relative).unwrap();
        if changed_length {
            fs::write(&path, b"bad").unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        let mut other = sample("tweet", "author");
        other.target = User {
            id: "other",
            screen_name: "other",
            display_name: None,
        };
        let repaired = store.save_media(&other, &b"original"[..]).unwrap();
        assert_eq!(repaired.entity_id, original.entity_id);
        assert_eq!(
            store
                .connection()
                .query_row(
                    "SELECT count(*) FROM media_sources WHERE entity_id=?1",
                    [original.entity_id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
        let report = store.inspect().unwrap();
        assert!(report.missing_files.is_empty());
        assert!(report.changed_files.is_empty());
        assert_eq!(status(dir.path()).unwrap().media, 1);
    }
}

#[test]
fn saved_location_keeps_media_after_the_portable_directory_moves() {
    let directory = isolated_directory();
    let original = directory.path().join("original");
    let data = original.join("data");
    let mut store = Store::open(&data).unwrap();
    store
        .save_media(&sample("portable-tweet", "author"), &b"portable sample"[..])
        .unwrap();
    drop(store);
    let locator = original.join("location.json");
    persist_location(&locator, &data).unwrap();
    let setting: DataLocation = serde_json::from_reader(File::open(&locator).unwrap()).unwrap();
    assert_eq!(setting.path, Path::new("data"));
    assert_eq!(read_location(&locator, PathBuf::new()).unwrap(), data);

    let moved = directory.path().join("moved");
    assert!(original.starts_with(directory.path()) && moved.starts_with(directory.path()));
    fs::rename(&original, &moved).unwrap();
    let restored = read_location(&moved.join("location.json"), PathBuf::new()).unwrap();
    assert_eq!(restored, moved.join("data"));
    let info = status(&restored).unwrap();
    assert_eq!((info.tweets, info.media, info.sources), (1, 1, 1));
    let store = Store::open(&restored).unwrap();
    let report = store.inspect().unwrap();
    assert!(report.missing_files.is_empty() && report.changed_files.is_empty());
    drop(store);

    fs::write(moved.join("location.json"), br#"{"path":"../outside"}"#).unwrap();
    assert!(read_location(&moved.join("location.json"), PathBuf::new()).is_err());
}

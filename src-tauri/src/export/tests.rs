use super::paths::validate_directory;
use super::*;
use crate::storage::{MediaSource, User};
use rusqlite::Connection;

pub(super) fn fixture_directory() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime")
        .join("export-tests");
    fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    output: PathBuf,
}

#[test]
fn text_only_accounts_do_not_create_export_directories_or_records() {
    let f = Fixture::new();
    let mut s = Store::open(&f.root).unwrap();
    let u = User {
        id: "a",
        screen_name: "alpha",
        display_name: None,
    };
    s.save_tweet(&u, &u, "text", Some("text"), None).unwrap();
    let p = f.run(&f.options(Mode::Full));
    assert_eq!(p.outcome, "completed");
    assert!(p.accounts.is_empty());
    assert_eq!(fs::read_dir(&f.output).unwrap().count(), 0);
    assert_eq!(
        s.connection()
            .query_row("SELECT count(*) FROM export_directories", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn one_account_failure_does_not_stop_exporting_the_rest() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", None, "image", b"one");
    f.add("b", "beta", "2", None, "image", b"two");
    let mut o = f.options(Mode::Full);
    o.account = None;
    let first = f.run(&o);
    let first_account = first.accounts.iter().find(|a| a.account == "a").unwrap();
    let directory = Path::new(&first_account.directory);
    assert!(
        directory
            .canonicalize()
            .unwrap()
            .starts_with(f.output.canonicalize().unwrap())
    );
    fs::remove_dir_all(directory).unwrap();
    o.mode = Mode::Incremental;
    let p = f.run(&o);
    assert_eq!(p.outcome, "completed_with_issues");
    assert!(p.stage.contains("部分完成"));
    assert!(
        p.issues
            .iter()
            .any(|i| i.account == "a" && i.reason == "account_failed")
    );
    assert_eq!(p.accounts.len(), 1);
    assert_eq!(p.accounts[0].account, "b");
    assert_eq!(f.index(&p).len(), 1);
    assert!(!directory.exists());
}

#[test]
fn cancellation_midway_through_index_verification_preserves_both_indexes() {
    let f = Fixture::new();
    for id in ["1", "2"] {
        f.add(
            "a",
            "alpha",
            id,
            None,
            "image",
            &vec![id.as_bytes()[0]; 192 * 1024],
        );
    }
    let first = f.run(&f.options(Mode::Full));
    let directory = Path::new(&first.accounts[0].directory);
    let prior_json = fs::read(directory.join("index.json")).unwrap();
    let prior_csv = fs::read(directory.join("index.csv")).unwrap();
    let store = Store::open(&f.root).unwrap();
    let id: i64 = store
        .connection()
        .query_row("SELECT export_id FROM export_directories", [], |r| r.get(0))
        .unwrap();
    let entities = load_entities(&store, "a").unwrap();
    let stop = AtomicBool::new(false);
    let mut p = Progress::default();
    let mut stopped_at = 0;
    let indexed = rebuild_index(
        &store,
        id,
        directory,
        "a",
        &entities,
        &stop,
        &mut p,
        &mut |p| {
            if p.checked_bytes == 0 {
                // Let the 150 ms progress throttle elapse before the first chunk.
                std::thread::sleep(Duration::from_millis(160));
            }
            if p.checked_bytes > 0 {
                stopped_at = p.checked_bytes;
                stop.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert_eq!(indexed, None);
    assert_eq!(stopped_at, 64 * 1024);
    assert_eq!(fs::read(directory.join("index.json")).unwrap(), prior_json);
    assert_eq!(fs::read(directory.join("index.csv")).unwrap(), prior_csv);
}
impl Fixture {
    fn new() -> Self {
        let d = fixture_directory();
        let root = d.path().join("data");
        let output = d.path().join("exports");
        fs::create_dir_all(&output).unwrap();
        Store::open(&root).unwrap();
        Self {
            _dir: d,
            root,
            output,
        }
    }
    fn add(
        &self,
        account: &str,
        screen: &str,
        tweet: &str,
        date: Option<&str>,
        kind: &str,
        body: &[u8],
    ) -> i64 {
        let mut s = Store::open(&self.root).unwrap();
        s.save_media(
            &MediaSource {
                sequence: None,
                target: User {
                    id: account,
                    screen_name: screen,
                    display_name: None,
                },
                author: User {
                    id: "author",
                    screen_name: "original_author",
                    display_name: None,
                },
                tweet_id: tweet,
                text: Some("合成文本\n含逗号,和引号\""),
                published_at: date,
                media_key: &format!("key-{tweet}"),
                original_filename: "cdn-original.jpg",
                original_url: "https://example.invalid/media?original=1",
                media_type: kind,
                mime_type: if kind == "image" {
                    "image/jpeg"
                } else {
                    "video/mp4"
                },
            },
            body,
        )
        .unwrap()
        .entity_id
    }
    fn options(&self, mode: Mode) -> Options {
        Options {
            account: Some("a".into()),
            mode,
            media_types: archive::MediaTypes::default(),
            date_start: None,
            date_end: None,
            output_directory: Some(self.output.clone()),
        }
    }
    fn run(&self, o: &Options) -> Progress {
        run(&self.root, &self.output, o, &AtomicBool::new(false), |_| {})
    }
    fn index(&self, p: &Progress) -> Vec<IndexEntry> {
        serde_json::from_slice(
            &fs::read(Path::new(&p.accounts[0].directory).join("index.json")).unwrap(),
        )
        .unwrap()
    }
    fn file(&self, p: &Progress, e: &IndexEntry) -> PathBuf {
        Path::new(&p.accounts[0].directory).join(&e.path)
    }
}
#[test]
fn shared_account_sources_and_unarchived_profiles_export_without_cross_account_leaks() {
    let f = Fixture::new();
    let shared = f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"shared");
    f.add("b", "beta", "2", Some("2026-01-02"), "video", b"video");
    let mut s = Store::open(&f.root).unwrap();
    s.add_account("waiting", &archive::Settings::default())
        .unwrap();
    s.connection().execute_batch("INSERT INTO archive_tweets VALUES('b','1');
        INSERT INTO media_sources(target_user_id,tweet_id,media_key,entity_id,original_filename,original_url,media_type)
        SELECT 'b',tweet_id,'shared-key',entity_id,'beta-shared.jpg',original_url,media_type
        FROM media_sources WHERE target_user_id='a';").unwrap();
    drop(s);
    let mut o = f.options(Mode::Full);
    o.account = None;
    let p = f.run(&o);
    assert_eq!(p.outcome, "completed");
    assert_eq!((p.accounts.len(), p.copied), (2, 3));
    for result in &p.accounts {
        let entries: Vec<IndexEntry> = serde_json::from_slice(
            &fs::read(Path::new(&result.directory).join("index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(entries.len(), if result.account == "a" { 1 } else { 2 });
        let e = entries.iter().find(|e| e.entity_id == shared).unwrap();
        assert_eq!(e.sources.len(), 1);
        assert_eq!(
            e.sources[0].original_filename,
            if result.account == "a" {
                "cdn-original.jpg"
            } else {
                "beta-shared.jpg"
            }
        );
        assert_eq!(
            fs::read(Path::new(&result.directory).join(&e.path)).unwrap(),
            b"shared"
        );
    }
}

#[test]
fn exports_read_a_and_c_layouts_and_keep_detected_extension() {
    let f = Fixture::new();
    f.add(
        "a",
        "alpha",
        "1",
        Some("2026-01-01"),
        "image",
        b"\x89PNG\r\n\x1a\ncontent",
    );
    for layout in [storage::Layout::A, storage::Layout::C] {
        let mut store = Store::open(&f.root).unwrap();
        let result = store.organize_media(layout, &AtomicBool::new(false), |_| {});
        assert!(result.error.is_none(), "{result:?}");
        drop(store);
        let p = f.run(&f.options(Mode::Full));
        assert_eq!(p.outcome, "completed", "{p:?}");
        assert_eq!(p.copied, 1);
        let entries = f.index(&p);
        assert!(entries[0].path.ends_with(".png"));
        assert_eq!(
            fs::read(f.file(&p, &entries[0])).unwrap(),
            b"\x89PNG\r\n\x1a\ncontent"
        );
    }
}

#[test]
fn deleting_exported_media_and_accounts_cleans_manifest_but_keeps_output_files() {
    use crate::storage::{DeleteOptions, DeleteScope};
    let f = Fixture::new();
    let shared = f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"shared");
    f.add("b", "beta", "2", Some("2026-01-02"), "video", b"video");
    let s = Store::open(&f.root).unwrap();
    s.connection().execute_batch("INSERT INTO archive_tweets VALUES('b','1');
        INSERT INTO media_sources(target_user_id,tweet_id,media_key,entity_id,original_filename,original_url,media_type)
        SELECT 'b',tweet_id,'shared-key',entity_id,original_filename,original_url,media_type
        FROM media_sources WHERE target_user_id='a';").unwrap();
    drop(s);
    let mut all = f.options(Mode::Full);
    all.account = None;
    let p = f.run(&all);
    assert_eq!(p.outcome, "completed");
    let snapshots: Vec<_> = p
        .accounts
        .iter()
        .flat_map(|r| {
            let entries: Vec<IndexEntry> = serde_json::from_slice(
                &fs::read(Path::new(&r.directory).join("index.json")).unwrap(),
            )
            .unwrap();
            entries
                .into_iter()
                .map(|e| (Path::new(&r.directory).join(e.path), e.sha256))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut s = Store::open(&f.root).unwrap();
    s.delete_account_data("alpha", DeleteScope::Images, &DeleteOptions::default())
        .unwrap();
    assert_eq!(s.connection().query_row("SELECT count(*) FROM export_files WHERE export_id IN(SELECT export_id FROM export_directories WHERE target_user_id='a')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    drop(s);
    let next = f.run(&f.options(Mode::Incremental));
    assert_eq!(next.outcome, "completed");
    assert!(f.index(&next).is_empty());
    let mut s = Store::open(&f.root).unwrap();
    s.delete_account_data(
        "alpha",
        DeleteScope::All,
        &DeleteOptions {
            confirmation: Some("alpha".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        s.connection()
            .query_row(
                "SELECT target_user_id FROM media_entities WHERE entity_id=?1",
                [shared],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "b"
    );
    s.delete_account_data("beta", DeleteScope::Images, &DeleteOptions::default())
        .unwrap();
    drop(s);
    let mut o = f.options(Mode::Incremental);
    o.account = Some("b".into());
    let next = f.run(&o);
    assert_eq!(next.outcome, "completed");
    assert_eq!(f.index(&next).len(), 1);
    let mut s = Store::open(&f.root).unwrap();
    s.delete_account_data(
        "beta",
        DeleteScope::All,
        &DeleteOptions {
            confirmation: Some("beta".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        s.connection()
            .query_row("SELECT count(*) FROM export_directories", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.connection()
            .query_row("SELECT count(*) FROM export_files", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.connection()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    for (path, hash) in snapshots {
        assert_eq!(storage::hash_file(&path).unwrap(), hash);
    }
}

#[test]
fn full_export_uses_fresh_timestamp_directory_and_verified_files() {
    let f = Fixture::new();
    f.add(
        "a",
        "alpha",
        "12",
        Some("2026-01-02T03:00:00Z"),
        "image",
        b"image",
    );
    let o = f.options(Mode::Full);
    let p = f.run(&o);
    assert_eq!(p.outcome, "completed");
    assert_eq!((p.copied, p.done, p.total), (1, 1, 1));
    let entries = f.index(&p);
    assert_eq!(entries[0].path, "media/2026-01-02_12_1.jpg");
    assert!(
        verified(
            &f.file(&p, &entries[0]),
            entries[0].byte_length,
            &entries[0].sha256
        )
        .unwrap()
    );
    let dir = Path::new(&p.accounts[0].directory);
    assert_eq!(dir.file_name().unwrap().to_string_lossy().len(), 15);
    let before = fs::read(dir.join("index.json")).unwrap();
    let second = f.run(&o);
    assert_ne!(second.accounts[0].directory, p.accounts[0].directory);
    assert_eq!(fs::read(dir.join("index.json")).unwrap(), before);
    let csv = fs::read_to_string(dir.join("index.csv")).unwrap();
    assert!(csv.starts_with("\u{feff}path,entity_id"));
    assert!(csv.contains("original_filename"));
    assert!(csv.contains("\"\"tweet_id\"\""));
}
#[test]
fn incremental_fills_new_entities_and_skips_verified_existing() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    let p = f.run(&f.options(Mode::Full));
    let first = f.index(&p);
    let first_path = f.file(&p, &first[0]);
    let old_modified = first_path.metadata().unwrap().modified().unwrap();
    f.add("a", "alpha", "2", Some("2026-01-02"), "video", b"two");
    let p2 = f.run(&f.options(Mode::Incremental));
    assert_eq!(p.accounts[0].directory, p2.accounts[0].directory);
    assert_eq!((p2.copied, p2.skipped, p2.accounts[0].indexed), (1, 1, 2));
    assert_eq!(
        first_path.metadata().unwrap().modified().unwrap(),
        old_modified
    );
}
#[test]
fn modified_and_deleted_exports_are_preserved_and_removed_from_both_indexes() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    f.add("a", "alpha", "2", Some("2026-01-02"), "image", b"two");
    let p = f.run(&f.options(Mode::Full));
    let entries = f.index(&p);
    let modified = f.file(&p, &entries[0]);
    let deleted = f.file(&p, &entries[1]);
    fs::write(&modified, b"ONE").unwrap();
    fs::remove_file(&deleted).unwrap();
    f.add("a", "alpha", "3", Some("2026-01-03"), "image", b"three");
    let next = f.run(&f.options(Mode::Incremental));
    assert_eq!(next.outcome, "completed_with_issues");
    assert_eq!(next.copied, 1);
    assert_eq!(next.issues.len(), 2);
    assert!(next.issues.iter().any(|i| i.reason == "modified"));
    assert!(next.issues.iter().any(|i| i.reason == "deleted"));
    assert_eq!(fs::read(&modified).unwrap(), b"ONE");
    assert!(!deleted.exists());
    assert_eq!(f.index(&next).len(), 1);
    let csv = fs::read_to_string(Path::new(&next.accounts[0].directory).join("index.csv")).unwrap();
    assert!(!csv.contains(&entries[0].path));
    assert!(!csv.contains(&entries[1].path));
    let again = f.run(&f.options(Mode::Incremental));
    assert_eq!(again.copied, 0);
    assert_eq!(again.issues.len(), 2);
    assert!(!deleted.exists());
}
#[test]
fn filter_changes_keep_old_files_and_all_valid_old_index_entries() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"image");
    f.add("a", "alpha", "2", Some("2026-01-02"), "video", b"video");
    f.add("a", "alpha", "3", Some("2026-01-03"), "gif", b"gif");
    let mut o = f.options(Mode::Full);
    o.media_types = archive::MediaTypes {
        image: true,
        video: false,
        gif: false,
    };
    let p = f.run(&o);
    let image = f.file(&p, &f.index(&p)[0]);
    o.mode = Mode::Incremental;
    o.media_types = archive::MediaTypes {
        image: false,
        video: false,
        gif: true,
    };
    let next = f.run(&o);
    assert_eq!(next.copied, 1);
    assert!(image.is_file());
    assert_eq!(f.index(&next).len(), 2);
    assert!(!f.index(&next).iter().any(|e| e.media_type == "video"));
    fs::remove_file(&image).unwrap();
    let next = f.run(&o);
    assert_eq!(next.issues[0].reason, "deleted");
    assert_eq!(f.index(&next).len(), 1);
}
#[test]
fn shared_entity_names_earliest_source_and_indexes_all_sources_despite_filter() {
    let f = Fixture::new();
    let id = f.add(
        "a",
        "alpha",
        "20",
        Some("Fri Jan 02 01:00:00 +0000 2026"),
        "image",
        b"shared",
    );
    assert_eq!(
        id,
        f.add(
            "a",
            "alpha",
            "10",
            Some("2026-01-01T03:00:00+02:00"),
            "gif",
            b"shared"
        )
    );
    let mut o = f.options(Mode::Full);
    o.date_start = Some("2026-01-02".into());
    o.date_end = o.date_start.clone();
    o.media_types = archive::MediaTypes {
        image: true,
        video: false,
        gif: false,
    };
    let p = f.run(&o);
    assert_eq!(p.copied, 1);
    let e = f.index(&p);
    assert_eq!(e[0].path, "media/2026-01-01_10_1.jpg");
    assert_eq!(e[0].sources.len(), 2);
    assert_eq!(e[0].sources[0].author, "original_author");
    assert_eq!(e[0].sources[0].original_filename, "cdn-original.jpg");
    f.add("a", "alpha", "5", Some("2025-12-01"), "image", b"shared");
    o.mode = Mode::Incremental;
    let next = f.run(&o);
    assert_eq!(next.copied, 0);
    let updated = f.index(&next);
    assert_eq!(updated[0].path, e[0].path);
    assert_eq!(updated[0].sources.len(), 3);
}
#[test]
fn date_filters_use_utc_inclusive_end_and_skip_unknown_dates() {
    let f = Fixture::new();
    f.add(
        "a",
        "alpha",
        "1",
        Some("2026-01-02T01:00:00+02:00"),
        "image",
        b"one",
    );
    f.add(
        "a",
        "alpha",
        "2",
        Some("2026-01-01T23:59:59Z"),
        "image",
        b"two",
    );
    f.add("a", "alpha", "3", None, "image", b"three");
    f.add("a", "alpha", "4", Some("2026-02-30"), "image", b"four");
    let mut o = f.options(Mode::Full);
    o.date_start = Some("2026-01-01".into());
    o.date_end = o.date_start.clone();
    let p = f.run(&o);
    assert_eq!(p.copied, 2);
    assert!(
        f.index(&p)
            .iter()
            .all(|e| e.path.starts_with("media/2026-01-01"))
    );
    o.date_start = Some("bad".into());
    assert!(o.validate().is_err());
    o.date_start = Some("2026-02-02".into());
    assert!(o.validate().is_err());
    o.date_start = None;
    o.date_end = None;
    o.media_types = archive::MediaTypes {
        image: false,
        video: false,
        gif: false,
    };
    assert!(o.validate().is_err());
}
#[test]
fn stop_discards_partial_file_then_incremental_completes_pending_manifest() {
    let f = Fixture::new();
    f.add(
        "a",
        "alpha",
        "1",
        Some("2026-01-01"),
        "image",
        &vec![9u8; 192 * 1024],
    );
    let store = Store::open(&f.root).unwrap();
    let output = validate_output(&f.root, &f.output).unwrap();
    let (id, dir) = directory(&store, &output, "a", "alpha", Mode::Full).unwrap();
    let entities = load_entities(&store, "a").unwrap();
    let file = plan(&store, id, &dir, &entities[0]).unwrap();
    let destination = export_path(&dir, &file.path).unwrap();
    let cancel = AtomicBool::new(false);
    let mut p = Progress::default();
    let completed = copy_verified(
        &storage::media_path(&f.root, &entities[0].stored_path).unwrap(),
        &destination,
        &store,
        id,
        &file,
        &cancel,
        &mut p,
        &mut |p| {
            if p.bytes > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert!(!completed);
    assert!(!destination.exists());
    assert_eq!(fs::read_dir(dir.join("media")).unwrap().count(), 0);
    assert_eq!(manifest(&store, id).unwrap()[0].state, FileState::Pending);
    drop(store);
    let p = f.run(&f.options(Mode::Incremental));
    assert_eq!(p.outcome, "completed");
    assert_eq!(p.copied, 1);
    assert!(destination.is_file());
    assert_eq!(f.index(&p).len(), 1);
}
#[test]
fn stop_event_finishes_status_and_can_restart() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    let cancel = AtomicBool::new(false);
    let mut events = Vec::new();
    let p = run(&f.root, &f.output, &f.options(Mode::Full), &cancel, |p| {
        if p.stage == "导出媒体" {
            cancel.store(true, Ordering::Relaxed);
        }
        events.push(p);
    });
    assert_eq!(p.outcome, "stopped");
    assert!(!p.running);
    assert!(p.finished_at.is_some());
    assert!(events[0].running);
    assert!(!events.last().unwrap().running);
    let next = f.run(&f.options(Mode::Incremental));
    assert_eq!(next.copied, 1);
}
#[test]
fn rename_before_sqlite_marker_is_recovered_but_conflicting_pending_file_is_preserved() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    f.add("a", "alpha", "2", Some("2026-01-02"), "image", b"two");
    let store = Store::open(&f.root).unwrap();
    let out = validate_output(&f.root, &f.output).unwrap();
    let (id, dir) = directory(&store, &out, "a", "alpha", Mode::Full).unwrap();
    let entities = load_entities(&store, "a").unwrap();
    let one = plan(&store, id, &dir, &entities[0]).unwrap();
    let two = plan(&store, id, &dir, &entities[1]).unwrap();
    store
        .connection()
        .execute(
            "UPDATE export_files SET state='publishing' WHERE export_id=?1 AND entity_id=?2",
            params![id, one.entity_id],
        )
        .unwrap();
    fs::write(export_path(&dir, &one.path).unwrap(), b"one").unwrap();
    fs::write(export_path(&dir, &two.path).unwrap(), b"USER").unwrap();
    drop(store);
    let p = f.run(&f.options(Mode::Incremental));
    assert_eq!(p.copied, 0);
    assert_eq!(p.issues.len(), 1);
    assert_eq!(p.issues[0].reason, "conflict");
    assert_eq!(f.index(&p).len(), 1);
    let store = Store::open(&f.root).unwrap();
    assert!(
        manifest(&store, id)
            .unwrap()
            .iter()
            .find(|f| f.entity_id == one.entity_id)
            .unwrap()
            .state
            == FileState::Written
    );
    assert_eq!(
        fs::read(export_path(&dir, &two.path).unwrap()).unwrap(),
        b"USER"
    );
}

#[test]
fn publishing_crash_then_user_deletion_is_never_restored() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    let store = Store::open(&f.root).unwrap();
    let out = validate_output(&f.root, &f.output).unwrap();
    let (id, dir) = directory(&store, &out, "a", "alpha", Mode::Full).unwrap();
    let entities = load_entities(&store, "a").unwrap();
    let file = plan(&store, id, &dir, &entities[0]).unwrap();
    // Simulate the durable publication intent, completed rename, then process death
    // before marking written. The user subsequently deletes the visible file.
    store
        .connection()
        .execute(
            "UPDATE export_files SET state='publishing' WHERE export_id=?1",
            [id],
        )
        .unwrap();
    let destination = export_path(&dir, &file.path).unwrap();
    fs::write(&destination, b"one").unwrap();
    fs::remove_file(&destination).unwrap();
    drop(store);
    let p = f.run(&f.options(Mode::Incremental));
    assert_eq!(p.copied, 0);
    assert_eq!(p.issues.len(), 1);
    assert_eq!(p.issues[0].reason, "uncertain");
    assert!(!destination.exists());
    assert!(f.index(&p).is_empty());
    let next = f.run(&f.options(Mode::Full));
    assert_eq!(next.copied, 1);
    assert!(!destination.exists());
}

#[test]
fn verification_is_interruptible_and_stopped_index_can_be_completed_next_time() {
    let f = Fixture::new();
    f.add(
        "a",
        "alpha",
        "1",
        Some("2026-01-01"),
        "image",
        &vec![8u8; 192 * 1024],
    );
    let p = f.run(&f.options(Mode::Full));
    let entries = f.index(&p);
    let path = f.file(&p, &entries[0]);
    let directory = Path::new(&p.accounts[0].directory);
    let prior_json = fs::read(directory.join("index.json")).unwrap();
    let prior_csv = fs::read(directory.join("index.csv")).unwrap();
    let cancel = AtomicBool::new(false);
    let mut progress = Progress::default();
    let result = verify_file(
        &path,
        entries[0].byte_length,
        &entries[0].sha256,
        &cancel,
        &mut progress,
        &mut |p| {
            if p.checked_bytes > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    )
    .unwrap();
    assert_eq!(result, None);
    assert_eq!(progress.checked_bytes, 64 * 1024);
    cancel.store(false, Ordering::Relaxed);
    let stopped = run(
        &f.root,
        &f.output,
        &f.options(Mode::Incremental),
        &cancel,
        |p| {
            if p.stage == "核对并写入索引" {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(stopped.outcome, "stopped");
    assert_eq!(f.index(&stopped).len(), 1);
    assert_eq!(fs::read(directory.join("index.json")).unwrap(), prior_json);
    assert_eq!(fs::read(directory.join("index.csv")).unwrap(), prior_csv);
    assert!(path.is_file());
    let next = f.run(&f.options(Mode::Incremental));
    assert_eq!(next.copied, 0);
    assert_eq!(f.index(&next).len(), 1);
}
#[test]
fn missing_or_corrupt_archive_media_never_enters_indexes() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    f.add("a", "alpha", "2", Some("2026-01-02"), "image", b"two");
    let store = Store::open(&f.root).unwrap();
    let e = load_entities(&store, "a").unwrap();
    fs::remove_file(storage::media_path(&f.root, &e[0].stored_path).unwrap()).unwrap();
    fs::write(
        storage::media_path(&f.root, &e[1].stored_path).unwrap(),
        b"TWO",
    )
    .unwrap();
    drop(store);
    let p = f.run(&f.options(Mode::Full));
    assert_eq!(p.copied, 0);
    assert!(f.index(&p).is_empty());
    assert!(p.issues.iter().any(|i| i.reason == "source_missing"));
    assert!(p.issues.iter().any(|i| i.reason == "source_changed"));
    assert_eq!(
        fs::read_dir(Path::new(&p.accounts[0].directory).join("media"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
fn output_inside_data_and_traversal_and_ads_are_rejected() {
    let f = Fixture::new();
    assert!(validate_output(&f.root, &f.root).is_err());
    let nested = f.root.join("exports");
    fs::create_dir_all(&nested).unwrap();
    assert!(validate_output(&f.root, &nested).is_err());
    assert!(validate_directory(&f.root, &f.root.join("not-yet-created")).is_err());
    for path in [
        "../outside",
        "media/../bad",
        "media\\bad",
        "media/a:stream",
        "media/a?.jpg",
        "media/a..jpg",
        "media/bad/extra",
    ] {
        assert!(export_path(&f.output, path).is_err(), "{path}");
    }
    assert!(validate_output(&f.root, Path::new("relative")).is_err());
    assert!(validate_output(&f.root, &f.output.join("../exports")).is_err());
}
#[test]
fn windows_illegal_names_and_case_collisions_are_cleaned_without_losing_accounts() {
    for (raw, expected) in [
        ("CON", "_CON"),
        ("AUX.jpg", "_AUX.jpg"),
        ("LPT¹", "_LPT¹"),
        ("a<>:\"/\\|?*b", "a_________b"),
        (".. ", "_"),
        (".", "unnamed"),
    ] {
        assert_eq!(paths::clean_filename(raw), expected);
    }
    let f = Fixture::new();
    f.add("a", "CON", "../../x:?", Some("2026-01-01"), "image", b"one");
    f.add("b", "con", "2", Some("2026-01-01"), "image", b"two");
    let mut o = f.options(Mode::Full);
    o.account = None;
    let p = f.run(&o);
    assert_eq!(p.accounts.len(), 2);
    assert!(p.accounts.iter().all(|a| !a.directory.starts_with(r"\\?\")));
    let store = Store::open(&f.root).unwrap();
    let records: Vec<(String, String)> = store
        .connection()
        .prepare("SELECT output_root,directory FROM export_directories")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        records
            .iter()
            .all(|(root, dir)| !root.starts_with(r"\\?\") && !dir.starts_with(r"\\?\"))
    );
    assert_ne!(
        p.accounts[0].directory.to_lowercase(),
        p.accounts[1].directory.to_lowercase()
    );
    assert!(p.accounts.iter().all(|a| {
        Path::new(&a.directory)
            .canonicalize()
            .unwrap()
            .starts_with(f.output.canonicalize().unwrap())
    }));
    assert_eq!(p.copied, 2);
    assert!(p.issues.is_empty());
}
#[test]
fn account_subdirectory_cannot_enter_data_even_when_output_is_ancestor() {
    let f = Fixture::new();
    f.add("a", "data", "1", Some("2026-01-01"), "image", b"one");
    let output = f.root.parent().unwrap();
    let p = run(
        &f.root,
        output,
        &f.options(Mode::Full),
        &AtomicBool::new(false),
        |_| {},
    );
    assert_eq!(p.outcome, "incomplete");
    assert!(p.error.unwrap().contains("数据目录内部"));
}
#[test]
fn deleted_export_directory_is_not_recreated_and_output_changes_require_full() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    let p = f.run(&f.options(Mode::Full));
    let dir = Path::new(&p.accounts[0].directory);
    let resolved = dir.canonicalize().unwrap();
    assert!(resolved.starts_with(f.output.canonicalize().unwrap()));
    fs::remove_dir_all(&resolved).unwrap();
    let next = f.run(&f.options(Mode::Incremental));
    assert_eq!(next.outcome, "incomplete");
    assert_eq!(next.issues[0].reason, "deleted");
    assert!(!dir.exists());
    let new = f.output.join("new");
    fs::create_dir(&new).unwrap();
    let o = f.options(Mode::Incremental);
    let next = run(&f.root, &new, &o, &AtomicBool::new(false), |_| {});
    assert!(next.error.unwrap().contains("上次导出不同"));
}
#[test]
fn schema_v4_migration_backs_up_and_rolls_back_failed_step() {
    let d = fixture_directory();
    let root = d.path().join("data");
    fs::create_dir(&root).unwrap();
    let db = root.join("archive.sqlite3");
    let c = Connection::open(&db).unwrap();
    c.execute_batch(include_str!("../storage/schema.sql"))
        .unwrap();
    c.execute_batch(include_str!("../storage/archive-schema.sql"))
        .unwrap();
    c.execute_batch(include_str!("../storage/runs-schema.sql"))
        .unwrap();
    c.execute_batch(include_str!("../storage/workspace-schema.sql"))
        .unwrap();
    c.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO users(user_id,screen_name) VALUES('keep','kept'); CREATE TABLE export_files(existing TEXT);").unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='export_directories'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let backup = fs::read_dir(root.join("temporary"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let b = Connection::open(backup).unwrap();
    assert_eq!(
        b.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        b.query_row(
            "SELECT screen_name FROM users WHERE user_id='keep'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "kept"
    );
    drop(b);
    c.execute_batch("DROP TABLE export_files").unwrap();
    drop(c);
    let s = Store::open(&root).unwrap();
    assert_eq!(
        s.connection()
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
    assert_eq!(
        s.connection()
            .query_row("SELECT count(*) FROM export_files", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn shared_storage_lock_excludes_export_archive_import_and_delete() {
    let f = Fixture::new();
    f.add("a", "alpha", "1", Some("2026-01-01"), "image", b"one");
    let access = crate::StorageAccess(std::sync::Mutex::new(()));
    let guard = access.0.try_lock().unwrap();
    let called = std::cell::Cell::new(false);
    let blocked = commands::with_storage_lock(&access, || {
        called.set(true);
        Ok(f.run(&f.options(Mode::Full)))
    });
    assert!(blocked.is_err());
    assert!(!called.get());
    assert_eq!(fs::read_dir(&f.output).unwrap().count(), 0);
    drop(guard);
    let completed = commands::with_storage_lock(&access, || {
        Ok(run(
            &f.root,
            &f.output,
            &f.options(Mode::Full),
            &AtomicBool::new(false),
            |p| {
                if p.running {
                    assert!(access.0.try_lock().is_err());
                    assert!(std::thread::scope(|scope| scope
                        .spawn(|| access.0.try_lock().is_err())
                        .join()
                        .unwrap()));
                }
            },
        ))
    })
    .unwrap();
    assert_eq!(completed.copied, 1);
    assert!(access.0.try_lock().is_ok());
}
#[cfg(windows)]
#[test]
fn windows_junction_output_and_media_paths_are_rejected() {
    let f = Fixture::new();
    let junction = f.output.join("junction");
    crate::test_support::junction(&junction, &f.root).unwrap();
    assert!(validate_output(&f.root, &junction).is_err());
    assert!(validate_directory(&f.root, &junction.join("nested")).is_err());
    assert!(export_path(&junction, "media/file.jpg").is_err());
    fs::remove_dir(&junction).unwrap();
}

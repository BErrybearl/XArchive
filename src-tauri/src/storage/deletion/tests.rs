use super::*;
use crate::archive::{MediaTypes, Settings};

fn fixture() -> (tempfile::TempDir, Store) {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/workspace-tests");
    fs::create_dir_all(&base).unwrap();
    let d = tempfile::tempdir_in(base).unwrap();
    let s = Store::open(d.path()).unwrap();
    (d, s)
}
fn user(id: &str) -> User<'_> {
    User {
        id,
        screen_name: id,
        display_name: None,
    }
}
fn media(s: &mut Store, account: &str, tweet: &str, kind: &str) -> i64 {
    s.save_media(
        &MediaSource {
            sequence: None,
            target: user(account),
            author: user("author"),
            tweet_id: tweet,
            text: Some("合成数据"),
            published_at: None,
            media_key: kind,
            original_filename: "test",
            original_url: "https://example.invalid/test",
            media_type: kind,
            mime_type: "application/octet-stream",
        },
        kind.as_bytes(),
    )
    .unwrap()
    .entity_id
}
fn path(s: &Store, id: i64) -> PathBuf {
    let relative: String = s
        .connection
        .query_row(
            "SELECT relative_path FROM media_entities WHERE entity_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    media_path(&s.root, &relative).unwrap()
}
fn all() -> DeleteOptions {
    DeleteOptions {
        confirmation: Some("a".into()),
        disable_type: false,
        preview_token: None,
    }
}
fn share(s: &mut Store, id: i64) {
    s.save_tweet(&user("b"), &user("author"), "photo", None, None)
        .unwrap();
    s.connection.execute("INSERT INTO media_sources(target_user_id,tweet_id,media_key,entity_id,original_filename,original_url,media_type) VALUES ('b','photo','image',?1,'x','https://example.invalid/x','image')",[id]).unwrap();
}
#[test]
fn shared_file_survives_type_and_owner_removal() {
    let (_d, mut s) = fixture();
    let entity = media(&mut s, "a", "photo", "image");
    let file = path(&s, entity);
    share(&mut s, entity);
    let p = s.preview_account_delete("a", DeleteScope::Images).unwrap();
    assert_eq!((p.sources, p.files, p.retained_files), (1, 0, 1));
    s.delete_account_data("a", DeleteScope::Images, &DeleteOptions::default())
        .unwrap();
    assert!(file.exists());
    assert_eq!(status(&s.root).unwrap().sources, 1);
    s.delete_account_data("a", DeleteScope::All, &all())
        .unwrap();
    assert!(file.exists());
    assert_eq!(
        s.connection
            .query_row("SELECT target_user_id FROM media_entities", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "b"
    );
    assert_eq!(
        s.connection
            .query_row("SELECT count(*) FROM tweets", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        s.connection
            .query_row("SELECT 1 FROM users WHERE user_id='a'", [], |r| r
                .get::<_, i64>(0))
            .optional()
            .unwrap()
            .is_none()
    );
    s.connection
        .execute_batch("PRAGMA foreign_key_check")
        .unwrap();
}
#[test]
fn single_type_preserves_other_types_metadata_and_disables_option() {
    let (_d, mut s) = fixture();
    let i = media(&mut s, "a", "photo", "image");
    let v = media(&mut s, "a", "video", "video");
    let g = media(&mut s, "a", "gif", "gif");
    let image = path(&s, i);
    let video = path(&s, v);
    let gif = path(&s, g);
    s.add_account("a", &Settings::default()).unwrap();
    s.connection
        .execute(
            "INSERT INTO archive_checkpoints VALUES ('a','full:UserMedia:all','cursor',1,NULL,1,1)",
            [],
        )
        .unwrap();
    s.connection
        .execute(
            "INSERT INTO archive_runs VALUES ('a',1,NULL,'all:timeline0','done','{}')",
            [],
        )
        .unwrap();
    let p = s.preview_account_delete("a", DeleteScope::Images).unwrap();
    assert_eq!(
        (p.sources, p.tweet_metadata, p.files, p.bytes),
        (1, 0, 1, 5)
    );
    assert!(image.exists());
    let result = s
        .delete_account_data(
            "a",
            DeleteScope::Images,
            &DeleteOptions {
                disable_type: true,
                confirmation: None,
                preview_token: None,
            },
        )
        .unwrap();
    assert!(result.orphaned_files.is_empty());
    assert!(!image.exists());
    assert!(video.exists() && gif.exists());
    assert_eq!(status(&s.root).unwrap().tweets, 3);
    let settings = s.account_settings("a", &Settings::default()).unwrap();
    assert_eq!(
        settings.preferences.media_types,
        Some(MediaTypes {
            image: false,
            video: true,
            gif: true
        })
    );
    assert_eq!(
        count(&s.connection, "SELECT count(*) FROM archive_checkpoints").unwrap(),
        0
    );
    assert!(
        s.connection
            .query_row("SELECT full_scope FROM archive_runs", [], |r| r
                .get::<_, Option<String>>(0))
            .unwrap()
            .is_none()
    );
}
#[test]
fn text_delete_preserves_media_and_other_account_tweet() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    for tweet in ["text", "shared"] {
        s.save_tweet(&user("a"), &user("author"), tweet, None, None)
            .unwrap();
    }
    s.save_tweet(&user("b"), &user("author"), "shared", None, None)
        .unwrap();
    let p = s
        .preview_account_delete("a", DeleteScope::TextTweets)
        .unwrap();
    assert_eq!(
        (p.tweet_links, p.tweet_metadata, p.sources, p.files),
        (2, 1, 0, 0)
    );
    s.delete_account_data("a", DeleteScope::TextTweets, &DeleteOptions::default())
        .unwrap();
    assert_eq!(status(&s.root).unwrap().tweets, 2);
    assert_eq!(status(&s.root).unwrap().sources, 1);
    assert_eq!(
        count(
            &s.connection,
            "SELECT count(*) FROM archive_tweets WHERE target_user_id='a'"
        )
        .unwrap(),
        1
    );
}
#[test]
fn all_clears_account_records_and_keeps_other_account() {
    let (_d, mut s) = fixture();
    let i = media(&mut s, "a", "photo", "image");
    let file = path(&s, i);
    media(&mut s, "b", "video", "video");
    s.add_account("a", &Settings::default()).unwrap();
    s.connection.execute_batch("INSERT INTO archive_checkpoints VALUES ('a','full:UserMedia:all',NULL,1,NULL,1,1); INSERT INTO archive_runs VALUES ('a',1,2,'all','done',NULL); INSERT INTO archive_retries VALUES ('a','photo','retry','image','{}',1,'test','pending');").unwrap();
    assert!(
        s.delete_account_data("a", DeleteScope::All, &DeleteOptions::default())
            .is_err()
    );
    assert!(file.exists());
    s.delete_account_data("a", DeleteScope::All, &all())
        .unwrap();
    assert!(!file.exists());
    for table in [
        "media_sources",
        "media_entities",
        "archive_tweets",
        "archives",
        "archive_retries",
        "archive_checkpoints",
        "archive_runs",
        "account_profiles",
    ] {
        assert_eq!(
            count(
                &s.connection,
                &format!(
                    "SELECT count(*) FROM {table} WHERE {}='a'",
                    if table == "account_profiles" {
                        "screen_name"
                    } else {
                        "target_user_id"
                    }
                )
            )
            .unwrap(),
            0
        );
    }
    assert_eq!(status(&s.root).unwrap().media, 1);
    assert_eq!(list_archived_accounts(&s.root).unwrap().len(), 1);
}
#[test]
fn backup_is_before_delete_and_keeps_five_without_touching_other_files() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    let result = s
        .delete_account_data("a", DeleteScope::Images, &DeleteOptions::default())
        .unwrap();
    let backup = Connection::open(&result.backup).unwrap();
    assert_eq!(
        count(&backup, "SELECT count(*) FROM media_sources").unwrap(),
        1
    );
    drop(backup);
    let sentinel = s.root.join("backups/keep.txt");
    fs::write(&sentinel, b"keep").unwrap();
    for _ in 0..6 {
        s.backup_index().unwrap();
    }
    assert_eq!(
        fs::read_dir(s.root.join("backups"))
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "sqlite3"))
            .count(),
        5
    );
    assert!(sentinel.exists());
}
#[test]
fn failed_file_delete_commits_index_and_reports_orphan() {
    let (_d, mut s) = fixture();
    let id = media(&mut s, "a", "photo", "image");
    let file = path(&s, id);
    let result = s
        .delete_account_with("a", DeleteScope::Images, &DeleteOptions::default(), |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "synthetic failure",
            ))
        })
        .unwrap();
    assert_eq!(result.orphaned_files.len(), 1);
    assert!(file.exists());
    assert_eq!(status(&s.root).unwrap().media, 0);
    assert_eq!(s.inspect().unwrap().untracked_files, result.orphaned_files);
}
#[test]
fn escaped_index_path_refuses_preview_and_execution_before_backup() {
    let (_d, mut s) = fixture();
    let id = media(&mut s, "a", "photo", "image");
    let file = path(&s, id);
    s.connection
        .execute_batch("PRAGMA ignore_check_constraints=ON;")
        .unwrap();
    s.connection
        .execute("UPDATE media_entities SET relative_path='../outside'", [])
        .unwrap();
    assert!(s.preview_account_delete("a", DeleteScope::Images).is_err());
    assert!(
        s.delete_account_data("a", DeleteScope::Images, &DeleteOptions::default())
            .is_err()
    );
    assert!(file.exists());
    assert_eq!(status(&s.root).unwrap().media, 1);
    assert!(!s.root.join("backups").exists());
}
#[test]
fn account_registration_deduplicates_links_inherits_and_remembers() {
    let (_d, mut s) = fixture();
    let a = s
        .add_account("https://x.com/A/", &Settings::default())
        .unwrap();
    assert_eq!(a.target, "a");
    let mut a = a;
    a.include_timeline = true;
    a.preferences.concurrency = 3;
    s.save_account_settings(&a).unwrap();
    let b = s.add_account("@B", &Settings::default()).unwrap();
    assert_eq!(b.preferences.concurrency, 3);
    assert!(b.include_timeline);
    let mut b = b;
    b.preferences.concurrency = 1;
    s.save_account_settings(&b).unwrap();
    assert_eq!(
        s.account_settings("a", &Settings::default())
            .unwrap()
            .preferences
            .concurrency,
        3
    );
    s.add_account("https://twitter.com/a", &Settings::default())
        .unwrap();
    assert_eq!(list_archived_accounts(&s.root).unwrap().len(), 2);
    assert_eq!(
        s.workspace_settings(&Settings::default()).unwrap().target,
        "a"
    );
    assert_eq!(status(&s.root).unwrap().tweets, 0);
    assert!(
        s.add_account("https://example.invalid/a", &Settings::default())
            .is_err()
    );
}
#[test]
fn account_integrity_is_read_only_and_does_not_check_other_accounts() {
    let (_d, mut s) = fixture();
    let a = media(&mut s, "a", "photo", "image");
    let b = media(&mut s, "b", "video", "video");
    let pa = path(&s, a);
    let pb = path(&s, b);
    fs::write(&pa, b"change").unwrap();
    fs::remove_file(&pb).unwrap();
    let report = s.inspect_account("a").unwrap();
    assert_eq!(report.changed_files.len(), 1);
    assert!(report.missing_files.is_empty());
    assert_eq!(fs::read(&pa).unwrap(), b"change");
    assert_eq!(status(&s.root).unwrap().media, 2);
    assert_eq!(s.inspect_account("b").unwrap().missing_files.len(), 1);
}
#[test]
fn registration_without_archive_can_be_removed() {
    let (_d, mut s) = fixture();
    s.add_account("a", &Settings::default()).unwrap();
    let p = s.preview_account_delete("a", DeleteScope::All).unwrap();
    assert_eq!((p.sources, p.files, p.tweet_metadata), (0, 0, 0));
    s.delete_account_data("a", DeleteScope::All, &all())
        .unwrap();
    assert!(list_archived_accounts(&s.root).unwrap().is_empty());
}

#[test]
fn preview_token_refuses_changed_data_or_directory_without_mutating() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    let p = s.preview_account_delete("a", DeleteScope::Images).unwrap();
    media(&mut s, "a", "photo2", "image");
    let options = DeleteOptions {
        preview_token: Some(p.token.clone()),
        ..Default::default()
    };
    assert!(
        s.delete_account_data("a", DeleteScope::Images, &options)
            .is_err()
    );
    assert_eq!(status(&s.root).unwrap().sources, 2);
    assert!(!s.root.join("backups").exists());
    let (_other, mut other) = fixture();
    media(&mut other, "a", "photo", "image");
    assert!(
        other
            .delete_account_data("a", DeleteScope::Images, &options)
            .is_err()
    );
    let p = s.preview_account_delete("a", DeleteScope::Images).unwrap();
    assert!(
        s.delete_account_data(
            "a",
            DeleteScope::Images,
            &DeleteOptions {
                preview_token: Some(p.token),
                ..Default::default()
            }
        )
        .is_ok()
    );
}
#[test]
fn current_backup_survives_clock_rollback_and_future_named_backups() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    let dir = s.root.join("backups");
    fs::create_dir_all(&dir).unwrap();
    for second in 0..5 {
        fs::write(
            dir.join(format!("index-99991231-2359{second:02}.sqlite3")),
            b"synthetic older backup",
        )
        .unwrap();
    }
    let result = s
        .delete_account_data("a", DeleteScope::Images, &DeleteOptions::default())
        .unwrap();
    assert!(Path::new(&result.backup).exists());
    assert_eq!(fs::read_dir(dir).unwrap().count(), 5);
    let c = Connection::open(result.backup).unwrap();
    assert_eq!(count(&c, "SELECT count(*) FROM media_sources").unwrap(), 1);
}

#[test]
fn renamed_account_keeps_identity_options_and_media_without_duplicate_registration() {
    let (_d, mut s) = fixture();
    let entity = media(&mut s, "a", "photo", "image");
    let file = path(&s, entity);
    let mut settings = s.add_account("a", &Settings::default()).unwrap();
    settings.preferences.concurrency = 3;
    settings.include_timeline = true;
    s.save_account_settings(&settings).unwrap();
    s.bind_account(
        &crate::auth::Account {
            user_id: "a".into(),
            screen_name: "renamed".into(),
            display_name: "新名称".into(),
        },
        &settings,
    )
    .unwrap();
    let rows = list_archived_accounts(&s.root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].user_id.as_str(),
            rows[0].screen_name.as_str(),
            rows[0].media
        ),
        ("a", "renamed", 1)
    );
    let saved = s.account_settings("renamed", &Settings::default()).unwrap();
    assert_eq!(saved.preferences.concurrency, 3);
    assert!(saved.include_timeline);
    assert_eq!(saved.target, "renamed");
    assert_eq!(
        s.workspace_settings(&Settings::default()).unwrap().target,
        "renamed"
    );
    assert!(file.exists());
    assert!(s.account_id("a").is_err());
    assert_eq!(s.account_id("renamed").unwrap().as_deref(), Some("a"));
}

#[test]
fn renamed_metadata_reuses_old_options_and_merges_unbound_registration() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    let mut original = s.add_account("a", &Settings::default()).unwrap();
    original.preferences.concurrency = 3;
    s.save_account_settings(&original).unwrap();
    // Another timeline may reveal the new username before this account is selected.
    s.connection
        .execute(
            "UPDATE users SET screen_name='renamed' WHERE user_id='a'",
            [],
        )
        .unwrap();
    assert_eq!(list_archived_accounts(&s.root).unwrap().len(), 1);
    let inherited = s.add_account("renamed", &Settings::default()).unwrap();
    assert_eq!(inherited.preferences.concurrency, 3);
    s.connection
        .execute(
            "UPDATE account_profiles SET screen_name='a' WHERE target_user_id='a'",
            [],
        )
        .unwrap();
    s.connection
        .execute(
            "INSERT INTO account_profiles VALUES ('renamed',NULL,?1)",
            [serde_json::to_string(&inherited).unwrap()],
        )
        .unwrap();
    s.bind_account(
        &crate::auth::Account {
            user_id: "a".into(),
            screen_name: "renamed".into(),
            display_name: String::new(),
        },
        &inherited,
    )
    .unwrap();
    assert_eq!(list_archived_accounts(&s.root).unwrap().len(), 1);
    assert_eq!(
        count(&s.connection, "SELECT count(*) FROM account_profiles").unwrap(),
        1
    );
    assert_eq!(
        s.account_settings("renamed", &Settings::default())
            .unwrap()
            .preferences
            .concurrency,
        3
    );
}

#[test]
fn changed_username_identity_cannot_overwrite_another_registration() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    s.add_account("a", &Settings::default()).unwrap();
    media(&mut s, "b", "video", "video");
    s.add_account("b", &Settings::default()).unwrap();
    assert!(
        s.bind_account(
            &crate::auth::Account {
                user_id: "b".into(),
                screen_name: "a".into(),
                display_name: String::new()
            },
            &Settings::default()
        )
        .is_err()
    );
    assert_eq!(s.account_id("a").unwrap().as_deref(), Some("a"));
    assert_eq!(s.account_id("b").unwrap().as_deref(), Some("b"));
    assert_eq!(status(&s.root).unwrap().media, 2);
}

#[test]
fn reused_username_cannot_read_delete_or_reset_another_account() {
    let (_d, mut s) = fixture();
    media(&mut s, "a", "photo", "image");
    s.add_account("a", &Settings::default()).unwrap();
    media(&mut s, "b", "video", "video");
    s.add_account("b", &Settings::default()).unwrap();
    s.connection.execute_batch("INSERT INTO archive_checkpoints VALUES ('a','full:UserMedia:all','a-cursor',0,NULL,NULL,NULL),('b','full:UserMedia:all','b-cursor',0,NULL,NULL,NULL); UPDATE users SET screen_name='c' WHERE user_id='b'; UPDATE users SET screen_name='b' WHERE user_id='a';").unwrap();
    assert!(s.account_settings("b", &Settings::default()).is_err());
    assert!(s.account_data("b").is_err());
    assert!(s.inspect_account("b").is_err());
    assert!(s.reset_account_progress("b").is_err());
    assert!(s.preview_account_delete("b", DeleteScope::All).is_err());
    assert!(
        s.delete_account_data(
            "b",
            DeleteScope::All,
            &DeleteOptions {
                confirmation: Some("b".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert_eq!(status(&s.root).unwrap().media, 2);
    assert_eq!(
        count(&s.connection, "SELECT count(*) FROM archive_checkpoints").unwrap(),
        2
    );
    assert!(!s.root.join("backups").exists());
    // The current canonical name still resolves its own stable identity.
    assert_eq!(s.account_data("c").unwrap()[1].sources, 1);
}

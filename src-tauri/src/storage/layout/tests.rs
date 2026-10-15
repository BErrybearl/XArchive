use super::*;
use crate::storage::tests::{isolated_directory, migration_fixture, sample};

fn path(store: &Store, id: i64) -> String {
    store
        .connection
        .query_row(
            "SELECT relative_path FROM media_entities WHERE entity_id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
}
fn organize(store: &mut Store, layout: Layout) -> LayoutProgress {
    let p = store.organize_media(layout, &AtomicBool::new(false), |_| {});
    assert!(p.error.is_none(), "{p:?}");
    p
}
fn intact(store: &Store) {
    let report = store.inspect().unwrap();
    assert!(
        report.missing_files.is_empty() && report.changed_files.is_empty(),
        "{report:?}"
    );
}
#[test]
fn names_types_magic_unknown_and_windows_safety() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    assert_eq!(status(d.path()).unwrap().layout, Layout::C);
    let mut source = sample("bad:/?*tweet", "author");
    source.target.screen_name = "CON";
    source.sequence = Some(3);
    let id = s
        .save_media(&source, &b"\x89PNG\r\n\x1a\nsample"[..])
        .unwrap()
        .entity_id;
    let relative = path(&s, id);
    assert!(relative.starts_with("media/@_CON/图片/"), "{relative}");
    assert!(relative.ends_with("_bad____tweet_3.png"), "{relative}");
    assert_eq!(clean(" NUL.txt. ", 40), "_NUL.txt");
    assert_eq!(clean("COM1", 40), "_COM1");
    assert_eq!(clean("a..b\\c:d", 40), "a_b_c_d");
    assert!(clean(&"长".repeat(200), 40).encode_utf16().count() <= 40);
    for (kind, body, folder, ext) in [
        ("video", &b"\0\0\0\x18ftypisomxx"[..], "视频", "mp4"),
        ("gif", &b"GIF89axx"[..], "GIF", "gif"),
        ("image", &b"RIFFxxxxWEBPxx"[..], "图片", "webp"),
    ] {
        source.tweet_id = kind;
        source.media_type = kind;
        let id = s.save_media(&source, body).unwrap().entity_id;
        assert!(path(&s, id).contains(&format!("/{folder}/")));
        assert!(path(&s, id).ends_with(&format!(".{ext}")));
    }
    source.tweet_id = "unknown";
    source.original_url = "https://example.invalid/noext";
    source.mime_type = "application/octet-stream";
    let id = s
        .save_media(&source, &b"unrecognized"[..])
        .unwrap()
        .entity_id;
    assert!(path(&s, id).ends_with(".bin"));
    source.tweet_id = "url";
    source.original_url = "https://example.invalid/file.jpeg?name=orig";
    let id = s
        .save_media(&source, &b"url fallback"[..])
        .unwrap()
        .entity_id;
    assert!(path(&s, id).ends_with(".jpg"));
    source.tweet_id = "format";
    source.original_url = "https://example.invalid/file?format=webp&name=orig";
    let id = s
        .save_media(&source, &b"query fallback"[..])
        .unwrap()
        .entity_id;
    assert!(path(&s, id).ends_with(".webp"));
    source.tweet_id = "mime";
    source.original_url = "https://example.invalid/noext";
    source.mime_type = "image/png";
    let id = s
        .save_media(&source, &b"mime fallback"[..])
        .unwrap()
        .entity_id;
    assert!(path(&s, id).ends_with(".png"));
    source.tweet_id = "avif";
    source.mime_type = "image/avif";
    let id = s
        .save_media(&source, &b"\0\0\0\x18ftypavifxx"[..])
        .unwrap()
        .entity_id;
    assert!(path(&s, id).ends_with(".avif"));
    intact(&s);
    for bad in [
        "media/../outside",
        "media/a/./b",
        "media/a//b",
        "media/a/b:ads",
        "media/a\\b",
        "media/a/b.",
    ] {
        assert!(media_path(d.path(), bad).is_err(), "{bad}");
    }
    assert!(media_path(d.path(), "media/@target/图片/file.jpg").is_ok());
    assert!(
        build_path(
            &d.path().join("x".repeat(240)),
            &s.connection,
            Layout::C,
            "a",
            None,
            "1",
            1,
            "image",
            "jpg"
        )
        .is_err()
    );
}
#[test]
fn collisions_preserve_user_files_and_repeat_without_name_churn() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    let source = sample("1", "a");
    let desired = build_path(
        &s.root,
        &s.connection,
        Layout::C,
        "target",
        source.published_at,
        "1",
        1,
        "image",
        "jpg",
    )
    .unwrap();
    let user = media_path(&s.root, &desired).unwrap();
    fs::create_dir_all(user.parent().unwrap()).unwrap();
    fs::write(&user, b"user").unwrap();
    let id = s.save_media(&source, &b"download"[..]).unwrap().entity_id;
    assert_eq!(path(&s, id), desired.replace(".jpg", " (2).jpg"));
    assert_eq!(organize(&mut s, Layout::C).renamed, 0);
    assert_eq!(fs::read(user).unwrap(), b"user");
    assert_eq!(s.inspect().unwrap().untracked_files, vec![desired.clone()]);
    assert_eq!(
        s.inspect_account("target").unwrap().untracked_files,
        vec![desired]
    );
}
#[cfg(windows)]
#[test]
fn windows_directory_case_does_not_make_tracked_media_orphans() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    fs::create_dir_all(s.root.join("media/@TARGET/图片")).unwrap();
    s.save_media(&sample("1", "a"), &b"one"[..]).unwrap();
    let report = s.inspect().unwrap();
    assert!(report.untracked_files.is_empty(), "{report:?}");
    let report = s.inspect_account("target").unwrap();
    assert!(report.untracked_files.is_empty(), "{report:?}");
}
#[cfg(windows)]
#[test]
fn missing_indexed_paths_still_reserve_windows_case_aliases() {
    for (first, second) in [("ABC", "abc"), ("Ä", "ä")] {
        let d = isolated_directory();
        let mut s = Store::open(d.path()).unwrap();
        let id = s
            .save_media(&sample(first, "a"), &b"one"[..])
            .unwrap()
            .entity_id;
        let old = path(&s, id);
        fs::remove_file(s.root.join(&old)).unwrap();
        let next = s
            .save_media(&sample(second, "a"), &b"two"[..])
            .unwrap()
            .entity_id;
        assert!(
            !s.root.join(&old).exists(),
            "new entity reused a missing entity's path"
        );
        assert!(path(&s, next).ends_with(" (2).jpg"));
        let report = s.inspect().unwrap();
        assert_eq!(report.missing_files, vec![old]);
        assert!(report.changed_files.is_empty() && report.untracked_files.is_empty());
    }
}
#[cfg(windows)]
#[test]
fn case_only_organization_does_not_add_collision_suffixes() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    let id = s
        .save_media(&sample("1", "a"), &b"one"[..])
        .unwrap()
        .entity_id;
    let old = path(&s, id);
    s.connection
        .execute(
            "UPDATE users SET screen_name='TARGET' WHERE screen_name='target'",
            [],
        )
        .unwrap();
    assert_eq!(organize(&mut s, Layout::C).renamed, 0);
    assert_eq!(path(&s, id), old);
    intact(&s);
    assert!(s.inspect().unwrap().untracked_files.is_empty());
}
#[cfg(windows)]
#[test]
fn failed_move_reserves_its_target_then_resume_and_delete_release_names() {
    use std::os::windows::fs::OpenOptionsExt;
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    let first = s
        .save_media(&sample("1", "a"), &b"one"[..])
        .unwrap()
        .entity_id;
    let old = path(&s, first);
    // Allow reading, but deny file deletion/movement until the handle is dropped.
    let busy = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(s.root.join(&old))
        .unwrap();
    let p = s.organize_media(Layout::A, &AtomicBool::new(false), |_| {});
    assert!(p.error.is_some(), "{p:?}");
    assert_eq!(path(&s, first), old);
    let planned: String = s
        .connection
        .query_row(
            "SELECT new_path FROM media_rename_log WHERE entity_id=?1 AND state='planned'",
            [first],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!s.root.join(&planned).exists());
    let mut source = sample("1", "a");
    source.media_key = "another-slot";
    source.sequence = Some(1);
    let second = s.save_media(&source, &b"two"[..]).unwrap().entity_id;
    assert_eq!(path(&s, second), planned.replace(".jpg", " (2).jpg"));
    drop(busy);
    organize(&mut s, Layout::A);
    assert_eq!(path(&s, first), planned);
    intact(&s);
    assert!(s.inspect().unwrap().untracked_files.is_empty());
    let preview = s
        .preview_account_delete("target", DeleteScope::Images)
        .unwrap();
    s.delete_account_data(
        "target",
        DeleteScope::Images,
        &DeleteOptions {
            preview_token: Some(preview.token),
            ..Default::default()
        },
    )
    .unwrap();
    let restored = s
        .save_media(&sample("1", "a"), &b"one"[..])
        .unwrap()
        .entity_id;
    assert_eq!(path(&s, restored), planned);
    intact(&s);
}
#[test]
fn local_dates_cross_month_and_first_source_order() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    organize(&mut s, Layout::A);
    let mut source = sample("200", "author");
    source.sequence = Some(2);
    source.published_at = Some("2026-03-01T00:30:00+14:00");
    let id = s.save_media(&source, &b"same bytes"[..]).unwrap().entity_id;
    let local: String = s
        .connection
        .query_row(
            "SELECT strftime('%Y-%m-%d',?1,'localtime')",
            [source.published_at],
            |r| r.get(0),
        )
        .unwrap();
    assert!(path(&s, id).contains(&format!("/{}/{}_200_2.jpg", &local[..7], local)));
    source.tweet_id = "100";
    source.published_at = Some("2026-01-15T12:00:00Z");
    source.sequence = Some(4);
    assert!(s.save_media(&source, &b"same bytes"[..]).unwrap().reused);
    assert!(path(&s, id).contains("/2026-01/"));
    assert!(path(&s, id).ends_with("_100_4.jpg"));
    source.tweet_id = "99";
    s.save_media(&source, &b"same bytes"[..]).unwrap();
    assert!(path(&s, id).ends_with("_99_4.jpg"));
    source.tweet_id = "400";
    source.published_at = Some("2026-01-15T12:00:00.100Z");
    s.save_media(&source, &b"subsecond"[..]).unwrap();
    source.tweet_id = "500";
    source.published_at = Some("2026-01-15T12:00:00.010Z");
    let sub = s.save_media(&source, &b"subsecond"[..]).unwrap();
    assert!(path(&s, sub.entity_id).ends_with("_500_4.jpg"));
    source.tweet_id = "300";
    source.published_at = Some("2026-05-20T12:00:00Z");
    s.save_media(&source, &b"other bytes"[..]).unwrap();
    assert_eq!(status(d.path()).unwrap().media, 3);
    intact(&s);
}
#[test]
fn account_rename_waits_for_organization_and_roundtrip_is_consistent() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    let id = s
        .save_media(&sample("1", "a"), &b"one"[..])
        .unwrap()
        .entity_id;
    let mut source = sample("2", "a");
    source.media_type = "video";
    s.save_media(&source, &b"two"[..]).unwrap();
    s.connection
        .execute(
            "UPDATE users SET screen_name='renamed' WHERE screen_name='target'",
            [],
        )
        .unwrap();
    source.target.screen_name = "renamed";
    source.tweet_id = "0";
    s.save_media(&source, &b"one"[..]).unwrap();
    assert!(path(&s, id).starts_with("media/@target/"));
    assert_eq!(
        s.media_directory(Some("renamed")).unwrap(),
        s.root.join("media/@target")
    );
    for layout in [Layout::A, Layout::C, Layout::A, Layout::C] {
        assert_eq!(organize(&mut s, layout).renamed, 2);
        intact(&s);
        assert_eq!(media_files(&s.root).unwrap().len(), 2);
        assert_eq!(status(d.path()).unwrap().layout, layout);
        assert!(path(&s, id).starts_with("media/@renamed/"));
        let logs: i64 = s
            .connection
            .query_row(
                "SELECT count(*) FROM media_rename_log WHERE state<>'done'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logs, 0);
    }
    assert!(!s.root.join("media/@target").exists());
    assert_eq!(
        s.media_directory(Some("renamed")).unwrap(),
        s.root.join("media/@renamed")
    );
    assert_eq!(organize(&mut s, Layout::C).renamed, 0);
}
#[test]
fn v5_schema_upgrade_keeps_bin_until_default_c_organization() {
    let d = isolated_directory();
    let c = migration_fixture(d.path(), 5);
    c.execute(
        "UPDATE media_sources SET original_filename='tweet_02.jpg'",
        [],
    )
    .unwrap();
    fs::create_dir_all(d.path().join("media")).unwrap();
    fs::write(d.path().join("media/kept.bin"), b"old!").unwrap();
    c.execute(
        "UPDATE media_entities SET sha256=?1",
        [hex_digest(&Sha256::digest(b"old!"))],
    )
    .unwrap();
    drop(c);
    let mut s = Store::open(d.path()).unwrap();
    assert_eq!(path(&s, 7), "media/kept.bin");
    assert_eq!(status(d.path()).unwrap().layout, Layout::C);
    let p = organize(&mut s, Layout::C);
    assert_eq!(p.renamed, 1);
    assert!(Path::new(&p.backup).is_file());
    assert!(path(&s, 7).starts_with("media/@kept/图片/"));
    assert!(path(&s, 7).ends_with("_tweet_3.jpg"));
    assert!(!d.path().join("media/kept.bin").exists());
    intact(&s);
}
#[test]
fn planned_and_renamed_crashes_recover_on_open_without_copying() {
    for moved in [false, true] {
        for state in ["planned", "renamed"] {
            let d = isolated_directory();
            let mut s = Store::open(d.path()).unwrap();
            let id = s
                .save_media(&sample("1", "a"), &b"recover bytes"[..])
                .unwrap()
                .entity_id;
            let old = path(&s, id);
            let new = "media/@target/2026-10/recovered.jpg";
            s.connection
                .execute(
                    "INSERT INTO media_rename_log VALUES(?1,?2,?3,'image','jpg','target',?4)",
                    params![id, old, new, state],
                )
                .unwrap();
            if moved {
                let new_path = media_path(&s.root, new).unwrap();
                fs::create_dir_all(new_path.parent().unwrap()).unwrap();
                rename_noclobber(&media_path(&s.root, &old).unwrap(), &new_path).unwrap();
            }
            drop(s);
            let mut s = Store::open(d.path()).unwrap();
            intact(&s);
            assert_eq!(path(&s, id), if moved { new } else { &old });
            let state: String = s
                .connection
                .query_row(
                    "SELECT state FROM media_rename_log WHERE entity_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(state, if moved { "done" } else { "planned" });
            organize(&mut s, Layout::C);
            intact(&s);
            assert_eq!(media_files(&s.root).unwrap().len(), 1);
            drop(s);
            intact(&Store::open(d.path()).unwrap());
        }
    }
}
#[test]
fn recovery_conflicts_preserve_both_files_and_log() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    let id = s
        .save_media(&sample("1", "a"), &b"original"[..])
        .unwrap()
        .entity_id;
    let old = path(&s, id);
    let new = "media/conflict.jpg";
    fs::write(s.root.join(new), b"user").unwrap();
    s.connection
        .execute(
            "INSERT INTO media_rename_log VALUES(?1,?2,?3,'image','jpg','target','planned')",
            params![id, old, new],
        )
        .unwrap();
    drop(s);
    assert!(Store::open(d.path()).is_err());
    assert_eq!(fs::read(d.path().join(old)).unwrap(), b"original");
    assert_eq!(fs::read(d.path().join(new)).unwrap(), b"user");
}
#[test]
fn stopped_task_can_resume_and_delete_respects_nested_layout() {
    let d = isolated_directory();
    let mut s = Store::open(d.path()).unwrap();
    for (tweet, body) in [("1", b"one"), ("2", b"two")] {
        s.save_media(&sample(tweet, "a"), &body[..]).unwrap();
    }
    let cancel = AtomicBool::new(true);
    let p = s.organize_media(Layout::A, &cancel, |_| {});
    assert!(p.stopped);
    assert_eq!(p.done, 0);
    intact(&s);
    organize(&mut s, Layout::A);
    intact(&s);
    organize(&mut s, Layout::C);
    intact(&s);
    s.connection
        .execute("UPDATE storage_layout SET layout='A'", [])
        .unwrap();
    let cancel = AtomicBool::new(false);
    let id: i64 = s
        .connection
        .query_row("SELECT min(entity_id) FROM media_entities", [], |r| {
            r.get(0)
        })
        .unwrap();
    s.organize_entity(id, true).unwrap();
    cancel.store(true, Ordering::Relaxed);
    let p = s.organize_media(Layout::A, &cancel, |_| {});
    assert!(p.stopped);
    intact(&s);
    organize(&mut s, Layout::A);
    intact(&s);
    let preview = s
        .preview_account_delete("target", DeleteScope::Images)
        .unwrap();
    let result = s
        .delete_account_data(
            "target",
            DeleteScope::Images,
            &DeleteOptions {
                preview_token: Some(preview.token),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.deleted.files, 2);
    assert!(result.orphaned_files.is_empty());
    assert!(media_files(&s.root).unwrap().is_empty());
    assert!(s.root.join("media").is_dir());
}

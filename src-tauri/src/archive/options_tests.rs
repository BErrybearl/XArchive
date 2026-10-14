use super::tests::*;
use super::*;

#[test]
fn retry_only_never_scans_or_advances_checkpoints() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13)], None)],
        Media {
            failures: 1,
            ..Default::default()
        },
    );
    task(
        &mut store,
        &options(Mode::Full),
        &AtomicBool::new(false),
        &mut n,
    );
    let before: String = store
        .connection()
        .query_row(
            "SELECT coalesce(cursor,'')||exhausted FROM archive_checkpoints",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut o = options(Mode::Retry);
    o.preferences.media_types = Some(MediaTypes {
        image: false,
        video: true,
        gif: false,
    });
    let mut s = Session::default();
    let mut n = network(&mut s, vec![], Media::default());
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.outcome, "retried");
    assert_eq!(p.pages, 0);
    assert!(n.requests.transport.calls.is_empty());
    assert_eq!(n.media.calls.len(), 1);
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT coalesce(cursor,'')||exhausted FROM archive_checkpoints",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        before
    );
}

#[test]
fn retry_lower_limit_marks_pending_without_reviving_failed_items() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13), tweet(2, 13)], None)],
        Media {
            failures: 2,
            ..Default::default()
        },
    );
    task(
        &mut store,
        &options(Mode::Full),
        &AtomicBool::new(false),
        &mut n,
    );
    store
        .connection()
        .execute(
            "UPDATE archive_retries SET status='failed',attempts=3 WHERE tweet_id='2'",
            [],
        )
        .unwrap();
    let mut o = options(Mode::Retry);
    o.preferences.retry_limit = 1;
    let mut s = Session::default();
    let mut n = network(&mut s, vec![], Media::default());
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!((p.pending, p.failed), (0, 2));
    assert!(n.requests.transport.calls.is_empty());
    assert!(n.media.calls.is_empty());
    o.preferences.retry_limit = 5;
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!((p.pending, p.failed), (0, 2));
    assert!(n.media.calls.is_empty());
}

fn moving(id: u64, kind: &str) -> serde_json::Value {
    let mut t = tweet(id, 13);
    t["legacy"]["extended_entities"]["media"] = json!([{"type":kind,"media_key":format!("7_{id}"),"video_info":{"variants":[
        {"content_type":"video/mp4","bitrate":100,"url":"https://video.twimg.com/low.mp4"},
        {"content_type":"video/mp4","bitrate":500,"url":"https://video.twimg.com/mid.mp4"},
        {"content_type":"video/mp4","bitrate":900,"url":"https://video.twimg.com/high.mp4"}
    ]}}]);
    t
}
#[test]
fn media_logs_are_one_per_item_and_bytes_are_counted() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13), tweet(2, 13)], None)],
        Media::default(),
    );
    let p = task(
        &mut store,
        &options(Mode::Full),
        &AtomicBool::new(false),
        &mut n,
    );
    assert_eq!(
        p.logs
            .iter()
            .filter(|l| l.contains("下载 图片") || l.contains("复用 已有媒体"))
            .count(),
        2
    );
    assert_eq!(p.bytes, 2 * b"same-synthetic-content".len() as u64);
    assert_eq!(p.page_total, 2);
    assert_eq!(p.page_item, 2);
    assert!(p.logs.iter().any(|l| l.contains("第 1 页已保存")));
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(3, 13)], None)],
        Media {
            failures: 1,
            ..Media::default()
        },
    );
    let p = task(
        &mut store,
        &options(Mode::Full),
        &AtomicBool::new(false),
        &mut n,
    );
    assert_eq!(
        p.logs
            .iter()
            .filter(|l| l.contains("失败 ·") && l.contains("将重试"))
            .count(),
        1
    );
}
#[test]
fn progress_throttle_keeps_key_events_and_log_limit() {
    let mut p = Progress::default();
    let mut events = Vec::new();
    let mut notify = |p| events.push(p);
    push(&mut p, true, &mut notify);
    for i in 0..205 {
        media_log(&mut p, format!("下载 {i}"), false, &mut notify);
    }
    assert_eq!(p.logs.len(), 200);
    assert!(p.logs[0].ends_with("下载 5"));
    log(&mut p, "第 1 页已保存", &mut notify);
    media_log(&mut p, "失败 · 将重试", true, &mut notify);
    finish(&mut p, Ok(()), &AtomicBool::new(false));
    push(&mut p, true, &mut notify);
    assert!(events.len() <= 5);
    assert!(
        events
            .iter()
            .any(|p| p.logs.last().is_some_and(|s| s.contains("页已保存")))
    );
    assert!(
        events
            .iter()
            .any(|p| p.logs.last().is_some_and(|s| s.contains("失败 ·")))
    );
    assert!(!events.last().unwrap().running);
    let mut p = Progress::default();
    p.last_push = Some(Instant::now() - Duration::from_millis(151));
    let mut sent = 0;
    push(&mut p, false, &mut |_| sent += 1);
    assert_eq!(sent, 1);
}
#[test]
fn gif_and_video_are_independent_and_metadata_remains() {
    for (video, gif, want) in [(false, true, "high.mp4"), (true, false, "high.mp4")] {
        let d = directory();
        let mut store = Store::open(d.path()).unwrap();
        let mut s = Session::default();
        let mut o = options(Mode::Full);
        o.preferences.media_types = Some(MediaTypes {
            image: false,
            video,
            gif,
        });
        let mut n = network(
            &mut s,
            vec![page(
                vec![moving(1, "video"), moving(2, "animated_gif"), tweet(3, 13)],
                None,
            )],
            Media::default(),
        );
        let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
        assert_eq!(n.media.calls.len(), 1);
        assert!(n.media.calls[0].ends_with(want));
        assert_eq!(p.skipped, 2);
        assert_eq!(storage::status(d.path()).unwrap().tweets, 3);
        let kind: String = store
            .connection()
            .query_row("SELECT media_type FROM media_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, if gif { "gif" } else { "video" });
    }
}
#[test]
fn retweet_toggle_preserves_outer_and_original_metadata() {
    for enabled in [false, true] {
        let d = directory();
        let mut store = Store::open(d.path()).unwrap();
        let mut s = Session::default();
        let mut outer = tweet(2, 13);
        outer["legacy"]["extended_entities"]["media"] = json!([]);
        let mut original = tweet(1, 11);
        original["core"]["user_results"]["result"]["rest_id"] = json!("2");
        original["core"]["user_results"]["result"]["legacy"]["screen_name"] = json!("author");
        outer["legacy"]["retweeted_status_result"] = json!({"result":original});
        let mut o = options(Mode::Full);
        o.preferences.include_retweets = enabled;
        let mut n = network(&mut s, vec![page(vec![outer], None)], Media::default());
        let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
        assert_eq!(n.media.calls.len(), usize::from(enabled));
        assert_eq!(p.tweets, 2);
        assert_eq!(storage::list_archived_accounts(d.path()).unwrap().len(), 1);
    }
}
#[test]
fn date_filter_saves_whole_page_and_cutoff_never_completes_full() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut o = options(Mode::Full);
    o.preferences.date_start = Some("2026-10-12".into());
    o.preferences.date_end = Some("2026-10-13".into());
    let mut n = network(
        &mut s,
        vec![page(
            vec![tweet(1, 14), tweet(2, 13), tweet(3, 11), tweet(4, 12)],
            Some("unused"),
        )],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.outcome, "date_range");
    assert!(!p.exhausted);
    assert_eq!(p.pages, 1);
    assert_eq!(n.media.calls.len(), 2);
    assert_eq!(p.tweets, 4);
    assert_eq!(p.skipped, 2);
    let cp = checkpoint(&store, "1", &format!("full:UserMedia:{}", o.scope_key())).unwrap();
    assert!(cp.completed_at.is_none());
    assert!(!cp.exhausted);
    assert!(cp.cursor.is_none());
    assert!(
        storage::list_archived_accounts(d.path()).unwrap()[0]
            .full_scope
            .is_none()
    );
}
#[test]
fn pinned_old_post_does_not_trigger_date_cutoff() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut first = page(vec![tweet(1, 10), tweet(2, 13)], Some("next"));
    let mut v: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    let entries =
        &mut v["data"]["user"]["result"]["timeline_v2"]["timeline"]["instructions"][0]["entries"];
    let pin = entries.as_array_mut().unwrap().remove(0);
    v["data"]["user"]["result"]["timeline_v2"]["timeline"]["instructions"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"TimelinePinEntry","entry":pin}));
    first.body = serde_json::to_vec(&v).unwrap();
    let mut o = options(Mode::Full);
    o.preferences.date_start = Some("2026-10-12".into());
    let mut n = network(
        &mut s,
        vec![first, page(vec![tweet(3, 12)], None)],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.pages, 2);
    assert_eq!(p.outcome, "completed");
}
#[test]
fn quality_url_selection_and_existing_source_survives_quality_change() {
    let parsed = parser::archive_page(
        &serde_json::from_slice(&page(vec![moving(1, "video"), tweet(2, 13)], None).body).unwrap(),
    )
    .unwrap();
    let mut prefs = Preferences::default();
    prefs.video_quality = "medium".into();
    prefs.image_quality = "large".into();
    assert!(
        prefs
            .select_quality(&parsed.tweets[0].media[0])
            .url
            .ends_with("mid.mp4")
    );
    assert!(
        prefs
            .select_quality(&parsed.tweets[1].media[0])
            .url
            .ends_with("name=large")
    );
    let mut two = parsed.tweets[0].media[0].clone();
    two.variants.remove(1);
    assert!(prefs.select_quality(&two).url.ends_with("high.mp4"));
    let mid = prefs.select_quality(&parsed.tweets[0].media[0]);
    assert!(
        Preferences::default()
            .select_quality(&mid)
            .url
            .ends_with("high.mp4")
    );
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let o = options(Mode::Full);
    let mut n = network(
        &mut s,
        vec![page(vec![moving(1, "video"), tweet(2, 13)], None)],
        Media::default(),
    );
    task(&mut store, &o, &AtomicBool::new(false), &mut n);
    let mut o = o;
    o.preferences = prefs;
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![moving(1, "video"), tweet(2, 13)], None)],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert!(n.media.calls.is_empty());
    assert_eq!(p.skipped, 2);
}
#[test]
fn retry_limit_changes_reclassify_durable_queue() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut o = options(Mode::Full);
    o.preferences.retry_limit = 1;
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13)], None)],
        Media {
            failures: 1,
            ..Media::default()
        },
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!((p.pending, p.failed), (0, 1));
    o.preferences.retry_limit = 5;
    o.mode = Mode::Incremental;
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13)], None)],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!((p.pending, p.failed), (0, 0));
    assert_eq!(n.media.calls.len(), 1);
}
#[test]
fn settings_roundtrip_and_invalid_selection_do_not_overwrite() {
    let d = directory();
    let path = d.path().join("archive-options.json");
    let mut s = Settings::default();
    s.target = "fixture".into();
    s.include_timeline = true;
    s.preferences = Preferences {
        media_types: Some(MediaTypes {
            image: true,
            video: false,
            gif: true,
        }),
        include_retweets: false,
        date_start: Some("2026-10-01".into()),
        date_end: Some("2026-10-13".into()),
        image_quality: "large".into(),
        video_quality: "medium".into(),
        request_pace: "careful".into(),
        concurrency: 3,
        retry_limit: 5,
    };
    save_settings(&path, &s).unwrap();
    assert_eq!(
        serde_json::to_value(load_settings(&path).unwrap()).unwrap(),
        serde_json::to_value(&s).unwrap()
    );
    let before = std::fs::read(&path).unwrap();
    s.preferences.media_types = Some(MediaTypes {
        image: false,
        video: false,
        gif: false,
    });
    assert!(save_settings(&path, &s).is_err());
    assert_eq!(before, std::fs::read(path).unwrap());
    assert!(date_seconds("2026-02-29").is_none());
    assert!(date_seconds("2024-02-29").is_some());
}
#[test]
fn accounts_query_tracks_modes_baseline_and_empty_target() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13)], None)],
        Media::default(),
    );
    task(
        &mut store,
        &options(Mode::Full),
        &AtomicBool::new(false),
        &mut n,
    );
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13)], None)],
        Media::default(),
    );
    let p = task(
        &mut store,
        &options(Mode::Incremental),
        &AtomicBool::new(false),
        &mut n,
    );
    assert_eq!(p.outcome, "updated");
    let rows = storage::list_archived_accounts(d.path()).unwrap();
    let a = &rows[0];
    assert_eq!(a.media, 1);
    assert!(a.last_full_at.is_some());
    assert!(a.last_incremental_at.is_some());
    assert_eq!(a.full_scope.as_deref(), Some("all:timeline0"));
    let mut o = options(Mode::Incremental);
    o.preferences.date_start = Some("2026-10-12".into());
    assert_ne!(Some(o.baseline_key()), a.full_scope);
    let account = Account {
        user_id: "99".into(),
        screen_name: "empty".into(),
        display_name: "Empty".into(),
    };
    save_checkpoint(
        &store,
        &account,
        "full:UserMedia:all",
        &Checkpoint::default(),
    )
    .unwrap();
    assert_eq!(storage::list_archived_accounts(d.path()).unwrap().len(), 2);
}

#[derive(Clone)]
struct ParallelMedia {
    barrier: std::sync::Arc<std::sync::Barrier>,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    peak: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    closed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    cancel: Option<std::sync::Arc<AtomicBool>>,
}
struct ParallelBody {
    owner: ParallelMedia,
    body: std::io::Cursor<Vec<u8>>,
}
impl std::io::Read for ParallelBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(c) = &self.owner.cancel {
            c.store(true, Ordering::Relaxed);
        }
        std::io::Read::read(&mut self.body, buf)
    }
}
impl Drop for ParallelBody {
    fn drop(&mut self) {
        self.owner.active.fetch_sub(1, Ordering::SeqCst);
        self.owner.closed.fetch_add(1, Ordering::SeqCst);
    }
}
impl MediaTransport for ParallelMedia {
    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        Some(Box::new(self.clone()))
    }
    fn open(
        &mut self,
        _: url::Url,
        h: reqwest::header::HeaderMap,
    ) -> std::result::Result<Box<dyn std::io::Read>, download::DownloadError> {
        assert!(h.is_empty());
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        // Hold this batch until every requested worker has an open stream.
        self.barrier.wait();
        Ok(Box::new(ParallelBody {
            owner: self.clone(),
            body: std::io::Cursor::new(vec![42; 2048]),
        }))
    }
}
fn parallel(width: usize, cancel: Option<std::sync::Arc<AtomicBool>>) -> ParallelMedia {
    ParallelMedia {
        barrier: std::sync::Arc::new(std::sync::Barrier::new(width)),
        active: Default::default(),
        peak: Default::default(),
        closed: Default::default(),
        cancel,
    }
}

struct CoordinatorApi<T> {
    inner: T,
    thread: std::thread::ThreadId,
}
impl<T: Transport> Transport for CoordinatorApi<T> {
    fn get(
        &mut self,
        url: url::Url,
        headers: reqwest::header::HeaderMap,
    ) -> Result<platform::client::Response> {
        assert_eq!(std::thread::current().id(), self.thread);
        self.inner.get(url, headers)
    }
}
#[test]
fn concurrency_uses_requested_limit_and_api_stays_on_coordinator() {
    for width in 1..=3 {
        let d = directory();
        let mut store = Store::open(d.path()).unwrap();
        let mut s = Session::default();
        let mut o = options(Mode::Full);
        o.preferences.concurrency = width;
        let mock = network(
            &mut s,
            vec![page((1..=6).map(|id| tweet(id, 13)).collect(), None)],
            Media::default(),
        );
        let m = parallel(width, None);
        let peak = m.peak.clone();
        let active = m.active.clone();
        let closed = m.closed.clone();
        let mut n = Network {
            requests: Requests {
                transport: CoordinatorApi {
                    inner: mock.requests.transport,
                    thread: std::thread::current().id(),
                },
                session: mock.requests.session,
                delay: mock.requests.delay,
            },
            media: m,
            params: mock.params,
            login: mock.login,
        };
        let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
        assert_eq!(peak.load(Ordering::SeqCst), width);
        // download_batch joins its scoped threads before returning: these
        // lifecycle counters require no timing assumptions or polling.
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(closed.load(Ordering::SeqCst), 6);
        assert_eq!(p.outcome, "completed");
        assert_eq!(p.added + p.reused, 6);
        assert_eq!(n.requests.transport.inner.calls.len(), 1);
        assert!(store.inspect().unwrap().missing_files.is_empty());
    }
}
#[test]
fn concurrent_stop_drains_active_media_and_keeps_page_checkpoint() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let c = std::sync::Arc::new(AtomicBool::new(false));
    let mock = network(
        &mut s,
        vec![page(
            vec![tweet(1, 13), tweet(2, 13), tweet(3, 13)],
            Some("next"),
        )],
        Media::default(),
    );
    let mut n = Network {
        requests: mock.requests,
        media: parallel(2, Some(c.clone())),
        params: mock.params,
        login: mock.login,
    };
    let p = task(&mut store, &options(Mode::Full), &c, &mut n);
    assert_eq!(p.outcome, "stopped");
    assert_eq!(p.pages, 0);
    assert_eq!(p.added + p.reused, 2);
    assert_eq!(p.pending, 1);
    let cp = checkpoint(&store, "1", "full:UserMedia:all").unwrap();
    assert!(cp.cursor.is_none());
    assert!(!cp.exhausted);
    assert_eq!(storage::status(d.path()).unwrap().sources, 2);
    assert!(store.inspect().unwrap().missing_files.is_empty());
}

#[test]
fn date_cutoff_with_download_failure_remains_incomplete() {
    let d = directory();
    let mut store = Store::open(d.path()).unwrap();
    let mut s = Session::default();
    let mut o = options(Mode::Full);
    o.preferences.date_start = Some("2026-10-12".into());
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 13), tweet(2, 11)], Some("unused"))],
        Media {
            failures: 1,
            ..Media::default()
        },
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.outcome, "incomplete");
    assert!(p.date_ended);
    assert_eq!(p.pending, 1);
    assert!(p.summary.contains("按日期范围结束"));
    assert!(!p.exhausted);
}
#[test]
fn start_time_and_request_pace_have_valid_defaults() {
    let d = directory();
    let mut events = Vec::new();
    let mut s = Session::default();
    let p = run(
        d.path(),
        &mut s,
        &options(Mode::Full),
        &AtomicBool::new(false),
        |p| events.push(p),
    );
    assert!(events[0].started_at > 0);
    assert!(p.finished_at.unwrap() >= p.started_at);
    let mut pref = Preferences::default();
    assert_eq!(pref.request_delay(), Duration::from_secs(1));
    pref.request_pace = "careful".into();
    assert_eq!(pref.request_delay(), Duration::from_secs(2));
}
#[test]
fn upgraded_v2_accounts_keep_completed_baseline() {
    let d = directory();
    let db = d.path().join("archive.sqlite3");
    let c = rusqlite::Connection::open(db).unwrap();
    c.execute_batch(include_str!("../storage/schema.sql"))
        .unwrap();
    c.execute_batch(include_str!("../storage/archive-schema.sql"))
        .unwrap();
    c.execute_batch("INSERT INTO users(user_id,screen_name) VALUES ('1','fixture');INSERT INTO archive_checkpoints(target_user_id,mode,exhausted,completed_at,completed_newest) VALUES ('1','full:UserMedia:image',1,1000,900),('1','full:UserTweets:image',1,1000,900);").unwrap();
    drop(c);
    let before = storage::list_archived_accounts(d.path()).unwrap();
    assert_eq!(before[0].full_scope.as_deref(), Some("image:timeline1"));
    let _store = Store::open(d.path()).unwrap();
    let after = storage::list_archived_accounts(d.path()).unwrap();
    assert_eq!(after[0].full_scope, before[0].full_scope);
    assert_eq!(after[0].last_full_at, Some(1000));
    let backups: Vec<_> = std::fs::read_dir(d.path().join("temporary"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
}

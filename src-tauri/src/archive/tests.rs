use super::*;
use platform::client::Response;
use reqwest::header::HeaderMap;
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{Cursor, Read},
    sync::Arc,
};
use url::Url;
use zeroize::Zeroizing;

pub(super) fn directory() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/archive-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}
pub(super) fn target() -> Account {
    Account {
        user_id: "1".into(),
        screen_name: "fixture".into(),
        display_name: "合成账号".into(),
    }
}
pub(super) fn options(mode: Mode) -> Options {
    Options {
        target: "fixture".into(),
        mode,
        filter: Filter::All,
        include_timeline: false,
        preferences: Preferences::default(),
    }
}
pub(super) fn tweet(id: u64, day: u8) -> Value {
    json!({"__typename":"Tweet","rest_id":id.to_string(),"core":{"user_results":{"result":{"rest_id":"1","legacy":{"screen_name":"fixture","name":"Fixture"}}}},"legacy":{"id_str":id.to_string(),"full_text":"合成推文","created_at":format!("Mon Oct {day:02} 12:00:00 +0000 2026"),"extended_entities":{"media":[{"type":"photo","media_key":format!("3_{id}"),"media_url_https":format!("https://pbs.twimg.com/media/{id}.jpg?name=small")}]}}})
}
pub(super) fn page(tweets: Vec<Value>, next: Option<&str>) -> Response {
    let mut entries:Vec<Value>=tweets.into_iter().map(|t|json!({"content":{"itemContent":{"itemType":"TimelineTweet","tweet_results":{"result":t}}}})).collect();
    if let Some(next) = next {
        entries.push(json!({"content":{"cursorType":"Bottom","value":next}}));
    }
    Response{status:200,rate:RateLimit{remaining:Some(100),reset_at:Some(platform::client::now()+1000)},body:serde_json::to_vec(&json!({"data":{"user":{"result":{"timeline_v2":{"timeline":{"instructions":[{"type":"TimelineAddEntries","entries":entries}]}}}}}})).unwrap()}
}
fn status(code: u16) -> Response {
    Response {
        status: code,
        rate: RateLimit {
            remaining: None,
            reset_at: Some(platform::client::now() + 1000),
        },
        body: b"private synthetic_auth_secret".to_vec(),
    }
}
pub(super) struct Api {
    queue: VecDeque<Response>,
    pub(super) calls: Vec<(String, Option<String>)>,
}
impl Transport for Api {
    fn get(&mut self, url: Url, headers: HeaderMap) -> Result<Response> {
        assert_eq!(url.host_str(), Some("x.com"));
        assert!(headers["cookie"].is_sensitive());
        let variables: Value =
            serde_json::from_str(&url.query_pairs().find(|(k, _)| k == "variables").unwrap().1)
                .unwrap();
        self.calls.push((
            url.path().rsplit('/').next().unwrap().into(),
            variables["cursor"].as_str().map(str::to_owned),
        ));
        Ok(self.queue.pop_front().expect("unexpected API request"))
    }
}
#[derive(Default)]
pub(super) struct Media {
    pub(super) calls: Vec<String>,
    pub(super) failures: usize,
    pub(super) stop: Option<Arc<AtomicBool>>,
    pub(super) broken: bool,
}
struct Body {
    cursor: Cursor<Vec<u8>>,
    pub(super) stop: Option<Arc<AtomicBool>>,
    pub(super) broken: bool,
    read: bool,
}
impl Read for Body {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        if self.broken && self.read {
            return Err(std::io::Error::other("synthetic_auth_secret"));
        }
        self.read = true;
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.cursor.read(b)
    }
}
impl MediaTransport for Media {
    fn open(
        &mut self,
        url: Url,
        headers: HeaderMap,
    ) -> std::result::Result<Box<dyn Read>, download::DownloadError> {
        assert!(headers.is_empty());
        assert!(matches!(
            url.host_str(),
            Some("pbs.twimg.com" | "video.twimg.com")
        ));
        self.calls.push(url.into());
        if self.failures > 0 {
            self.failures -= 1;
            return Err(download::DownloadError::Network);
        }
        Ok(Box::new(Body {
            cursor: Cursor::new(b"same-synthetic-content".to_vec()),
            stop: self.stop.clone(),
            broken: self.broken,
            read: false,
        }))
    }
}
pub(super) fn network(
    session: &mut Session,
    replies: Vec<Response>,
    media: Media,
) -> Network<'_, Api, Media> {
    let mut params = params::defaults();
    params.public_bearer = "synthetic_bearer_secret".into();
    Network {
        requests: Requests {
            transport: Api {
                queue: replies.into(),
                calls: Vec::new(),
            },
            session,
            delay: Duration::ZERO,
        },
        media,
        params,
        login: PlatformLogin {
            account: target(),
            proxy: String::new(),
            auth_token: Zeroizing::new("synthetic_auth_secret".into()),
            ct0: Zeroizing::new("synthetic_csrf_secret".into()),
        },
    }
}
pub(super) fn task(
    store: &mut Store,
    opts: &Options,
    cancel: &AtomicBool,
    n: &mut impl Backend,
) -> Progress {
    let mut p = Progress {
        running: true,
        ..Progress::default()
    };
    let r = execute(store, &target(), opts, cancel, n, &mut p, &mut |_| {});
    finish(&mut p, r, cancel);
    p
}
#[test]
fn multi_page_full_and_repeat_preserve_sources_and_exact_dedupe() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let mut s = Session::default();
    let cancel = AtomicBool::new(false);
    let mut n = network(
        &mut s,
        vec![
            page(vec![tweet(102, 13)], Some("a")),
            page(vec![tweet(101, 12)], None),
        ],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "completed");
    assert!(p.exhausted);
    assert_eq!((p.pages, p.added, p.reused), (2, 1, 1));
    assert_eq!(n.requests.transport.calls[1].1.as_deref(), Some("a"));
    let cp = checkpoint(&store, "1", "full:UserMedia:all").unwrap();
    assert!(cp.completed_at.is_some());
    assert!(cp.completed_newest.is_some());
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(102, 13), tweet(101, 12)], None)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.skipped, 2);
    assert!(n.media.calls.is_empty());
    let info = storage::status(dir.path()).unwrap();
    assert_eq!(
        (info.tweets, info.media, info.sources, info.pending),
        (2, 1, 2, 0)
    );
    assert!(store.inspect().unwrap().missing_files.is_empty());
}
#[test]
fn checkpoint_resume_after_429_and_no_completion_until_end() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let mut s = Session::default();
    let cancel = AtomicBool::new(false);
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(102, 13)], Some("resume")), status(429)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "rate_limit");
    assert_eq!(p.pages, 1);
    assert!(p.error.unwrap().reset_at.is_some());
    let cp = checkpoint(&store, "1", "full:UserMedia:all").unwrap();
    assert_eq!(cp.cursor.as_deref(), Some("resume"));
    assert!(cp.completed_at.is_none());
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(101, 12)], None)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "completed");
    assert_eq!(n.requests.transport.calls[0].1.as_deref(), Some("resume"));
    assert_eq!(storage::status(dir.path()).unwrap().sources, 2);
}
#[test]
fn invalid_cursor_error_and_repeated_cursor_fall_back_to_top() {
    for repeat in [false, true] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let cp = Checkpoint {
            cursor: Some("bad".into()),
            ..Checkpoint::default()
        };
        save_checkpoint(&store, &target(), "full:UserMedia:all", &cp).unwrap();
        let mut s = Session::default();
        let first = if repeat {
            page(vec![], Some("bad"))
        } else {
            status(400)
        };
        let mut n = network(
            &mut s,
            vec![first, page(vec![tweet(101, 12)], None)],
            Media::default(),
        );
        let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
        assert_eq!(p.outcome, "completed");
        assert!(p.logs.iter().any(|l| l.contains("正在跳过已归档部分")));
        assert_eq!(n.requests.transport.calls[1].1, None);
    }
}
#[test]
fn malformed_cursor_response_falls_back_but_homepage_schema_error_stops() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    save_checkpoint(
        &store,
        &target(),
        "full:UserMedia:all",
        &Checkpoint {
            cursor: Some("bad".into()),
            ..Checkpoint::default()
        },
    )
    .unwrap();
    let malformed = Response {
        status: 200,
        rate: RateLimit::default(),
        body: b"{\"data\":{}}".to_vec(),
    };
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![malformed, page(vec![tweet(1, 10)], None)],
        Media::default(),
    );
    assert_eq!(
        task(&mut store, &options(Mode::Full), &cancel, &mut n).outcome,
        "completed"
    );
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![Response {
            status: 200,
            rate: RateLimit::default(),
            body: b"{}".to_vec(),
        }],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "incomplete");
    assert_eq!(p.error.unwrap().category, Category::InterfaceChanged);
}
#[test]
fn incremental_checks_whole_extra_page_before_time_boundary() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media::default(),
    );
    task(&mut store, &options(Mode::Full), &cancel, &mut n);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![
            page(vec![tweet(100, 10), tweet(102, 12)], Some("a")),
            page(vec![tweet(99, 9), tweet(101, 11)], Some("b")),
        ],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Incremental), &cancel, &mut n);
    assert_eq!(p.outcome, "updated");
    assert_eq!(p.pages, 2);
    assert!(!p.exhausted);
    assert_eq!(storage::status(dir.path()).unwrap().tweets, 4);
    assert_eq!(n.requests.transport.calls[0].1, None);
    assert!(p.logs.iter().any(|l| l.contains("时间边界")));
}
#[test]
fn incremental_two_quiet_pages_and_pinned_old_post_do_not_hide_new_content() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media::default(),
    );
    task(&mut store, &options(Mode::Full), &cancel, &mut n);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![
            page(vec![tweet(100, 10)], Some("a")),
            page(vec![tweet(100, 10)], Some("b")),
        ],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Incremental), &cancel, &mut n);
    assert_eq!(p.pages, 2);
    assert_eq!(p.outcome, "updated");
    assert!(p.logs.iter().any(|l| l.contains("连续两页")));
}
#[test]
fn remaining_zero_commits_page_then_stops_without_next_request() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut response = page(vec![tweet(100, 10)], Some("a"));
    response.rate.remaining = Some(0);
    let mut n = network(&mut s, vec![response], Media::default());
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "rate_limit");
    assert_eq!(p.pages, 1);
    assert_eq!(
        checkpoint(&store, "1", "full:UserMedia:all")
            .unwrap()
            .cursor
            .as_deref(),
        Some("a")
    );
    assert_eq!(n.requests.transport.calls.len(), 1);
}
#[test]
fn authentication_failure_stops_and_logs_never_echo_credentials() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    for code in [401, 403] {
        let mut s = Session::default();
        let mut n = network(&mut s, vec![status(code)], Media::default());
        let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
        assert_eq!(p.outcome, "authentication");
        assert_eq!(n.requests.transport.calls.len(), 1);
        let json = serde_json::to_string(&p).unwrap();
        for secret in [
            "synthetic_auth_secret",
            "synthetic_csrf_secret",
            "synthetic_bearer_secret",
            "private",
        ] {
            assert!(!json.contains(secret));
        }
    }
}
#[test]
fn failed_media_is_pending_and_incremental_retries_it_once_per_run() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media {
            failures: 1,
            ..Media::default()
        },
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "incomplete");
    assert_eq!(p.pending, 1);
    assert_eq!(p.failures[0].attempts, 1);
    assert!(
        checkpoint(&store, "1", "full:UserMedia:all")
            .unwrap()
            .completed_at
            .is_none()
    );
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Incremental), &cancel, &mut n);
    assert_eq!(p.outcome, "updated");
    assert!(p.failures.is_empty());
    assert_eq!(n.media.calls.len(), 1);
    assert_eq!(storage::status(dir.path()).unwrap().sources, 1);
}
#[test]
fn failed_media_reaches_cap_and_cannot_be_misreported_complete() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    for attempt in 1..=4 {
        let mut s = Session::default();
        let replies = vec![page(vec![tweet(100, 10)], None)];
        let mut n = network(
            &mut s,
            replies,
            Media {
                failures: 100,
                ..Media::default()
            },
        );
        let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
        assert_eq!(
            p.outcome,
            if attempt >= 3 {
                "completed"
            } else {
                "incomplete"
            }
        );
        if attempt >= 3 {
            assert!(p.stage.contains("1 项失败"));
            assert!(
                checkpoint(&store, "1", "full:UserMedia:all")
                    .unwrap()
                    .completed_at
                    .is_some()
            );
        }
        assert_eq!(p.failures[0].attempts, attempt.min(3));
        assert_eq!(p.failed, u64::from(attempt >= 3));
        if attempt == 4 {
            assert!(n.media.calls.is_empty());
        }
    }
}

#[test]
fn ten_empty_cursor_pages_finish_but_short_gaps_do_not() {
    for gap in [0, 1, 2] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let mut replies = vec![page(vec![tweet(2, 12)], Some("start"))];
        for i in 0..gap {
            replies.push(page(vec![], Some(&format!("gap{i}"))));
        }
        replies.push(page(vec![tweet(1, 11)], Some("tail")));
        for i in 0..10 {
            replies.push(page(vec![], Some(&format!("tail{i}"))));
        }
        let mut s = Session::default();
        let mut n = network(&mut s, replies, Media::default());
        let p = task(
            &mut store,
            &options(Mode::Full),
            &AtomicBool::new(false),
            &mut n,
        );
        assert_eq!(p.outcome, "completed");
        assert!(p.exhausted);
        assert_eq!(p.pages, 12 + gap);
        assert_eq!(p.tweets, 2);
        assert!(
            checkpoint(&store, "1", "full:UserMedia:all")
                .unwrap()
                .completed_at
                .is_some()
        );
    }
}

#[test]
fn empty_first_page_is_an_interface_error_with_or_without_cursor() {
    for next in [None, Some("empty")] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let mut s = Session::default();
        let mut n = network(&mut s, vec![page(vec![], next)], Media::default());
        let p = task(
            &mut store,
            &options(Mode::Full),
            &AtomicBool::new(false),
            &mut n,
        );
        assert_eq!(p.error.unwrap().category, Category::InterfaceChanged);
        assert!(!p.exhausted);
        assert!(
            checkpoint(&store, "1", "full:UserMedia:all")
                .unwrap()
                .completed_at
                .is_none()
        );
    }
}

#[test]
fn exhausted_incomplete_scan_restarts_and_terminal_failures_keep_time_baseline() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let mut o = options(Mode::Full);
    o.preferences.retry_limit = 1;
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 10)], None)],
        Media {
            failures: 1,
            ..Default::default()
        },
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.outcome, "completed");
    assert_eq!(p.failed, 1);
    // Simulate the old durable state, where terminal failures prevented completion.
    store
        .connection()
        .execute(
            "UPDATE archive_checkpoints SET completed_at=NULL,completed_newest=NULL",
            [],
        )
        .unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(2, 13), tweet(1, 10)], None)],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(n.requests.transport.calls[0].1, None);
    assert_eq!((p.pages, p.added, p.failed), (1, 1, 1));
    assert_eq!(p.outcome, "completed");
    assert!(p.stage.contains("1 项失败"));
    let cp = checkpoint(&store, "1", "full:UserMedia:all").unwrap();
    assert_eq!(
        cp.completed_newest,
        parser::archive_page(
            &serde_json::from_slice::<Value>(&page(vec![tweet(2, 13)], None).body).unwrap()
        )
        .unwrap()
        .tweets[0]
            .timestamp
    );
    assert!(cp.completed_at.is_some());
    let summary: String = store
        .connection()
        .query_row(
            "SELECT summary FROM archive_runs WHERE target_user_id='1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(summary.contains("1 项失败"));
}

#[test]
fn out_of_scope_retries_do_not_block_completion() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 10)], None)],
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
    let mut o = options(Mode::Full);
    o.filter = Filter::Video;
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(1, 10)], None)],
        Media::default(),
    );
    let p = task(&mut store, &o, &AtomicBool::new(false), &mut n);
    assert_eq!(p.outcome, "completed");
    assert_eq!((p.pending, p.failed), (0, 0));
    assert!(n.media.calls.is_empty());
    assert_eq!(storage::status(dir.path()).unwrap().pending, 1);
}

#[test]
fn missing_or_truncated_media_is_redownloaded_without_losing_other_sources() {
    for missing in [true, false] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let mut s = Session::default();
        let mut n = network(
            &mut s,
            vec![page(vec![tweet(1, 10), tweet(2, 11)], None)],
            Media::default(),
        );
        task(
            &mut store,
            &options(Mode::Full),
            &AtomicBool::new(false),
            &mut n,
        );
        let path: String = store
            .connection()
            .query_row(
                "SELECT relative_path FROM media_entities LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let path = storage::media_path(store.root(), &path).unwrap();
        if missing {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::write(&path, b"changed").unwrap();
        }
        let mut s = Session::default();
        let mut n = network(
            &mut s,
            vec![page(vec![tweet(1, 10)], None)],
            Media::default(),
        );
        let p = task(
            &mut store,
            &options(Mode::Full),
            &AtomicBool::new(false),
            &mut n,
        );
        assert_eq!(p.outcome, "completed");
        assert_eq!(n.media.calls.len(), 1);
        assert_eq!(
            p.logs
                .iter()
                .filter(|l| l.contains("本地文件缺失，重新下载"))
                .count(),
            1
        );
        assert_eq!(storage::status(dir.path()).unwrap().sources, 2);
        let (path,len):(String,i64) = store.connection().query_row("SELECT e.relative_path,e.byte_length FROM media_sources s JOIN media_entities e USING(entity_id) WHERE s.tweet_id='1'", [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(
            std::fs::metadata(storage::media_path(store.root(), &path).unwrap())
                .unwrap()
                .len(),
            len as u64
        );
    }
}
#[test]
fn stop_during_media_keeps_index_consistent_and_original_cursor_for_resume() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(102, 12), tweet(101, 11)], Some("a"))],
        Media {
            stop: Some(cancel.clone()),
            ..Media::default()
        },
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "stopped");
    assert_eq!(storage::status(dir.path()).unwrap().sources, 1);
    assert_eq!(p.pending, 1);
    assert_eq!(
        checkpoint(&store, "1", "full:UserMedia:all")
            .unwrap()
            .cursor,
        None
    );
    assert!(store.inspect().unwrap().missing_files.is_empty());
    cancel.store(false, Ordering::Relaxed);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![
            page(vec![tweet(102, 12), tweet(101, 11)], Some("a")),
            page(vec![], None),
        ],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "completed");
    assert_eq!(storage::status(dir.path()).unwrap().sources, 2);
    assert_eq!(n.media.calls.len(), 1);
}
#[test]
fn partial_media_body_does_not_create_entity_or_source() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let mut s = Session::default();
    let cancel = AtomicBool::new(false);
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media {
            broken: true,
            ..Media::default()
        },
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.pending, 1);
    assert_eq!(storage::status(dir.path()).unwrap().sources, 0);
    assert!(store.inspect().unwrap().missing_files.is_empty());
    assert_eq!(store.inspect().unwrap().temporary_files, 0);
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("synthetic_auth_secret")
    );
}
#[test]
fn media_host_allowlist_and_redirects_are_strict() {
    for url in [
        "http://pbs.twimg.com/a.jpg",
        "https://pbs.twimg.com.evil.invalid/a.jpg",
        "https://evil.invalid/a.mp4",
        "https://user:pass@pbs.twimg.com/a.jpg",
        "https://pbs.twimg.com:444/a.jpg",
        "https://abs.twimg.com/a.jpg",
    ] {
        assert!(download::allowed(url).is_err());
    }
    assert!(download::allowed("https://pbs.twimg.com/a.jpg?name=orig").is_ok());
    assert!(download::allowed("https://video.twimg.com/a.mp4").is_ok());
    for status in [301, 302, 307, 308] {
        assert_eq!(
            download::response_status(status),
            Err(download::DownloadError::Redirect)
        );
    }
}
#[test]
fn shared_write_lock_excludes_archive_import_and_cleanup() {
    let access = crate::StorageAccess(std::sync::Mutex::new(()));
    let archive = access.0.try_lock().unwrap();
    assert!(access.0.try_lock().is_err());
    drop(archive);
    assert!(access.0.try_lock().is_ok());
}

fn video(id: u64, kind: &str) -> Value {
    let mut t = tweet(id, 12);
    t["legacy"]["extended_entities"]["media"] = json!([{"type":kind,"media_key":format!("7_{id}"),"video_info":{"variants":[
        {"content_type":"application/x-mpegURL","url":"https://video.twimg.com/stream.m3u8"},
        {"content_type":"video/mp4","bitrate":100,"url":"https://video.twimg.com/low.mp4"},
        {"content_type":"video/mp4","bitrate":9000,"url":"https://video.twimg.com/high.mp4"}]}}]);
    t
}
#[test]
fn type_filters_keep_all_metadata_and_choose_best_mp4_for_video_and_gif() {
    for filter in [Filter::Image, Filter::Video, Filter::All] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut s = Session::default();
        let mut n = network(
            &mut s,
            vec![page(
                vec![
                    tweet(100, 10),
                    video(101, "video"),
                    video(102, "animated_gif"),
                ],
                None,
            )],
            Media::default(),
        );
        let mut opts = options(Mode::Full);
        opts.filter = filter;
        let p = task(&mut store, &opts, &cancel, &mut n);
        assert_eq!(p.outcome, "completed");
        assert_eq!(storage::status(dir.path()).unwrap().tweets, 3);
        let expected = match filter {
            Filter::Image => 1,
            Filter::Video => 2,
            Filter::All => 3,
        };
        assert_eq!(n.media.calls.len(), expected);
        assert!(
            !n.media
                .calls
                .iter()
                .any(|u| u.contains("low.mp4") || u.contains("m3u8"))
        );
        if filter != Filter::Video {
            assert!(
                n.media
                    .calls
                    .iter()
                    .any(|u| u.contains("name=orig") && !u.contains("name=small"))
            );
        }
        let saved: u32 = store
            .connection()
            .query_row("SELECT count(*) FROM media_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(saved as usize, expected);
    }
}
#[test]
fn ordinary_timeline_is_opt_in_and_stores_text_only_tweets() {
    for include in [false, true] {
        let dir = directory();
        let mut store = Store::open(dir.path()).unwrap();
        let cancel = AtomicBool::new(false);
        let mut s = Session::default();
        let mut text = tweet(99, 9);
        text["legacy"]["extended_entities"]["media"] = json!([]);
        let mut replies = vec![page(vec![tweet(100, 10)], None)];
        if include {
            replies.push(page(vec![text], Some("timeline-next")));
            replies.push(page(vec![], None));
        }
        let mut n = network(&mut s, replies, Media::default());
        let mut opts = options(Mode::Full);
        opts.include_timeline = include;
        let p = task(&mut store, &opts, &cancel, &mut n);
        assert_eq!(p.outcome, "completed");
        assert_eq!(
            n.requests.transport.calls.len(),
            if include { 3 } else { 1 }
        );
        assert_eq!(
            storage::status(dir.path()).unwrap().tweets,
            if include { 2 } else { 1 }
        );
        if include {
            assert_eq!(n.requests.transport.calls[1].0, "UserTweets");
            assert_eq!(
                n.requests.transport.calls[2].1.as_deref(),
                Some("timeline-next")
            );
            assert!(
                checkpoint(&store, "1", "full:UserTweets:all")
                    .unwrap()
                    .completed_at
                    .is_some()
            );
        }
    }
}
#[test]
fn prior_imported_filename_source_is_reused_without_new_source() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let a = target();
    store
        .save_media(
            &MediaSource {
                sequence: None,
                target: user(&a),
                author: user(&a),
                tweet_id: "100",
                text: Some("旧数据"),
                published_at: None,
                media_key: "legacy-filename:100_00.jpg",
                original_filename: "100_00.jpg",
                original_url: "https://pbs.twimg.com/media/100.jpg?name=orig",
                media_type: "image",
                mime_type: "image/jpeg",
            },
            &b"imported"[..],
        )
        .unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10)], None)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "completed");
    assert!(n.media.calls.is_empty());
    assert_eq!(storage::status(dir.path()).unwrap().sources, 1);
}
#[test]
fn pinned_old_tweet_does_not_terminate_incremental_before_newer_pages() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![page(vec![tweet(100, 10), tweet(90, 1)], None)],
        Media::default(),
    );
    task(&mut store, &options(Mode::Full), &cancel, &mut n);
    let mut first = page(vec![tweet(100, 10), tweet(101, 11)], Some("a"));
    let mut payload: Value = serde_json::from_slice(&first.body).unwrap();
    payload["data"]["user"]["result"]["timeline_v2"]["timeline"]["instructions"].as_array_mut().unwrap().push(json!({"type":"TimelinePinEntry","entry":{"content":{"itemContent":{"tweet_results":{"result":tweet(90,1)}}}}}));
    first.body = serde_json::to_vec(&payload).unwrap();
    let mut second = page(vec![tweet(102, 12)], Some("b"));
    let mut payload: Value = serde_json::from_slice(&second.body).unwrap();
    payload["data"]["user"]["result"]["timeline_v2"]["timeline"]["instructions"].as_array_mut().unwrap().push(json!({"type":"TimelinePinEntry","entry":{"content":{"itemContent":{"tweet_results":{"result":tweet(90,1)}}}}}));
    second.body = serde_json::to_vec(&payload).unwrap();
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![first, second, page(vec![tweet(103, 13)], None)],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Incremental), &cancel, &mut n);
    assert_eq!(p.outcome, "updated");
    assert_eq!(p.pages, 3);
    assert_eq!(storage::status(dir.path()).unwrap().tweets, 5);
}
#[test]
fn v1_migration_preserves_data_and_takes_readable_backup_before_change() {
    let dir = directory();
    let db = dir.path().join("archive.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(include_str!("../storage/schema.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO users(user_id,screen_name) VALUES ('1','kept')",
        [],
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&db).unwrap();
    assert_eq!(storage::status(dir.path()).unwrap().pending, 0);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        store
            .connection()
            .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        6
    );
    let backups: Vec<_> = std::fs::read_dir(dir.path().join("temporary"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    assert_ne!(before, std::fs::read(&db).unwrap());
    let backup = rusqlite::Connection::open_with_flags(
        &backups[0],
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        backup
            .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        backup
            .query_row("SELECT screen_name FROM users WHERE user_id='1'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT screen_name FROM users WHERE user_id='1'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "kept"
    );
}

#[test]
fn cursor_failure_after_committed_pages_clears_old_cycle_detection_on_fallback() {
    let dir = directory();
    let mut store = Store::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut s = Session::default();
    let mut n = network(
        &mut s,
        vec![
            page(vec![tweet(102, 12)], Some("a")),
            status(400),
            page(vec![tweet(102, 12)], Some("a")),
            page(vec![tweet(101, 11)], None),
        ],
        Media::default(),
    );
    let p = task(&mut store, &options(Mode::Full), &cancel, &mut n);
    assert_eq!(p.outcome, "completed");
    assert_eq!(p.skipped, 1);
    assert_eq!(n.media.calls.len(), 2);
    assert_eq!(storage::status(dir.path()).unwrap().sources, 2);
}

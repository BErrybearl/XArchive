//! Cross-module integration tests, included by archive under cfg(test) so test-only
//! endpoints and credentials cannot become part of the production library API.
mod server;
use super::*;
use crate::{export, platform::proxy::ProxyPlan};
use server::{Server, media_bytes};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
enum Route {
    Direct,
    ExplicitProxy,
    SystemBypass,
}
struct Fixture {
    server: Server,
    _directory: tempfile::TempDir,
    root: PathBuf,
    output: PathBuf,
    route: Route,
}
impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".runtime/e2e-tests");
        fs::create_dir_all(&parent).unwrap();
        let directory = tempfile::tempdir_in(parent).unwrap();
        let root = directory.path().join("data");
        let output = directory.path().join("export");
        fs::create_dir(&output).unwrap();
        Self {
            server: Server::start(),
            _directory: directory,
            root,
            output,
            route: Route::Direct,
        }
    }
    fn options(&self, mode: Mode) -> Options {
        Options {
            target: "fixture".into(),
            mode,
            filter: Filter::All,
            include_timeline: true,
            preferences: Preferences {
                retry_limit: 2,
                ..Default::default()
            },
        }
    }
    fn run(&self, session: &mut Session, mode: Mode) -> Progress {
        self.run_notified(session, mode, &AtomicBool::new(false), |_| {})
    }
    fn run_notified(
        &self,
        session: &mut Session,
        mode: Mode,
        cancel: &AtomicBool,
        notify: impl FnMut(Progress),
    ) -> Progress {
        let plan = match self.route {
            Route::Direct => ProxyPlan::for_test(None, ""),
            Route::ExplicitProxy => ProxyPlan::resolve(self.server.base.as_str()).unwrap(),
            // If bypass is broken, this deliberately closed proxy port fails the request.
            Route::SystemBypass => ProxyPlan::for_test(Some("http://127.0.0.1:1"), "127.0.0.1"),
        };
        run_with(
            &self.root,
            session,
            &self.options(mode),
            cancel,
            notify,
            Duration::ZERO,
            || {
                let mut params = params::defaults();
                params.public_bearer = "synthetic_public_bearer".into();
                let login = PlatformLogin {
                    auth_token: Zeroizing::new("synthetic_auth".into()),
                    ct0: Zeroizing::new("synthetic_csrf".into()),
                    account: Account {
                        user_id: "90000".into(),
                        screen_name: "viewer".into(),
                        display_name: "Synthetic viewer".into(),
                    },
                    proxy: String::new(),
                };
                Ok((
                    login,
                    Http::for_test(plan.clone(), self.server.base.clone())?,
                    HttpMedia::for_test(plan, self.server.base.clone())?,
                    params,
                ))
            },
        )
    }
    fn store(&self) -> Store {
        Store::open(&self.root).unwrap()
    }
    fn count(&self, query: &str) -> u64 {
        let count: i64 = self
            .store()
            .connection()
            .query_row(query, [], |r| r.get(0))
            .unwrap();
        count.try_into().unwrap()
    }
    fn checkpoint(&self, op: &str) -> Checkpoint {
        checkpoint(
            &self.store(),
            "1",
            &format!("full:{op}:{}", self.options(Mode::Full).scope_key()),
        )
        .unwrap()
    }
    fn cursors(&self, op: &str) -> Vec<Option<String>> {
        self.server
            .requests()
            .iter()
            .filter(|r| r.operation() == op)
            .map(|r| r.cursor())
            .collect()
    }
    fn cdn_count(&self, path: &str) -> usize {
        self.server
            .requests()
            .iter()
            .filter(|r| r.url.path() == path)
            .count()
    }
    fn assert_counts(&self, tweets: u64, files: u64, pending: u64) {
        let status = storage::status(&self.root).unwrap();
        assert_eq!(
            (status.tweets, status.media, status.sources, status.pending),
            (tweets, files, files, pending)
        );
        assert_eq!(
            fs::read_dir(self.root.join("media")).unwrap().count() as u64,
            files
        );
        let report = self.store().inspect().unwrap();
        assert!(report.missing_files.is_empty(), "{report:?}");
        assert!(report.changed_files.is_empty(), "{report:?}");
        assert!(report.untracked_files.is_empty(), "{report:?}");
        assert_eq!(report.temporary_files, 0);
    }
    fn export(&self, mode: export::Mode) -> export::Progress {
        export::run(
            &self.root,
            &self.output,
            &export::Options {
                account: Some("1".into()),
                mode,
                media_types: MediaTypes::default(),
                date_start: None,
                date_end: None,
                output_directory: None,
            },
            &AtomicBool::new(false),
            |_| {},
        )
    }
}
fn outcome(progress: &Progress, expected: &str) {
    assert_eq!(progress.outcome, expected, "{progress:?}");
    if expected != "rate_limit" {
        assert!(progress.error.is_none(), "{progress:?}");
    }
}
fn complete(f: &Fixture, session: &mut Session) -> Progress {
    let p = f.run(session, Mode::Full);
    outcome(&p, "completed");
    assert!(p.exhausted);
    f.assert_counts(8, 5, 0);
    p
}

#[test]
fn full_archive_and_incremental_use_real_http_and_bound_the_empty_tail() {
    let f = Fixture::new();
    let mut session = Session::default();
    let p = complete(&f, &mut session);
    assert_eq!((p.pages, p.tweets, p.added, p.failed), (14, 10, 5, 0));
    assert_eq!(p.rate_limits["UserMedia"].remaining, Some(99));
    assert!(p.rate_limits["UserMedia"].reset_at.unwrap() > platform::client::now());
    let mut expected = vec![None, Some("m2".into())];
    expected.extend((0..10).map(|n| Some(format!("tail-{n}"))));
    assert_eq!(f.cursors("UserMedia"), expected);
    assert_eq!(f.cursors("UserTweets"), vec![None, Some("t2".into())]);
    for op in ["UserMedia", "UserTweets"] {
        let cp = f.checkpoint(op);
        assert!(cp.exhausted && cp.completed_at.is_some());
        assert_eq!(
            cp.completed_newest,
            Some(date_seconds("2026-10-13").unwrap() + 43200)
        );
    }
    assert_eq!(f.cdn_count("/cdn/high.mp4"), 1);
    assert_eq!(f.cdn_count("/cdn/low.mp4"), 0);
    assert_eq!(f.cdn_count("/cdn/playlist.m3u8"), 0);
    assert_eq!(f.cdn_count("/cdn/pinned.jpg"), 1);
    assert_eq!(
        f.count("SELECT count(*) FROM media_sources s JOIN tweets t ON t.tweet_id=s.tweet_id WHERE s.tweet_id='180' AND t.author_user_id='2'"),
        1
    );
    for r in f
        .server
        .requests()
        .iter()
        .filter(|r| r.url.path().ends_with(".jpg"))
    {
        assert!(r.url.query_pairs().any(|(k, v)| k == "name" && v == "orig"));
    }
    let completed_at = f.checkpoint("UserMedia").completed_at;
    f.server.clear_requests();
    let quiet = f.run(&mut session, Mode::Incremental);
    outcome(&quiet, "updated");
    assert_eq!((quiet.pages, quiet.added, quiet.reused), (4, 0, 0));
    assert_eq!(f.cursors("UserMedia"), vec![None, Some("m2".into())]);
    assert_eq!(f.cursors("UserTweets"), vec![None, Some("t2".into())]);
    assert!(
        f.server
            .requests()
            .iter()
            .all(|r| !r.url.path().starts_with("/cdn/"))
    );
    assert_eq!(f.checkpoint("UserMedia").completed_at, completed_at);
    f.assert_counts(8, 5, 0);

    f.server.state.lock().unwrap().new_tweet = true;
    f.server.clear_requests();
    let updated = f.run(&mut session, Mode::Incremental);
    outcome(&updated, "updated");
    assert_eq!((updated.pages, updated.added), (4, 1));
    assert_eq!(f.cdn_count("/cdn/new.jpg"), 1);
    assert_eq!(
        f.count("SELECT count(*) FROM archive_tweets WHERE tweet_id='400'"),
        1
    );
    f.assert_counts(9, 6, 0);
}

#[test]
fn stopping_after_a_committed_page_resumes_from_the_durable_cursor() {
    let f = Fixture::new();
    let cancel = AtomicBool::new(false);
    let first = f.run_notified(&mut Session::default(), Mode::Full, &cancel, |p| {
        if p.pages == 1 {
            cancel.store(true, Ordering::Relaxed);
        }
    });
    outcome(&first, "stopped");
    assert_eq!(first.pages, 1);
    let cp = f.checkpoint("UserMedia");
    assert_eq!(cp.cursor.as_deref(), Some("m2"));
    assert!(!cp.exhausted && cp.completed_at.is_none());
    f.assert_counts(3, 3, 0);
    f.server.clear_requests();
    // New session and new Store connection simulate a process restart, no in-memory checkpoint.
    let resumed = f.run(&mut Session::default(), Mode::Full);
    outcome(&resumed, "completed");
    assert_eq!(f.cursors("UserMedia").first(), Some(&Some("m2".into())));
    assert!(!f.cursors("UserMedia").contains(&None));
    assert_eq!(f.cdn_count("/cdn/high.mp4"), 0);
    assert_eq!(resumed.added, 2);
    f.assert_counts(8, 5, 0);
}

#[test]
fn http_429_saves_checkpoint_and_blocks_requests_until_the_reset() {
    let f = Fixture::new();
    f.server.state.lock().unwrap().rate_on_second = true;
    let mut session = Session::default();
    let limited = f.run(&mut session, Mode::Full);
    outcome(&limited, "rate_limit");
    assert_eq!(
        limited.error.as_ref().unwrap().category,
        Category::RateLimit
    );
    assert_eq!(limited.pages, 1);
    assert_eq!(f.cursors("UserMedia"), vec![None, Some("m2".into())]);
    let reset = f.server.state.lock().unwrap().reset_at;
    assert_eq!(limited.error.as_ref().unwrap().reset_at, Some(reset));
    assert_eq!(session.limits["UserMedia"].remaining, Some(0));
    assert_eq!(session.limits["UserMedia"].reset_at, Some(reset));
    assert_eq!(f.checkpoint("UserMedia").cursor.as_deref(), Some("m2"));
    assert!(f.checkpoint("UserMedia").completed_at.is_none());
    f.assert_counts(3, 3, 0);
    f.server.clear_requests();
    let blocked = f.run(&mut session, Mode::Full);
    outcome(&blocked, "rate_limit");
    assert!(f.cursors("UserMedia").is_empty());
    assert_eq!(f.checkpoint("UserMedia").cursor.as_deref(), Some("m2"));
    // Honor the real header's Unix reset time; do not edit Session or the checkpoint.
    while platform::client::now() < reset {
        std::thread::sleep(Duration::from_millis(20));
    }
    f.server.clear_requests();
    let resumed = f.run(&mut session, Mode::Full);
    outcome(&resumed, "completed");
    assert_eq!(f.cursors("UserMedia").first(), Some(&Some("m2".into())));
    assert!(f.checkpoint("UserMedia").completed_at.is_some());
    f.assert_counts(8, 5, 0);
}

#[test]
fn failed_cdn_downloads_retry_then_export_and_preserve_modified_files() {
    let f = Fixture::new();
    f.server.state.lock().unwrap().faults = true;
    let mut session = Session::default();
    let first = f.run(&mut session, Mode::Full);
    outcome(&first, "incomplete");
    assert_eq!((first.pending, first.failed), (3, 0));
    let truncated = first
        .failures
        .iter()
        .find(|failure| failure.tweet_id == "250")
        .unwrap();
    assert_eq!(truncated.attempts, 1);
    assert_eq!(
        truncated.reason,
        download::DownloadError::EmptyOrTruncated.message()
    );
    assert!(f.checkpoint("UserMedia").completed_at.is_none());
    f.assert_counts(11, 5, 3);
    assert_eq!(f.cdn_count("/cdn/landing.jpg"), 0);
    let second = f.run(&mut session, Mode::Full);
    outcome(&second, "completed");
    assert_eq!((second.added, second.pending, second.failed), (1, 0, 2));
    for (id, reason) in [
        ("260", download::DownloadError::Http),
        ("240", download::DownloadError::Redirect),
    ] {
        let failure = second
            .failures
            .iter()
            .find(|failure| failure.tweet_id == id)
            .unwrap();
        assert!(failure.terminal);
        assert_eq!(failure.attempts, 2);
        assert_eq!(failure.reason, reason.message());
    }
    assert_eq!(
        f.count("SELECT count(*) FROM archive_retries WHERE status='failed'"),
        2
    );
    for path in [
        "/cdn/missing.jpg",
        "/cdn/redirect.jpg",
        "/cdn/truncated.jpg",
    ] {
        assert_eq!(f.cdn_count(path), 2);
    }
    assert_eq!(f.cdn_count("/cdn/landing.jpg"), 0);
    f.assert_counts(11, 6, 0);

    let requests_before_export = f.server.requests().len();
    let full = f.export(export::Mode::Full);
    assert_eq!(full.outcome, "completed", "{full:?}");
    assert_eq!(
        (full.copied, full.accounts.len(), full.accounts[0].indexed),
        (6, 1, 6)
    );
    let directory = Path::new(&full.accounts[0].directory);
    let entries: Vec<export::IndexEntry> =
        serde_json::from_slice(&fs::read(directory.join("index.json")).unwrap()).unwrap();
    assert_eq!(entries.len(), 6);
    assert_eq!(fs::read_dir(directory.join("media")).unwrap().count(), 6);
    let csv = fs::read_to_string(directory.join("index.csv")).unwrap();
    assert_eq!(csv.lines().count(), 7);
    assert!(csv.starts_with('\u{feff}'));
    for entry in &entries {
        let bytes = fs::read(directory.join(&entry.path)).unwrap();
        assert_eq!(bytes.len() as u64, entry.byte_length);
        assert_eq!(storage::hex_digest(&Sha256::digest(&bytes)), entry.sha256);
        assert_eq!(entry.sources.len(), 1);
        let url = url::Url::parse(&entry.sources[0].original_url).unwrap();
        assert_eq!(bytes, media_bytes(url.path()));
        assert!(csv.contains(&entry.path));
        assert!(csv.contains(&entry.sha256));
    }
    let original = entries
        .iter()
        .find(|entry| entry.sources[0].tweet_id == "180")
        .unwrap();
    assert_eq!(original.sources[0].author_id, "2");
    let changed = &entries[0];
    let path = directory.join(&changed.path);
    let mut edited = fs::read(&path).unwrap();
    edited[0] ^= 1; // Same length: prove digest checks, not just byte length checks.
    fs::write(&path, &edited).unwrap();
    let incremental = f.export(export::Mode::Incremental);
    assert_eq!(
        incremental.outcome, "completed_with_issues",
        "{incremental:?}"
    );
    assert_eq!(incremental.copied, 0);
    assert_eq!(
        incremental.accounts[0].directory,
        full.accounts[0].directory
    );
    assert_eq!(incremental.accounts[0].indexed, 5);
    assert_eq!(incremental.issues.len(), 1);
    assert_eq!(incremental.issues[0].reason, "modified");
    assert_eq!(fs::read(path).unwrap(), edited);
    let retained: Vec<export::IndexEntry> =
        serde_json::from_slice(&fs::read(directory.join("index.json")).unwrap()).unwrap();
    assert_eq!(retained.len(), 5);
    assert!(retained.iter().all(|entry| entry.path != changed.path));
    assert_eq!(
        fs::read_to_string(directory.join("index.csv"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    assert_eq!(fs::read_dir(directory.join("media")).unwrap().count(), 6);
    assert_eq!(f.server.requests().len(), requests_before_export);
    f.assert_counts(11, 6, 0);
}

#[test]
fn explicit_proxy_routes_api_and_cdn_through_real_reqwest() {
    let mut f = Fixture::new();
    f.route = Route::ExplicitProxy;
    complete(&f, &mut Session::default());
    let requests = f.server.requests();
    assert!(requests.iter().any(|r| r.url.path().starts_with("/cdn/")));
    assert!(
        requests
            .iter()
            .all(|r| r.target.starts_with(f.server.base.as_str()))
    );
}

#[test]
fn system_proxy_bypass_routes_api_and_cdn_directly() {
    let mut f = Fixture::new();
    f.route = Route::SystemBypass;
    complete(&f, &mut Session::default());
    assert!(
        f.server
            .requests()
            .iter()
            .all(|r| r.target.starts_with('/'))
    );
}

#[test]
fn local_media_allowlist_is_per_client_and_production_policy_stays_strict() {
    let f = Fixture::new();
    let media = HttpMedia::for_test(ProxyPlan::for_test(None, ""), f.server.base.clone()).unwrap();
    let local = f.server.base.join("cdn/image.jpg").unwrap();
    assert!(media.url(local.as_str()).is_ok());
    assert_eq!(
        download::allowed(local.as_str()),
        Err(download::DownloadError::UnsafeUrl)
    );
    assert!(download::allowed("https://pbs.twimg.com/media/image.jpg").is_ok());
    assert!(download::allowed("https://video.twimg.com/video.mp4").is_ok());
    for raw in [
        "http://pbs.twimg.com/image.jpg",
        "https://pbs.twimg.com.evil.invalid/image.jpg",
        "https://pbs.twimg.com:444/image.jpg",
        "https://user@pbs.twimg.com/image.jpg",
        "https://pbs.twimg.com/image.jpg#fragment",
    ] {
        assert_eq!(
            download::allowed(raw),
            Err(download::DownloadError::UnsafeUrl)
        );
        assert_eq!(media.url(raw), Err(download::DownloadError::UnsafeUrl));
    }
    let mut other_port = local.clone();
    other_port
        .set_port(Some(if local.port() == Some(1) { 2 } else { 1 }))
        .unwrap();
    assert_eq!(
        media.url(other_port.as_str()),
        Err(download::DownloadError::UnsafeUrl)
    );
    let mut credentials = local.clone();
    credentials.set_username("user").unwrap();
    assert_eq!(
        media.url(credentials.as_str()),
        Err(download::DownloadError::UnsafeUrl)
    );
    assert!(f.server.requests().is_empty());
}

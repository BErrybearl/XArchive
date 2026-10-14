//! One-account archive engine. The Tauri coordinator owns the shared storage lock.
mod download;
mod options;
mod state;
mod transfer;
use crate::{
    auth::{self, Account, PlatformLogin},
    platform::{
        self, Category, PlatformError,
        client::{Http, RateLimit, Requests, Session, Transport},
        params::{self, Params},
        parser::{self, ArchivePage, ArchivedTweet, Attachment},
    },
    storage::{self, MediaSource, Store, User},
};
use download::{HttpMedia, MediaTransport};
pub use options::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use state::*;
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
type Result<T> = platform::error::Result<T>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Full,
    Incremental,
    Retry,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    All,
    Image,
    Video,
}
impl Filter {
    fn accepts(self, kind: &str) -> bool {
        match self {
            Self::All => true,
            Self::Image => kind == "image",
            Self::Video => kind == "video" || kind == "gif",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Image => "image",
            Self::Video => "video",
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Options {
    pub target: String,
    pub mode: Mode,
    pub filter: Filter,
    pub include_timeline: bool,
    #[serde(flatten)]
    pub preferences: Preferences,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub stage: String,
    pub target: Option<Account>,
    pub pages: u64,
    pub page_item: u64,
    pub page_total: u64,
    pub current_item: String,
    pub bytes: u64,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub date_ended: bool,
    #[serde(skip)]
    pub(crate) last_push: Option<Instant>,
    pub tweets: u64,
    pub added: u64,
    pub reused: u64,
    pub existing: u64,
    pub skipped: u64,
    pub failed: u64,
    pub pending: u64,
    pub exhausted: bool,
    pub outcome: String,
    pub summary: String,
    pub rate_limits: BTreeMap<String, RateLimit>,
    pub logs: Vec<String>,
    pub failures: Vec<Failure>,
    pub error: Option<PlatformError>,
    #[serde(skip)]
    pub(crate) repair_logged: HashSet<(String, String)>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub tweet_id: String,
    pub kind: String,
    pub attempts: u32,
    pub reason: String,
    pub terminal: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Retry {
    tweet: ArchivedTweet,
    media: Attachment,
}

pub fn user(account: &Account) -> User<'_> {
    User {
        id: &account.user_id,
        screen_name: &account.screen_name,
        display_name: Some(&account.display_name),
    }
}
fn stopped(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}
fn push(p: &mut Progress, force: bool, notify: &mut impl FnMut(Progress)) {
    if force
        || p.last_push
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(150))
    {
        p.last_push = Some(Instant::now());
        notify(p.clone());
    }
}
fn append_log(p: &mut Progress, message: impl Into<String>) {
    let now = platform::client::now();
    p.logs.push(format!(
        "{:02}:{:02}:{:02} {}",
        (now / 3600 + 8) % 24,
        now / 60 % 60,
        now % 60,
        message.into()
    ));
    if p.logs.len() > 200 {
        p.logs.remove(0);
    }
}
fn log(p: &mut Progress, message: impl Into<String>, notify: &mut impl FnMut(Progress)) {
    append_log(p, message);
    push(p, true, notify);
}
fn media_log(
    p: &mut Progress,
    message: impl Into<String>,
    error: bool,
    notify: &mut impl FnMut(Progress),
) {
    append_log(p, message);
    push(p, error, notify);
}
trait Backend {
    fn page(&mut self, op: &str, target: &str, cursor: Option<&str>) -> Result<ArchivePage>;
    fn media(
        &mut self,
        item: &Attachment,
    ) -> std::result::Result<Box<dyn std::io::Read>, download::DownloadError>;
    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        None
    }
    fn rates(&self) -> BTreeMap<String, RateLimit>;
}
struct Network<'a, T, M> {
    requests: Requests<'a, T>,
    media: M,
    params: Params,
    login: PlatformLogin,
}
impl<T: Transport, M: MediaTransport> Backend for Network<'_, T, M> {
    fn page(&mut self, op: &str, target: &str, cursor: Option<&str>) -> Result<ArchivePage> {
        let mut variables = json!({"userId":target,"count":20,"includePromotedContent":false,"withClientEventToken":false,"withBirdwatchNotes":false,"withVoice":true,"withV2Timeline":true});
        if op == "UserTweets" {
            variables["withQuickPromoteEligibilityTweetFields"] = json!(true);
        }
        if let Some(cursor) = cursor {
            variables["cursor"] = json!(cursor);
        }
        parser::archive_page(
            &self
                .requests
                .query(op, variables, &self.params, &self.login)?,
        )
    }
    fn media(
        &mut self,
        item: &Attachment,
    ) -> std::result::Result<Box<dyn std::io::Read>, download::DownloadError> {
        self.media.open(
            self.media.url(&item.url)?,
            reqwest::header::HeaderMap::new(),
        )
    }
    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        self.media.worker()
    }
    fn rates(&self) -> BTreeMap<String, RateLimit> {
        self.requests.session.limits.clone()
    }
}
fn exhausted_rate(backend: &impl Backend) -> Option<PlatformError> {
    backend
        .rates()
        .values()
        .find(|r| {
            r.remaining == Some(0)
                && r.reset_at
                    .is_none_or(|reset| reset > platform::client::now())
        })
        .map(|r| PlatformError::rate(r.reset_at))
}

pub fn run(
    root: &Path,
    session: &mut Session,
    options: &Options,
    cancel: &AtomicBool,
    notify: impl FnMut(Progress),
) -> Progress {
    if options.mode == Mode::Retry {
        return run_retries(root, options, cancel, notify);
    }
    run_with(
        root,
        session,
        options,
        cancel,
        notify,
        options.preferences.request_delay(),
        || {
            let login = auth::platform_login(root).map_err(|e| match e {
                auth::AuthError::Authentication => PlatformError::new(Category::Authentication),
                _ => PlatformError::local(),
            })?;
            let http = Http::new(&login.proxy)?;
            let media = HttpMedia::new(&login.proxy)?;
            Ok((login, http, media, params::load(root).0))
        },
    )
}

// The same login checks, traversal, persistence and finish path serve production and E2E.
fn run_with(
    root: &Path,
    session: &mut Session,
    options: &Options,
    cancel: &AtomicBool,
    mut notify: impl FnMut(Progress),
    delay: Duration,
    connect: impl FnOnce() -> Result<(PlatformLogin, Http, HttpMedia, Params)>,
) -> Progress {
    for rate in session.limits.values_mut() {
        rate.ensure_reset();
    }
    let mut p = Progress {
        running: true,
        stage: "检查登录与目标账号".into(),
        started_at: platform::client::now(),
        target: Some(Account {
            user_id: String::new(),
            screen_name: parser::target_name(&options.target).unwrap_or_default(),
            display_name: String::new(),
        }),
        ..Progress::default()
    };
    notify(p.clone());
    let result = (|| -> Result<()> {
        options.validate().map_err(|_| PlatformError::local())?;
        parser::target_name(&options.target)?;
        let (login, http, media, params) = connect()?;
        session.for_account(&login.account.user_id);
        let mut network = Network {
            requests: Requests {
                transport: http,
                session,
                delay,
            },
            media,
            params,
            login,
        };
        if stopped(cancel) {
            p.outcome = "stopped".into();
            return Ok(());
        }
        if network.params.public_bearer.is_empty() {
            let found = network.requests.discover(Some(&network.login), true)?;
            network.params.query_ids.extend(found.ids);
            network.params.public_bearer = found.bearer.ok_or_else(PlatformError::interface)?;
        }
        if stopped(cancel) {
            p.outcome = "stopped".into();
            return Ok(());
        }
        let viewer = parser::viewer(&network.requests.query(
            "Viewer",
            json!({}),
            &network.params,
            &network.login,
        )?)?;
        if viewer.user_id != network.login.account.user_id {
            return Err(PlatformError::new(Category::Authentication));
        }
        if let Some(error) = exhausted_rate(&network) {
            return Err(error);
        }
        if stopped(cancel) {
            p.outcome = "stopped".into();
            return Ok(());
        }
        let target=parser::target(&network.requests.query("UserByScreenName",json!({"screen_name":parser::target_name(&options.target)?,"withSafetyModeUserFields":true}),&network.params,&network.login)?)?;
        p.target = Some(target.clone());
        if let Some(error) = exhausted_rate(&network) {
            return Err(error);
        }
        let mut store = Store::open(root).map_err(|_| PlatformError::local())?;
        store
            .bind_account(
                &target,
                &Settings {
                    target: target.screen_name.clone(),
                    include_timeline: options.include_timeline,
                    preferences: options.preferences.clone(),
                },
            )
            .map_err(|_| PlatformError::local())?;
        let result = execute(
            &mut store,
            &target,
            options,
            cancel,
            &mut network,
            &mut p,
            &mut notify,
        );
        p.rate_limits = network.rates();
        result
    })();
    p.rate_limits = session.limits.clone();
    finish(&mut p, result, cancel);
    notify(p.clone());
    p
}
fn finish(p: &mut Progress, result: Result<()>, cancel: &AtomicBool) {
    p.running = false;
    p.finished_at = Some(platform::client::now());
    if let Err(e) = result {
        p.outcome = if e.category == Category::RateLimit {
            "rate_limit"
        } else if e.category == Category::Authentication {
            "authentication"
        } else {
            "incomplete"
        }
        .into();
        p.error = Some(e);
    } else if stopped(cancel) {
        p.outcome = "stopped".into();
    }
    if p.outcome.is_empty() {
        p.outcome = "incomplete".into();
    }
    p.stage = match p.outcome.as_str() {
        "retried" => "待重试处理完成",
        "date_range" => "按日期范围结束",
        "completed" => "全量完成（所选范围）",
        "updated" => "增量扫描完成",
        "stopped" => "已停止",
        "rate_limit" => "限流停止",
        "authentication" => "认证失效，已停止",
        _ => "归档未完成",
    }
    .into();
    if p.failed > 0 && matches!(p.outcome.as_str(), "completed" | "updated" | "retried") {
        p.stage = format!("{}（{} 项失败）", p.stage, p.failed);
    }
    p.summary = format!(
        "{}；处理 {} 页、{} 条推文，新增 {} 份媒体、复用 {} 份、跳过 {} 条已有来源；待重试 {}，已失败 {}。",
        p.stage, p.pages, p.tweets, p.added, p.reused, p.skipped, p.pending, p.failed
    );
    if p.date_ended && p.outcome != "date_range" {
        p.summary.push_str(" 按日期范围结束扫描。");
    }
    append_log(p, p.summary.clone());
}
fn refresh_failures(
    store: &Store,
    target: &Account,
    options: &Options,
    p: &mut Progress,
) -> Result<()> {
    p.failures = failures(store, &target.user_id, options)?;
    p.pending = p.failures.iter().filter(|f| !f.terminal).count() as u64;
    p.failed = p.failures.iter().filter(|f| f.terminal).count() as u64;
    Ok(())
}

fn execute(
    store: &mut Store,
    target: &Account,
    options: &Options,
    cancel: &AtomicBool,
    backend: &mut impl Backend,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<()> {
    p.target = Some(target.clone());
    options.validate().map_err(|_| PlatformError::local())?;
    if options.mode != Mode::Retry {
        normalize_retries(store, &target.user_id, options.preferences.retry_limit)?;
    } else {
        store.connection().execute("UPDATE archive_retries SET status='failed' WHERE target_user_id=?1 AND status='pending' AND attempts>=?2",rusqlite::params![target.user_id,options.preferences.retry_limit]).map_err(|_|PlatformError::local())?;
    }
    let result = execute_inner(store, target, options, cancel, backend, p, notify);
    // Always report the durable retry state, including on an API error or user stop.
    let failures = refresh_failures(store, target, options, p);
    p.rate_limits = backend.rates();
    if options.mode != Mode::Retry {
        record_run(
            store,
            target,
            options,
            p,
            result.is_ok() && !stopped(cancel),
        )?;
    }
    result.and(failures)
}
fn execute_inner(
    store: &mut Store,
    target: &Account,
    options: &Options,
    cancel: &AtomicBool,
    backend: &mut impl Backend,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<()> {
    let mut attempted = HashSet::new();
    if let Some(e) = exhausted_rate(backend) {
        return Err(e);
    }
    p.stage = "处理待重试媒体".into();
    log(p, "检查并补齐待重试媒体", notify);
    let pending = retries(store, &target.user_id)?;
    p.page_total = pending.len() as u64;
    p.page_item = 0;
    let mut staged = Vec::new();
    for item in pending {
        if options.mode != Mode::Retry
            && (!options.accepts(&item.media.kind)
                || (!options.preferences.include_retweets && item.tweet.retweet)
                || !options.preferences.within_dates(&item.tweet))
        {
            continue;
        }
        attempted.insert((item.tweet.id.clone(), item.media.key.clone()));
        let mut item = item;
        item.media = options.preferences.select_quality(&item.media);
        staged.push(item);
    }
    transfer::download_batch(store, target, staged, options, cancel, backend, p, notify)?;
    if options.mode == Mode::Retry {
        refresh_failures(store, target, options, p)?;
        p.outcome = if p.pending + p.failed == 0 {
            "retried"
        } else {
            "incomplete"
        }
        .into();
        return Ok(());
    }
    let operations = if options.include_timeline {
        vec!["UserMedia", "UserTweets"]
    } else {
        vec!["UserMedia"]
    };
    let mut completed_modes = Vec::new();
    let mut all_exhausted = true;
    let mut date_ended = false;
    for op in operations {
        let full_mode = format!("full:{op}:{}", options.scope_key());
        let mode = format!(
            "{}:{op}:{}",
            if options.mode == Mode::Full {
                "full"
            } else {
                "incremental"
            },
            options.scope_key()
        );
        let base = checkpoint(store, &target.user_id, &full_mode)?;
        let cutoff = base.completed_newest;
        let mut cp = if options.mode == Mode::Full {
            base
        } else {
            Checkpoint::default()
        };
        if options.mode == Mode::Full && cp.exhausted {
            cp.cursor = None;
            cp.exhausted = false;
            cp.newest = None;
        }
        save_checkpoint(store, target, &mode, &cp)?;
        let mut fallback = false;
        let mut cursors = HashSet::new();
        let mut quiet = 0;
        let mut page_number = 0;
        let mut overlap_page = None;
        let mut empty_pages = 0;
        // A resumed cursor already follows previously saved entries.
        let mut seen_entries = cp.cursor.is_some();
        while !cp.exhausted {
            if stopped(cancel) {
                return Ok(());
            }
            if let Some(e) = exhausted_rate(backend) {
                return Err(e);
            }
            p.stage = if fallback {
                "正在跳过已归档部分".into()
            } else {
                format!("扫描 {op}")
            };
            p.rate_limits = backend.rates();
            push(p, false, notify);
            let page = backend.page(op, &target.user_id, cp.cursor.as_deref());
            let page = match page {
                Err(e)
                    if options.mode == Mode::Full
                        && cp.cursor.is_some()
                        && !fallback
                        && e.category == Category::InterfaceChanged =>
                {
                    cp.cursor = None;
                    cp.exhausted = false;
                    save_checkpoint(store, target, &mode, &cp)?;
                    fallback = true;
                    cursors.clear();
                    empty_pages = 0;
                    seen_entries = false;
                    log(p, "游标已失效，从首页重新扫描；正在跳过已归档部分", notify);
                    continue;
                }
                Err(e) => return Err(e),
                Ok(page) => page,
            };
            // A repeated/nonadvancing cursor is never evidence of reaching the end.
            if page
                .next
                .as_ref()
                .is_some_and(|next| Some(next) == cp.cursor.as_ref() || cursors.contains(next))
            {
                if options.mode == Mode::Full && cp.cursor.is_some() && !fallback {
                    cp.cursor = None;
                    cp.exhausted = false;
                    save_checkpoint(store, target, &mode, &cp)?;
                    fallback = true;
                    cursors.clear();
                    empty_pages = 0;
                    seen_entries = false;
                    log(
                        p,
                        "游标返回异常，从首页重新扫描；正在跳过已归档部分",
                        notify,
                    );
                    continue;
                }
                return Err(PlatformError::interface());
            }
            p.rate_limits = backend.rates();
            let has_entries = !page.tweets.is_empty() || page.unavailable > 0;
            if !has_entries && !seen_entries {
                return Err(PlatformError::interface());
            }
            seen_entries |= has_entries;
            empty_pages = if has_entries { 0 } else { empty_pages + 1 };
            page_number += 1;
            let mut new_content = false;
            let mut older = false;
            let mut before_start = false;
            p.page_total = page.tweets.iter().map(|t| t.media.len() as u64).sum();
            p.page_item = 0;
            let mut staged = Vec::new();
            // Persist the whole page's metadata and download intents before touching CDN.
            for tweet in &page.tweets {
                let known = known_tweet(store, &target.user_id, &tweet.id)?;
                if known && overlap_page.is_none() {
                    overlap_page = Some(page_number);
                }
                new_content |= !known;
                if !tweet.pinned && !tweet.retweet {
                    before_start |= options
                        .preferences
                        .date_start
                        .as_deref()
                        .and_then(date_seconds)
                        .zip(tweet.timestamp)
                        .is_some_and(|(s, t)| t < s);
                    older |= cutoff
                        .zip(tweet.timestamp)
                        .is_some_and(|(limit, time)| time < limit);
                    cp.newest = match (cp.newest, tweet.timestamp) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                }
                store
                    .save_tweet(
                        &user(target),
                        &user(&tweet.author),
                        &tweet.id,
                        Some(&tweet.text),
                        tweet.published_at.as_deref(),
                    )
                    .map_err(|_| PlatformError::local())?;
                for media in &tweet.media {
                    p.current_item = format!("{} · 推文 {}", media.kind, tweet.id);
                    let reason = if !options.preferences.within_dates(tweet) {
                        Some("日期外")
                    } else if !options.accepts(&media.kind)
                        || (!options.preferences.include_retweets && tweet.retweet)
                    {
                        Some("筛选外")
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        p.skipped += 1;
                        p.page_item += 1;
                        media_log(
                            p,
                            format!("跳过 {reason} · 推文 {}", tweet.id),
                            false,
                            notify,
                        );
                        continue;
                    }
                    let item = Retry {
                        tweet: tweet.clone(),
                        media: options.preferences.select_quality(media),
                    };
                    if source_exists(store, &target.user_id, &item, p)? {
                        clear_retry(store, &target.user_id, &item)?;
                        p.skipped += 1;
                        p.existing += 1;
                        p.page_item += 1;
                        media_log(p, "复用 已有媒体", false, notify);
                        continue;
                    }
                    new_content |= queue(store, &target.user_id, &item)?;
                    staged.push(item);
                }
            }
            let mut downloads = Vec::new();
            for item in staged {
                if attempted.insert((item.tweet.id.clone(), item.media.key.clone())) {
                    downloads.push(item);
                } else {
                    p.page_item += 1;
                    media_log(p, "跳过 本次已处理", false, notify);
                }
            }
            transfer::download_batch(
                store, target, downloads, options, cancel, backend, p, notify,
            )?;
            if stopped(cancel) {
                return Ok(());
            }
            p.pages += 1;
            p.tweets += page.tweets.len() as u64;
            cp.cursor = page.next;
            cp.exhausted = cp.cursor.is_none() || empty_pages >= 10;
            if let Some(cursor) = &cp.cursor {
                cursors.insert(cursor.clone());
            }
            // Commit point for page traversal: all media are indexed or durably queued.
            save_checkpoint(store, target, &mode, &cp)?;
            p.rate_limits = backend.rates();
            log(
                p,
                format!(
                    "{op} 第 {page_number} 页已保存；推文 {}，不可访问 {}",
                    page.tweets.len(),
                    page.unavailable
                ),
                notify,
            );
            if let Some(e) = exhausted_rate(backend) {
                return Err(e);
            }
            if options.mode == Mode::Full && before_start {
                date_ended = true;
                all_exhausted = false;
                // A date cutoff is a bounded scan, never a completed full checkpoint.
                cp.cursor = None;
                cp.exhausted = false;
                cp.newest = None;
                save_checkpoint(store, target, &mode, &cp)?;
                log(p, "按日期范围结束；未标记全量完成", notify);
                break;
            }
            quiet = if new_content { 0 } else { quiet + 1 };
            if options.mode == Mode::Incremental
                && !cp.exhausted
                && (overlap_page.is_none_or(|first| page_number > first)
                    && (quiet >= 2 || (older && page_number >= 2)))
            {
                log(
                    p,
                    if quiet >= 2 {
                        "连续两页没有新内容，增量扫描结束"
                    } else {
                        "已多检查一整页并越过上次全量的时间边界，增量扫描结束"
                    },
                    notify,
                );
                all_exhausted = false;
                break;
            }
        }
        all_exhausted &= cp.exhausted;
        completed_modes.push((mode, cp));
    }
    refresh_failures(store, target, options, p)?;
    p.exhausted = all_exhausted;
    p.date_ended = date_ended;
    if p.pending > 0 {
        p.outcome = "incomplete".into();
        return Ok(());
    }
    if date_ended {
        p.outcome = "date_range".into();
        return Ok(());
    }
    if options.mode == Mode::Full && all_exhausted {
        for (mode, mut cp) in completed_modes {
            cp.completed_at = Some(platform::client::now());
            cp.completed_newest = cp.newest;
            save_checkpoint(store, target, &mode, &cp)?;
        }
        p.outcome = "completed".into();
    } else {
        p.outcome = "updated".into();
    }
    Ok(())
}

struct RetryNetwork {
    media: HttpMedia,
}
impl Backend for RetryNetwork {
    fn page(&mut self, _: &str, _: &str, _: Option<&str>) -> Result<ArchivePage> {
        Err(PlatformError::local())
    }
    fn media(
        &mut self,
        item: &Attachment,
    ) -> std::result::Result<Box<dyn std::io::Read>, download::DownloadError> {
        self.media.open(
            self.media.url(&item.url)?,
            reqwest::header::HeaderMap::new(),
        )
    }
    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        self.media.worker()
    }
    fn rates(&self) -> BTreeMap<String, RateLimit> {
        BTreeMap::new()
    }
}
fn run_retries(
    root: &Path,
    options: &Options,
    cancel: &AtomicBool,
    mut notify: impl FnMut(Progress),
) -> Progress {
    let mut p = Progress {
        running: true,
        stage: "处理待重试媒体".into(),
        started_at: platform::client::now(),
        ..Default::default()
    };
    notify(p.clone());
    let result = (|| {
        let mut store = Store::open(root).map_err(|_| PlatformError::local())?;
        let name = parser::target_name(&options.target)?;
        let a = storage::list_archived_accounts(root)
            .map_err(|_| PlatformError::local())?
            .into_iter()
            .find(|a| a.screen_name.eq_ignore_ascii_case(&name))
            .ok_or_else(PlatformError::local)?;
        let target = Account {
            user_id: a.user_id,
            screen_name: a.screen_name,
            display_name: a.display_name,
        };
        let proxy = auth::platform_proxy(root).map_err(|_| PlatformError::local())?;
        let mut backend = RetryNetwork {
            media: HttpMedia::new(&proxy)?,
        };
        execute(
            &mut store,
            &target,
            options,
            cancel,
            &mut backend,
            &mut p,
            &mut notify,
        )
    })();
    finish(&mut p, result, cancel);
    notify(p.clone());
    p
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod options_tests;

#[cfg(test)]
#[path = "../../tests/e2e/mod.rs"]
mod e2e;

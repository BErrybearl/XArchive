//! Offline legacy import. Only the online-backup phase holds short SQLite read locks
//! on the selected source. All parsing, BLOB reads and verification use the snapshot.
use crate::storage::{self, MediaSource, StorageError, Store, User};
use rusqlite::{
    Connection, MAIN_DB, OpenFlags, OptionalExtension,
    backup::{Backup, StepResult},
    params,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

type Result<T> = storage::Result<T>;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub stage: String,
    pub done: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Counts {
    pub old_tweets: u64,
    pub old_media_rows: u64,
    pub expected_media: u64,
    pub old_sources: u64,
    pub imported_tweets: u64,
    pub imported_media: u64,
    pub imported_sources: u64,
    pub digests_checked: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Pending {
    pub key: String,
    pub items: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub passed: bool,
    pub stopped: bool,
    pub destination: String,
    pub snapshot: String,
    pub report_path: String,
    pub source_sha256_before: String,
    pub source_sha256_after: String,
    pub counts: Counts,
    pub missing: Vec<String>,
    pub mismatches: Vec<String>,
    pub pending: Vec<Pending>,
    pub notices: Vec<String>,
    pub error: Option<String>,
}

#[derive(Clone)]
struct Person {
    id: String,
    screen: String,
    display: Option<String>,
}
impl Person {
    fn user(&self) -> User<'_> {
        User {
            id: &self.id,
            screen_name: &self.screen,
            display_name: self.display.as_deref(),
        }
    }
}
struct Tweet {
    id: String,
    author: String,
    text: Option<String>,
    created: Option<String>,
}
struct Source {
    target: String,
    tweet: String,
    filename: String,
    url: String,
    kind: String,
    blob_id: i64,
    digest: Option<String>,
}
impl Source {
    fn key(&self) -> String {
        format!("legacy-filename:{}", self.filename)
    }
    fn label(&self) -> String {
        format!("{} / {} / {}", self.target, self.tweet, self.filename)
    }
    fn kind(&self) -> Result<&'static str> {
        match self.kind.as_str() {
            "photo" | "image" => Ok("image"),
            "video" => Ok("video"),
            "animated_gif" | "gif" => Ok("gif"),
            _ => Err(invalid("旧库包含不受支持的媒体类型")),
        }
    }
    fn mime(&self) -> &'static str {
        if self.kind == "video" || self.kind == "animated_gif" {
            return "video/mp4";
        }
        match self
            .filename
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "webp" => "image/webp",
            "gif" => "image/gif",
            _ => "application/octet-stream",
        }
    }
}
fn invalid(message: &str) -> StorageError {
    StorageError::Invalid(message.to_owned())
}
fn stopped(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// Also applied in release builds to reject traversal and reparse points.
fn validate_destination(root: &Path) -> Result<()> {
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("请选择完整的隔离数据目录路径"));
    }
    storage::check_path(root)?;
    if cfg!(debug_assertions) {
        let allowed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".runtime");
        if !root.starts_with(&allowed) {
            return Err(invalid("开发版只能使用本仓库 .runtime 内的隔离目录"));
        }
    }
    Ok(())
}

pub fn import(
    root: &Path,
    selected: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Report> {
    validate_destination(root)?;
    storage::check_path(selected)?;
    if !selected.is_file() {
        return Err(invalid("请选择本机旧版 SQLite 库文件"));
    }
    // Never accept the destination's own index (or a linked alias of it).
    let selected = selected.canonicalize()?;
    if root.join("archive.sqlite3").exists()
        && selected == root.join("archive.sqlite3").canonicalize()?
    {
        return Err(invalid("不能把新版归档索引作为旧库导入"));
    }
    storage::check_path(&root.join("temporary"))?;
    fs::create_dir_all(root.join("temporary"))?;
    let directory = tempfile::Builder::new()
        .prefix("legacy-import-")
        .tempdir_in(root.join("temporary"))?
        .keep();
    let snapshot = directory.join("source.sqlite3");
    let report_path = directory.join("report.json");
    let mut report = Report {
        destination: storage::friendly_path(root).to_string_lossy().into_owned(),
        snapshot: storage::friendly_path(&snapshot)
            .to_string_lossy()
            .into_owned(),
        report_path: storage::friendly_path(&report_path)
            .to_string_lossy()
            .into_owned(),
        notices: vec![
            "数量仅核对本次旧库对应的实体和来源，其他已有归档不计入。".into(),
            "旧版以来源 user_id 为账号范围；跨账号共用的 BLOB 在新版按账号独立保存。".into(),
            "待重试项目仅列在报告中，未下载，不表示已完成归档；缩略图、向量及缓存不迁移。".into(),
        ],
        ..Report::default()
    };
    let result = (|| -> Result<()> {
        progress(Progress {
            running: true,
            stage: "复制旧库".into(),
            ..Progress::default()
        });
        report.source_sha256_before = digest(CancelReader {
            reader: File::open(&selected)?,
            cancel,
        })?
        .0;
        {
            // No write-capable handle, journal-mode changes, ATTACH or source transaction.
            let original =
                Connection::open_with_flags(&selected, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            original.busy_timeout(Duration::from_millis(100))?;
            let mut destination = Connection::open(&snapshot)?;
            let backup = Backup::new(&original, &mut destination)?;
            let mut busy_since = None;
            loop {
                if stopped(cancel) {
                    report.stopped = true;
                    return Ok(());
                }
                match backup.step(32)? {
                    StepResult::Done => break,
                    StepResult::Busy | StepResult::Locked => {
                        let since = busy_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > Duration::from_secs(5) {
                            return Err(invalid("旧库忙，备份未完成；请关闭旧版后重试"));
                        }
                    }
                    StepResult::More => {
                        busy_since = None;
                    }
                    _ => return Err(invalid("旧库备份返回未知状态")),
                }
                let p = backup.progress();
                progress(Progress {
                    running: true,
                    stage: "复制旧库（页）".into(),
                    done: (p.pagecount - p.remaining).max(0) as u64,
                    total: p.pagecount.max(0) as u64,
                });
                std::thread::sleep(Duration::from_millis(1));
            }
        } // Release the source connection before opening the backup for any data reads.
        report.source_sha256_after = digest(CancelReader {
            reader: File::open(&selected)?,
            cancel,
        })?
        .0;
        if report.source_sha256_before != report.source_sha256_after {
            return Err(invalid("原库在备份期间发生变化，已停止；请关闭旧版后重试"));
        }
        let legacy = Connection::open_with_flags(&snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        legacy.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")?;
        let check: String = legacy.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(invalid("旧库副本完整性检查未通过"));
        }
        migrate(&legacy, root, cancel, &mut progress, &mut report)
    })();
    if let Err(error) = result {
        report.stopped |= stopped(cancel);
        report.error = Some(error.to_string());
        report.passed = false;
    }
    progress(Progress {
        running: true,
        stage: "保存核对报告".into(),
        ..Progress::default()
    });
    let mut file = File::create(&report_path)?;
    serde_json::to_writer_pretty(&mut file, &report)
        .map_err(|_| invalid("无法保存旧库核对报告"))?;
    file.sync_all()?;
    progress(Progress {
        running: false,
        stage: if report.stopped {
            "已停止，核对未通过"
        } else if report.passed {
            "核对通过"
        } else {
            "核对未通过"
        }
        .into(),
        done: report.counts.imported_sources,
        total: report.counts.old_sources,
    });
    Ok(report)
}

fn has_table(db: &Connection, name: &str) -> Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
fn load(db: &Connection) -> Result<(HashMap<String, Person>, Vec<Tweet>, Vec<Source>)> {
    // The supported v4 base columns are probed even on an empty database.
    let mut users = HashMap::new();
    let mut statement = db.prepare("SELECT user_id,screen_name,display_name FROM users")?;
    for row in statement.query_map([], |r| {
        Ok(Person {
            id: r.get(0)?,
            screen: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            display: r.get(2)?,
        })
    })? {
        let mut user = row?;
        if user.screen.trim().is_empty() {
            user.screen = user.id.clone();
        }
        users.insert(user.id.clone(), user);
    }
    let mut statement =
        db.prepare("SELECT tweet_id,user_id,text,created_at FROM tweets ORDER BY tweet_id")?;
    let tweets = statement
        .query_map([], |r| {
            Ok(Tweet {
                id: r.get(0)?,
                author: r.get(1)?,
                text: r.get(2)?,
                created: r.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut statement = db.prepare("SELECT user_id,tweet_id,filename,coalesce(url,''),media_type,id,sha256 FROM media ORDER BY id")?;
    let mapper = |r: &rusqlite::Row<'_>| {
        Ok(Source {
            target: r.get(0)?,
            tweet: r.get(1)?,
            filename: r.get(2)?,
            url: r.get(3)?,
            kind: r.get(4)?,
            blob_id: r.get(5)?,
            digest: r.get(6)?,
        })
    };
    let mut sources = statement
        .query_map([], mapper)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if has_table(db, "media_aliases")? {
        let mut statement = db.prepare("SELECT user_id,tweet_id,filename,coalesce(url,''),media_type,canonical_id,sha256 FROM media_aliases ORDER BY tweet_id,filename")?;
        sources.extend(
            statement
                .query_map([], mapper)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok((users, tweets, sources))
}
fn read_pending(db: &Connection, report: &mut Report) -> Result<()> {
    for table in ["settings", "state"] {
        if !has_table(db, table)? {
            continue;
        }
        let mut statement = db.prepare(&format!(
            "SELECT key,value FROM {table} WHERE key LIKE 'pending:%' ORDER BY key"
        ))?;
        for row in statement.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (key, value) = row?;
            let value = value.unwrap_or_default();
            match serde_json::from_str::<serde_json::Value>(&value) {
                Ok(items) if items.is_array() => report.pending.push(Pending {
                    key: format!("{table}/{key}"),
                    items,
                }),
                _ => {
                    report
                        .mismatches
                        .push(format!("待重试队列无法解析：{table}/{key}"));
                    report.pending.push(Pending {
                        key: format!("{table}/{key}"),
                        items: serde_json::Value::String(value),
                    });
                }
            }
        }
    }
    Ok(())
}

struct CancelReader<'a, R> {
    reader: R,
    cancel: &'a AtomicBool,
}
impl<R: Read> Read for CancelReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if stopped(self.cancel) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "导入已停止"));
        }
        self.reader.read(buffer)
    }
}
fn blob(db: &Connection, id: i64) -> Result<rusqlite::blob::Blob<'_>> {
    Ok(db.blob_open(MAIN_DB, "media", "data", id, true)?)
}
fn digest(mut reader: impl Read) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut length = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        length += n as u64;
    }
    Ok((storage::hex_digest(&hash.finalize()), length))
}
fn same_blobs(db: &Connection, left: i64, right: i64, cancel: &AtomicBool) -> Result<bool> {
    let left = blob(db, left)?;
    let mut right = blob(db, right)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    let mut left = CancelReader {
        reader: left,
        cancel,
    };
    let mut a = [0u8; 64 * 1024];
    let mut b = [0u8; 64 * 1024];
    loop {
        let n = left.read(&mut a)?;
        if n == 0 {
            return Ok(true);
        }
        right.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
    }
}

fn migrate(
    db: &Connection,
    root: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
    report: &mut Report,
) -> Result<()> {
    let (users, tweets, sources) = load(db)?;
    report.counts.old_tweets = tweets.len() as u64;
    report.counts.old_media_rows =
        db.query_row("SELECT count(*) FROM media", [], |r| r.get::<_, i64>(0))? as u64;
    report.counts.old_sources = sources.len() as u64;
    read_pending(db, report)?;
    let by_tweet: HashMap<_, _> = tweets.iter().map(|t| (t.id.as_str(), t)).collect();
    let targets: HashSet<_> = sources.iter().map(|s| s.target.as_str()).collect();
    let mut targets_by_tweet: HashMap<&str, HashSet<&str>> = HashMap::new();
    for source in &sources {
        targets_by_tweet
            .entry(&source.tweet)
            .or_default()
            .insert(&source.target);
    }
    let mut store = Store::open(root)?;
    for (i, tweet) in tweets.iter().enumerate() {
        if stopped(cancel) {
            report.stopped = true;
            break;
        }
        progress(Progress {
            running: true,
            stage: "导入推文".into(),
            done: i as u64,
            total: tweets.len() as u64,
        });
        let Some(author) = users.get(&tweet.author) else {
            report
                .missing
                .push(format!("推文 {} 的作者 {}", tweet.id, tweet.author));
            continue;
        };
        let tweet_targets = targets_by_tweet.get(tweet.id.as_str());
        let result = if tweet_targets.is_none() && targets.contains(author.id.as_str()) {
            store.save_tweet(
                &author.user(),
                &author.user(),
                &tweet.id,
                tweet.text.as_deref(),
                tweet.created.as_deref(),
            )
        } else {
            store.save_tweet_metadata(
                &author.user(),
                &tweet.id,
                tweet.text.as_deref(),
                tweet.created.as_deref(),
            )
        };
        if let Err(e) = result {
            report.mismatches.push(format!("推文 {}：{e}", tweet.id));
            continue;
        }
        for id in tweet_targets.into_iter().flatten() {
            if let Some(target) = users.get(*id) {
                if let Err(e) = store.save_tweet(
                    &target.user(),
                    &author.user(),
                    &tweet.id,
                    tweet.text.as_deref(),
                    tweet.created.as_deref(),
                ) {
                    report.mismatches.push(format!("推文 {}：{e}", tweet.id));
                }
            }
        }
    }
    for (i, source) in sources.iter().enumerate() {
        if stopped(cancel) {
            report.stopped = true;
            break;
        }
        progress(Progress {
            running: true,
            stage: "导入媒体与来源".into(),
            done: i as u64,
            total: sources.len() as u64,
        });
        let result = (|| -> Result<()> {
            let target = users
                .get(&source.target)
                .ok_or_else(|| invalid("来源账号在 users 中缺失"))?;
            let tweet = by_tweet
                .get(source.tweet.as_str())
                .ok_or_else(|| invalid("来源推文在 tweets 中缺失"))?;
            let author = users
                .get(&tweet.author)
                .ok_or_else(|| invalid("推文作者在 users 中缺失"))?;
            let key = source.key();
            let metadata = MediaSource {
                sequence: storage::layout::filename_sequence(&source.tweet, &source.filename),
                target: target.user(),
                author: author.user(),
                tweet_id: &source.tweet,
                text: tweet.text.as_deref(),
                published_at: tweet.created.as_deref(),
                media_key: &key,
                original_filename: &source.filename,
                original_url: &source.url,
                media_type: source.kind()?,
                mime_type: source.mime(),
            };
            store.save_media(
                &metadata,
                CancelReader {
                    reader: blob(db, source.blob_id)?,
                    cancel,
                },
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            if stopped(cancel) {
                report.stopped = true;
                break;
            }
            report
                .mismatches
                .push(format!("来源 {}：{error}", source.label()));
        }
    }
    // Verification re-reads every BLOB and every destination file; save counters are not evidence.
    verify(
        db, &store, root, &users, &tweets, &sources, cancel, progress, report,
    )?;
    report.passed = !report.stopped
        && report.error.is_none()
        && report.missing.is_empty()
        && report.mismatches.is_empty()
        && report.counts.old_tweets == report.counts.imported_tweets
        && report.counts.expected_media == report.counts.imported_media
        && report.counts.old_sources == report.counts.imported_sources;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify(
    db: &Connection,
    store: &Store,
    root: &Path,
    users: &HashMap<String, Person>,
    tweets: &[Tweet],
    sources: &[Source],
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
    report: &mut Report,
) -> Result<()> {
    let target = store.connection();
    for tweet in tweets {
        let found = target
            .query_row(
                "SELECT author_user_id,text,published_at FROM tweets WHERE tweet_id=?1",
                [&tweet.id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((author, text, date)) = found {
            report.counts.imported_tweets += 1;
            if author != tweet.author || text != tweet.text || date != tweet.created {
                report
                    .mismatches
                    .push(format!("推文元信息不一致：{}", tweet.id));
            }
        } else {
            report.missing.push(format!("推文：{}", tweet.id));
        }
    }
    for user in users.values().filter(|u| {
        tweets.iter().any(|t| t.author == u.id) || sources.iter().any(|s| s.target == u.id)
    }) {
        let found = target
            .query_row(
                "SELECT screen_name,display_name FROM users WHERE user_id=?1",
                [&user.id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        if found.as_ref() != Some(&(user.screen.clone(), user.display.clone())) {
            report
                .mismatches
                .push(format!("作者或账号元信息不一致：{}", user.id));
        }
    }
    let mut hashes: HashMap<i64, (String, u64)> = HashMap::new();
    let mut candidates: HashMap<(String, String, u64), Vec<i64>> = HashMap::new();
    let mut actual_entities = HashSet::new();
    let mut actual_sources = HashSet::new();
    let mut checked_files: HashMap<i64, (String, u64)> = HashMap::new();
    for (i, source) in sources.iter().enumerate() {
        // A requested stop keeps verification bounded and explicitly leaves the report unpassed.
        if stopped(cancel) {
            report.stopped = true;
            return Ok(());
        }
        progress(Progress {
            running: true,
            stage: "核对来源与媒体摘要".into(),
            done: i as u64,
            total: sources.len() as u64,
        });
        let old = if let Some(hash) = hashes.get(&source.blob_id) {
            hash.clone()
        } else {
            match blob(db, source.blob_id)
                .and_then(|reader| digest(CancelReader { reader, cancel }))
            {
                Ok(hash) => {
                    hashes.insert(source.blob_id, hash.clone());
                    hash
                }
                Err(e) => {
                    report
                        .missing
                        .push(format!("旧库 BLOB {}：{e}", source.blob_id));
                    continue;
                }
            }
        };
        if old.1 == 0 {
            report
                .mismatches
                .push(format!("旧库媒体为空：{}", source.label()));
        }
        if let Some(declared) = source.digest.as_ref().filter(|d| !d.is_empty()) {
            if declared.to_ascii_lowercase() != old.0 {
                report
                    .mismatches
                    .push(format!("旧库摘要与 BLOB 不一致：{}", source.label()));
            }
        }
        let group = candidates
            .entry((source.target.clone(), old.0.clone(), old.1))
            .or_default();
        let mut equal = false;
        for id in group.iter() {
            if same_blobs(db, *id, source.blob_id, cancel)? {
                equal = true;
                break;
            }
        }
        if !equal {
            group.push(source.blob_id);
            report.counts.expected_media += 1;
        }
        let found=target.query_row(
            "SELECT s.source_id,s.entity_id,s.original_filename,s.original_url,s.media_type,e.relative_path,e.sha256,e.byte_length
             FROM media_sources s JOIN media_entities e ON e.entity_id=s.entity_id AND e.target_user_id=s.target_user_id
             WHERE s.target_user_id=?1 AND s.tweet_id=?2 AND s.media_key=?3",
            params![source.target,source.tweet,source.key()],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?))).optional()?;
        let Some((sid, eid, name, url, kind, file, hash, length)) = found else {
            report.missing.push(format!("来源：{}", source.label()));
            continue;
        };
        actual_sources.insert(sid);
        actual_entities.insert(eid);
        if name != source.filename || url != source.url || source.kind().ok() != Some(kind.as_str())
        {
            report
                .mismatches
                .push(format!("来源元信息不一致：{}", source.label()));
        }
        let actual = if let Some(value) = checked_files.get(&eid) {
            value.clone()
        } else {
            match storage::media_path(root, &file).and_then(|p| {
                digest(CancelReader {
                    reader: File::open(p)?,
                    cancel,
                })
            }) {
                Ok(value) => {
                    checked_files.insert(eid, value.clone());
                    value
                }
                Err(e) => {
                    report.missing.push(format!("媒体 {}：{e}", source.label()));
                    continue;
                }
            }
        };
        report.counts.digests_checked += 1;
        if actual != old || hash != old.0 || (length < 0 || length as u64 != old.1) {
            report
                .mismatches
                .push(format!("媒体摘要或长度不一致：{}", source.label()));
        }
    }
    report.counts.imported_sources = actual_sources.len() as u64;
    report.counts.imported_media = actual_entities.len() as u64;
    if report.counts.old_media_rows != report.counts.expected_media {
        report.notices.push(format!("旧库物理 BLOB {} 份，按来源账号和实际字节核算的新库唯一媒体 {} 份；跨账号拆分或旧库重复内容导致差异。",report.counts.old_media_rows,report.counts.expected_media));
    }
    for (label, expected, actual) in [
        (
            "推文",
            report.counts.old_tweets,
            report.counts.imported_tweets,
        ),
        (
            "唯一媒体（按账号）",
            report.counts.expected_media,
            report.counts.imported_media,
        ),
        (
            "来源",
            report.counts.old_sources,
            report.counts.imported_sources,
        ),
    ] {
        if expected != actual {
            report.mismatches.push(format!(
                "{label}数量不一致：旧库预期 {expected}，新库核对 {actual}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

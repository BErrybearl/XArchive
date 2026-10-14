//! Offline exports. The Tauri coordinator holds the application's storage lock throughout.
mod catalog;
pub(crate) mod commands;
mod paths;
use crate::{
    archive,
    storage::{self, StorageError, Store},
};
use catalog::{Entity, load_entities};
pub(crate) use commands::ExportAccess;
use paths::{export_path, invalid, validate_output};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;
type Result<T> = storage::Result<T>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Full,
    Incremental,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Options {
    /// Stable archived user ID; null means all archived accounts.
    pub account: Option<String>,
    pub mode: Mode,
    #[serde(default)]
    pub media_types: archive::MediaTypes,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    /// null reuses the last selected output directory.
    pub output_directory: Option<PathBuf>,
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        if !self.media_types.image && !self.media_types.video && !self.media_types.gif {
            return Err(invalid("至少选择一种导出媒体类型"));
        }
        if self.account.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Err(invalid("请选择归档账号，或用 null 表示全部账号"));
        }
        for date in [&self.date_start, &self.date_end].into_iter().flatten() {
            if archive::date_seconds(date).is_none() {
                return Err(invalid("导出日期格式无效"));
            }
        }
        if self
            .date_start
            .as_ref()
            .zip(self.date_end.as_ref())
            .is_some_and(|(s, e)| s > e)
        {
            return Err(invalid("起始日期不能晚于结束日期"));
        }
        Ok(())
    }
    fn accepts(&self, entity: &Entity) -> bool {
        entity.sources.iter().any(|source| {
            let kind = match source.media_type.as_str() {
                "image" => self.media_types.image,
                "video" => self.media_types.video,
                "gif" => self.media_types.gif,
                _ => false,
            };
            kind && if self.date_start.is_none() && self.date_end.is_none() {
                true
            } else {
                source.date.as_ref().is_some_and(|day| {
                    self.date_start.as_ref().is_none_or(|s| day >= s)
                        && self.date_end.as_ref().is_none_or(|e| day <= e)
                })
            }
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub tweet_id: String,
    pub author_id: String,
    pub author: String,
    pub published_at: Option<String>,
    pub text: Option<String>,
    pub original_filename: String,
    pub original_url: String,
    pub media_type: String,
    pub sequence: u64,
    #[serde(skip)]
    date: Option<String>,
    #[serde(skip)]
    sort_time: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexEntry {
    pub path: String,
    pub entity_id: i64,
    pub sha256: String,
    pub byte_length: u64,
    pub media_type: String,
    pub mime_type: String,
    pub sources: Vec<Source>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub account: String,
    pub entity_id: i64,
    pub path: String,
    /// modified, deleted, uncertain, conflict, source_missing, source_changed, io
    pub reason: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct AccountResult {
    pub account: String,
    pub directory: String,
    pub indexed: u64,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub stage: String,
    pub outcome: String,
    pub account: Option<String>,
    pub current_item: String,
    pub total: u64,
    pub done: u64,
    pub copied: u64,
    pub skipped: u64,
    pub bytes: u64,
    pub checked_bytes: u64,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub accounts: Vec<AccountResult>,
    pub issues: Vec<Issue>,
    pub error: Option<String>,
    pub logs: Vec<String>,
    #[serde(skip)]
    last_push: Option<Instant>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
fn log(p: &mut Progress, msg: String) {
    p.logs.push(msg);
    if p.logs.len() > 200 {
        p.logs.remove(0);
    }
}
fn issue(
    p: &mut Progress,
    account: &str,
    file: &Manifest,
    directory: &Path,
    reason: &str,
    message: &str,
) {
    p.issues.push(Issue {
        account: account.into(),
        entity_id: file.entity_id,
        path: storage::friendly_path(&directory.join(&file.path))
            .to_string_lossy()
            .into_owned(),
        reason: reason.into(),
        message: message.into(),
    });
    log(p, format!("{}：{}（{}）", file.path, message, reason));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    Pending,
    Publishing,
    Written,
}
#[derive(Debug)]
struct Manifest {
    entity_id: i64,
    path: String,
    size: u64,
    digest: String,
    state: FileState,
}
fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn manifest(store: &Store, id: i64) -> Result<Vec<Manifest>> {
    let mut q = store.connection().prepare("SELECT entity_id,relative_path,byte_length,sha256,state FROM export_files WHERE export_id=?1 ORDER BY relative_path")?;
    Ok(q.query_map([id], |r| {
        Ok(Manifest {
            entity_id: r.get(0)?,
            path: r.get(1)?,
            size: unsigned(r, 2)?,
            digest: r.get(3)?,
            state: match r.get::<_, String>(4)?.as_str() {
                "pending" => FileState::Pending,
                "publishing" => FileState::Publishing,
                "written" => FileState::Written,
                _ => return Err(rusqlite::Error::InvalidQuery),
            },
        })
    })?
    .collect::<std::result::Result<_, _>>()?)
}

/// A completed file is never repaired. Pending entries can be completed after stop/crash.
pub fn run(
    root: &Path,
    output: &Path,
    options: &Options,
    cancel: &AtomicBool,
    mut notify: impl FnMut(Progress),
) -> Progress {
    let mut p = Progress {
        running: true,
        stage: "准备导出".into(),
        started_at: now(),
        ..Progress::default()
    };
    push(&mut p, true, &mut notify);
    if let Err(e) = execute(root, output, options, cancel, &mut p, &mut notify) {
        p.error = Some(e.to_string());
    }
    p.running = false;
    p.finished_at = Some(now());
    p.outcome = if p.error.is_some() {
        "incomplete"
    } else if cancel.load(Ordering::Relaxed) {
        "stopped"
    } else if !p.issues.is_empty() {
        "completed_with_issues"
    } else {
        "completed"
    }
    .into();
    p.stage = match p.outcome.as_str() {
        "completed" => "导出完成",
        "completed_with_issues" => "导出部分完成，问题已记录",
        "stopped" => "已停止",
        _ => "导出未完成",
    }
    .into();
    let summary = format!(
        "{}；新增 {}，跳过 {}，问题 {}",
        p.stage,
        p.copied,
        p.skipped,
        p.issues.len()
    );
    log(&mut p, summary);
    push(&mut p, true, &mut notify);
    p
}

fn execute(
    root: &Path,
    output: &Path,
    options: &Options,
    cancel: &AtomicBool,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<()> {
    options.validate()?;
    let output = validate_output(root, output)?;
    // Do not silently initialize an empty archive through an export command.
    if !storage::status(root)?.initialized {
        return Err(invalid("请先初始化或归档数据"));
    }
    let store = Store::open(root)?;
    let mut accounts = storage::list_archived_accounts(root)?;
    accounts.retain(|a| !a.user_id.is_empty());
    if let Some(account) = &options.account {
        accounts.retain(|a| &a.user_id == account);
        if accounts.is_empty() {
            return Err(invalid("所选账号不在归档列表中"));
        }
    }
    for account in accounts {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        p.account = Some(account.user_id.clone());
        p.stage = "导出媒体".into();
        let result = (|| -> Result<()> {
            let entities = load_entities(&store, &account.user_id)?;
            if entities.is_empty() {
                let prior: bool = store.connection().query_row(
                    "SELECT EXISTS(SELECT 1 FROM export_directories WHERE target_user_id=?1)",
                    [&account.user_id],
                    |r| r.get(0),
                )?;
                if options.mode != Mode::Incremental || !prior {
                    log(p, format!("{} 没有媒体，跳过导出", account.screen_name));
                    return Ok(());
                }
            }
            let selected: Vec<_> = entities.iter().filter(|e| options.accepts(e)).collect();
            p.total += selected.len() as u64;
            let (id, directory) = directory(
                &store,
                &output,
                &account.user_id,
                &account.screen_name,
                options.mode,
            )?;
            push(p, true, notify);
            let existing = manifest(&store, id)?;
            // A deleted export directory is a user deletion; never recreate it implicitly.
            if !directory.is_dir() {
                for file in &existing {
                    issue(
                        p,
                        &account.user_id,
                        file,
                        &directory,
                        "deleted",
                        "用户删除的导出目录未恢复",
                    );
                }
                return Err(invalid("上次导出目录已删除，请用全量导出创建新目录"));
            }
            for entity in selected {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let file = if let Some(file) = existing.iter().find(|f| f.entity_id == entity.id) {
                    Manifest {
                        entity_id: file.entity_id,
                        path: file.path.clone(),
                        size: file.size,
                        digest: file.digest.clone(),
                        state: file.state,
                    }
                } else {
                    plan(&store, id, &directory, entity)?
                };
                p.current_item = file.path.clone();
                // Existing written files are verified below together with all retained files.
                if file.state != FileState::Written {
                    let path = export_path(&directory, &file.path)?;
                    if path.exists() {
                        let Some(valid) =
                            verify_file(&path, file.size, &file.digest, cancel, p, notify)?
                        else {
                            break;
                        };
                        if valid {
                            mark_written(&store, id, file.entity_id)?;
                            p.skipped += 1;
                        } // Conflicts are reported exactly once by rebuild_index.
                    } else if file.state == FileState::Pending {
                        let source = storage::media_path(store.root(), &entity.stored_path)?;
                        match copy_verified(&source, &path, &store, id, &file, cancel, p, notify) {
                            Ok(true) => {
                                mark_written(&store, id, file.entity_id)?;
                                p.copied += 1;
                                log(p, format!("已导出 {}", file.path));
                            }
                            Ok(false) => break,
                            Err(e) => {
                                let reason = if !source.is_file() {
                                    "source_missing"
                                } else if matches!(e, StorageError::Invalid(_)) {
                                    "source_changed"
                                } else {
                                    "io"
                                };
                                issue(
                                    p,
                                    &account.user_id,
                                    &file,
                                    &directory,
                                    reason,
                                    &e.to_string(),
                                );
                                push(p, true, notify);
                            }
                        }
                    }
                } else {
                    p.skipped += 1;
                }
                p.done += 1;
                push(p, false, notify);
            }
            // Cancellation preserves the last published indexes.
            let entries = rebuild_index(
                &store,
                id,
                &directory,
                &account.user_id,
                &entities,
                cancel,
                p,
                notify,
            )?;
            p.accounts.push(AccountResult {
                account: account.user_id.clone(),
                directory: storage::friendly_path(&directory)
                    .to_string_lossy()
                    .into_owned(),
                indexed: entries.unwrap_or_else(|| {
                    fs::read(directory.join("index.json"))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Vec<IndexEntry>>(&b).ok())
                        .map_or(0, |e| e.len())
                }) as u64,
            });
            push(p, true, notify);
            Ok(())
        })();
        if let Err(e) = result {
            if options.account.is_some() {
                return Err(e);
            }
            p.issues.push(Issue {
                account: account.user_id.clone(),
                entity_id: 0,
                path: storage::friendly_path(&output)
                    .to_string_lossy()
                    .into_owned(),
                reason: "account_failed".into(),
                message: e.to_string(),
            });
            log(p, format!("{} 导出失败：{e}", account.screen_name));
            push(p, true, notify);
        }
    }
    if p.accounts.is_empty() && p.issues.iter().any(|i| i.reason == "account_failed") {
        return Err(invalid("账号导出均未完成，请查看问题清单"));
    }
    Ok(())
}

fn directory(
    store: &Store,
    output: &Path,
    account: &str,
    screen: &str,
    mode: Mode,
) -> Result<(i64, PathBuf)> {
    if mode == Mode::Incremental {
        let prior: Option<(i64,String,String)> = store.connection().query_row("SELECT export_id,directory,output_root FROM export_directories WHERE target_user_id=?1 ORDER BY export_id DESC LIMIT 1",[account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((id, dir, previous_root)) = prior {
            let dir = storage::friendly_path(Path::new(&dir));
            let previous_root = paths::resolved(Path::new(&previous_root))?;
            let resolved_dir = paths::resolved(&dir)?;
            let resolved_output = paths::resolved(output)?;
            if previous_root != resolved_output
                || !resolved_dir.starts_with(&resolved_output)
                || resolved_dir == resolved_output
            {
                return Err(invalid(
                    "输出目录与该账号上次导出不同，请用全量导出创建新目录",
                ));
            }
            paths::validate_directory(store.root(), &dir)?;
            return Ok((id, dir));
        }
    }
    let folder = paths::account_folder(store, account, screen)?;
    let base = output.join(folder);
    paths::validate_directory(store.root(), &base)?;
    fs::create_dir_all(&base)?;
    for offset in 0..86400 {
        let timestamp: String = store.connection().query_row(
            "SELECT strftime('%Y%m%d-%H%M%S','now',?1)",
            [format!("+{offset} seconds")],
            |r| r.get(0),
        )?;
        let dir = base.join(timestamp);
        paths::validate_directory(store.root(), &dir)?;
        let tracked: bool = store.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM export_directories WHERE directory=?1)",
            [dir.to_string_lossy()],
            |r| r.get(0),
        )?;
        if tracked {
            continue;
        }
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
        fs::create_dir(dir.join("media"))?;
        store.connection().execute("INSERT INTO export_directories(target_user_id,output_root,directory) VALUES (?1,?2,?3)",params![account,output.to_string_lossy(),dir.to_string_lossy()])?;
        return Ok((store.connection().last_insert_rowid(), dir));
    }
    Err(invalid("无法创建独立的全量导出目录"))
}
fn plan(store: &Store, id: i64, directory: &Path, entity: &Entity) -> Result<Manifest> {
    let first = entity
        .sources
        .first()
        .ok_or_else(|| invalid("媒体实体没有来源"))?;
    let date = first.date.as_deref().unwrap_or("unknown-date");
    let tweet = paths::clean_filename(&first.tweet_id);
    let ext = paths::extension(entity);
    let mut seq = first.sequence;
    loop {
        let path = format!("media/{date}_{tweet}_{seq}.{ext}");
        let destination = export_path(directory, &path)?;
        let occupied: bool = store.connection().query_row("SELECT EXISTS(SELECT 1 FROM export_files WHERE export_id=?1 AND relative_path=?2 COLLATE NOCASE)",params![id,path],|r|r.get(0))?;
        if !occupied && !destination.exists() {
            let length = i64::try_from(entity.size).map_err(|_| invalid("导出文件过大"))?;
            store.connection().execute("INSERT INTO export_files(export_id,entity_id,relative_path,byte_length,sha256,state) VALUES (?1,?2,?3,?4,?5,'pending')",params![id,entity.id,path,length,entity.digest])?;
            return Ok(Manifest {
                entity_id: entity.id,
                path,
                size: entity.size,
                digest: entity.digest.clone(),
                state: FileState::Pending,
            });
        }
        seq = seq
            .checked_add(1)
            .ok_or_else(|| invalid("导出文件序号超出范围"))?;
    }
}
fn mark_written(store: &Store, id: i64, entity: i64) -> Result<()> {
    store.connection().execute(
        "UPDATE export_files SET state='written' WHERE export_id=?1 AND entity_id=?2",
        params![id, entity],
    )?;
    Ok(())
}
#[cfg(test)]
fn verified(path: &Path, size: u64, digest: &str) -> Result<bool> {
    Ok(verify_file(
        path,
        size,
        digest,
        &AtomicBool::new(false),
        &mut Progress::default(),
        &mut |_| {},
    )? == Some(true))
}
// None means stopped. Verification streams in the same bounded chunks as copying.
fn verify_file(
    path: &Path,
    size: u64,
    digest: &str,
    cancel: &AtomicBool,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<Option<bool>> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    storage::check_path(path)?;
    if !path.is_file() || path.metadata()?.len() != size {
        return Ok(Some(false));
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        length += count as u64;
        p.checked_bytes += count as u64;
        push(p, false, notify);
    }
    Ok(Some(
        length == size && storage::hex_digest(&hash.finalize()) == digest,
    ))
}
fn copy_verified(
    source: &Path,
    destination: &Path,
    store: &Store,
    export_id: i64,
    manifest: &Manifest,
    cancel: &AtomicBool,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<bool> {
    storage::check_path(source)?;
    storage::check_path(destination)?;
    if !source.is_file() {
        return Err(invalid("归档媒体缺失，未生成导出文件"));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| invalid("导出路径无效"))?;
    storage::check_path(parent)?;
    // Missing media/ with completed files is treated as user deletion below; only
    // pending files may be added here, never previously completed files.
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut file = File::open(source)?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        temporary.write_all(&buffer[..count])?;
        hash.update(&buffer[..count]);
        length += count as u64;
        p.bytes += count as u64;
        push(p, false, notify);
    }
    if length != manifest.size || storage::hex_digest(&hash.finalize()) != manifest.digest {
        return Err(invalid("归档媒体大小或摘要不符，未生成导出文件"));
    }
    temporary.as_file().sync_all()?;
    match verify_file(
        temporary.path(),
        manifest.size,
        &manifest.digest,
        cancel,
        p,
        notify,
    )? {
        None => return Ok(false),
        Some(false) => return Err(invalid("临时导出文件核对未通过")),
        Some(true) => {}
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(false);
    }
    storage::check_path(destination)?;
    // Persist an intent before exposing the final file. On an uncertain process
    // death, a missing publishing file is protected instead of silently restored.
    store.connection().execute(
        "UPDATE export_files SET state='publishing' WHERE export_id=?1 AND entity_id=?2",
        params![export_id, manifest.entity_id],
    )?;
    match temporary.persist_noclobber(destination) {
        Ok(file) => file.sync_all()?,
        Err(error) => {
            // persist failed before publication and returned ownership of the temporary file.
            store.connection().execute(
                "UPDATE export_files SET state='pending' WHERE export_id=?1 AND entity_id=?2",
                params![export_id, manifest.entity_id],
            )?;
            return Err(StorageError::Io(error.error));
        }
    }
    Ok(true)
}

fn rebuild_index(
    store: &Store,
    id: i64,
    directory: &Path,
    account: &str,
    entities: &[Entity],
    cancel: &AtomicBool,
    p: &mut Progress,
    notify: &mut impl FnMut(Progress),
) -> Result<Option<usize>> {
    p.stage = "核对并写入索引".into();
    push(p, true, notify);
    let mut entries = Vec::new();
    for file in manifest(store, id)? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        p.current_item = file.path.clone();
        let path = export_path(directory, &file.path)?;
        let valid = match verify_file(&path, file.size, &file.digest, cancel, p, notify) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(None),
            Err(e) => {
                issue(p, account, &file, directory, "io", &e.to_string());
                push(p, true, notify);
                continue;
            }
        };
        if !valid {
            if file.state != FileState::Pending || path.exists() {
                let (reason, msg) = if !path.exists() {
                    if file.state == FileState::Publishing {
                        (
                            "uncertain",
                            "文件发布曾中断，缺失文件未恢复；可用全量另建目录",
                        )
                    } else {
                        ("deleted", "用户删除的文件未恢复")
                    }
                } else if file.state != FileState::Pending {
                    ("modified", "用户改动的文件未覆盖")
                } else {
                    ("conflict", "待写路径有不同内容，未覆盖")
                };
                issue(p, account, &file, directory, reason, msg);
                push(p, true, notify);
            }
            continue;
        }
        // Covers a crash after rename but before the SQLite written marker.
        if file.state != FileState::Written {
            mark_written(store, id, file.entity_id)?;
        }
        let entity = entities
            .iter()
            .find(|e| e.id == file.entity_id)
            .ok_or_else(|| invalid("导出清单实体缺失"))?;
        entries.push(IndexEntry {
            path: file.path,
            entity_id: file.entity_id,
            sha256: file.digest,
            byte_length: file.size,
            media_type: entity.sources[0].media_type.clone(),
            mime_type: entity.mime.clone(),
            sources: entity.sources.clone(),
        });
    }
    let json = serde_json::to_vec_pretty(&entries).map_err(|_| invalid("无法序列化导出索引"))?;
    let csv = catalog::csv(&entries)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    // Each index is independently replaced atomically; readers should use one format.
    atomic_write(&directory.join("index.json"), &json)?;
    atomic_write(&directory.join("index.csv"), &csv)?;
    Ok(Some(entries.len()))
}
pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    storage::check_path(path)?;
    let parent = path.parent().ok_or_else(|| invalid("文件路径无效"))?;
    let mut f = NamedTempFile::new_in(parent)?;
    f.write_all(bytes)?;
    f.as_file().sync_all()?;
    storage::check_path(path)?;
    f.persist(path)
        .map_err(|e| StorageError::Io(e.error))?
        .sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Layout {
    A,
    #[default]
    C,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct LayoutProgress {
    pub running: bool,
    pub layout: Layout,
    pub done: u64,
    pub total: u64,
    pub renamed: u64,
    pub stage: String,
    pub current: String,
    pub backup: String,
    pub stopped: bool,
    pub error: Option<String>,
}
pub(super) fn current_layout(db: &Connection) -> Result<Layout> {
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 6 {
        return Ok(Layout::C);
    }
    let value: String = db.query_row(
        "SELECT layout FROM storage_layout WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    match value.as_str() {
        "A" => Ok(Layout::A),
        "C" => Ok(Layout::C),
        _ => Err(invalid("存储结构设置无效")),
    }
}
pub(super) fn reserved_name(raw: &str) -> bool {
    let base = raw.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(
                &base[3..],
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
}
fn clean(raw: &str, max: usize) -> String {
    let mut result: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    while result.contains("..") {
        result = result.replace("..", "_");
    }
    result = result.trim_matches(['.', ' ']).to_owned();
    if reserved_name(&result) {
        result.insert(0, '_');
    }
    if result.is_empty() {
        result = "unknown".into();
    }
    if result.encode_utf16().count() > max {
        let digest = hex_digest(&Sha256::digest(result.as_bytes()));
        let mut prefix = String::new();
        for c in result.chars() {
            if prefix.encode_utf16().count() + c.len_utf16() > max.saturating_sub(9) {
                break;
            }
            prefix.push(c);
        }
        result = format!("{}-{}", prefix.trim_end_matches(['.', ' ']), &digest[..8]);
    }
    result
}
pub(super) fn detect_ext(path: &Path, raw_url: &str, mime: &str) -> Result<String> {
    let mut header = [0u8; 16];
    let size = File::open(path)?.read(&mut header)?;
    let b = &header[..size];
    let magic = if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" {
        match &b[8..12] {
            b"avif" | b"avis" => Some("avif"),
            b"heic" | b"heix" | b"hevc" | b"hevx" => Some("heic"),
            b"isom" | b"iso2" | b"iso3" | b"iso4" | b"iso5" | b"iso6" | b"mp41" | b"mp42"
            | b"avc1" | b"dash" | b"M4V " | b"M4A " => Some("mp4"),
            _ => None,
        }
    } else {
        None
    };
    if let Some(ext) = magic {
        return Ok(ext.into());
    }
    fn extension(raw: &str) -> Option<String> {
        let raw = raw.to_ascii_lowercase();
        let raw = if raw == "jpeg" { "jpg" } else { &raw };
        if !raw.is_empty() && raw.len() <= 10 && raw.bytes().all(|c| c.is_ascii_alphanumeric()) {
            Some(raw.into())
        } else {
            None
        }
    }
    if let Ok(url) = url::Url::parse(raw_url) {
        if let Some(ext) = url
            .query_pairs()
            .find(|(key, _)| key == "format")
            .and_then(|(_, value)| extension(&value))
        {
            return Ok(ext);
        }
        if let Some(ext) = url
            .path_segments()
            .and_then(|mut parts| parts.next_back())
            .and_then(|name| name.rsplit_once('.'))
            .and_then(|(_, ext)| extension(ext))
        {
            return Ok(ext);
        }
    }
    Ok(match mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "bin",
    }
    .into())
}
pub(super) fn source_sequence(db: &Connection, source: &MediaSource<'_>) -> Result<u64> {
    if let Some(sequence) = source.sequence {
        return Ok(sequence.max(1));
    }
    let existing: Option<i64> = db.query_row("SELECT sequence FROM media_sources WHERE target_user_id=?1 AND tweet_id=?2 AND media_key=?3", params![source.target.id,source.tweet_id,source.media_key], |r| r.get(0)).optional()?;
    if let Some(n) = existing.filter(|n| *n > 0) {
        return sql_unsigned(n);
    }
    sql_unsigned(db.query_row("SELECT coalesce(max(sequence),0)+1 FROM media_sources WHERE target_user_id=?1 AND tweet_id=?2", params![source.target.id, source.tweet_id], |r| r.get(0))?)
}
pub(crate) fn filename_sequence(tweet: &str, filename: &str) -> Option<u64> {
    let (stem, _) = filename.rsplit_once('.')?;
    let (prefix, slot) = stem.rsplit_once('_')?;
    if prefix != tweet {
        return None;
    }
    slot.parse::<u64>()
        .ok()?
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
}
pub(super) fn migrate_sequences(db: &Connection) -> Result<()> {
    let rows = {
        let mut q =
            db.prepare("SELECT source_id,tweet_id,media_key,original_filename FROM media_sources")?;
        q.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, tweet, key, filename) in rows {
        let ordinal = filename_sequence(&tweet, &filename)
            .or_else(|| {
                key.strip_prefix("legacy-filename:")
                    .and_then(|f| filename_sequence(&tweet, f))
            })
            .or_else(|| {
                key.strip_prefix("slot:")
                    .and_then(|n| n.parse::<u64>().ok())
                    .and_then(|n| n.checked_add(1))
                    .filter(|n| *n <= i64::MAX as u64)
            });
        if let Some(n) = ordinal {
            db.execute(
                "UPDATE media_sources SET sequence=?2 WHERE source_id=?1",
                params![id, n as i64],
            )?;
        }
    }
    Ok(())
}
fn source_time(db: &Connection, raw: &str) -> Result<Option<i64>> {
    let Some(seconds) = timestamp(db, raw)? else {
        return Ok(None);
    };
    // SQLite preserves the subsecond part of ISO publication timestamps. Public
    // Twitter timestamps have whole seconds and therefore use zero milliseconds.
    let millis: Option<i64> = db.query_row(
        "SELECT CAST(substr(strftime('%f',?1),4,3) AS INTEGER)",
        [raw],
        |r| r.get(0),
    )?;
    Ok(Some(seconds * 1000 + millis.unwrap_or(0)))
}
#[allow(clippy::too_many_arguments)]
pub(super) fn build_path(
    root: &Path,
    db: &Connection,
    layout: Layout,
    account: &str,
    published: Option<&str>,
    tweet: &str,
    sequence: u64,
    kind: &str,
    ext: &str,
) -> Result<String> {
    let timestamp = published
        .map(|raw| timestamp(db, raw))
        .transpose()?
        .flatten();
    let date: String = match timestamp {
        Some(t) => db.query_row(
            "SELECT strftime('%Y-%m-%d',?1,'unixepoch','localtime')",
            [t],
            |r| r.get(0),
        )?,
        None => "unknown-date".into(),
    };
    let account = clean(account.trim_start_matches('@'), 40);
    let folder = match layout {
        Layout::A => {
            if timestamp.is_some() {
                date[..7].to_owned()
            } else {
                "unknown-month".into()
            }
        }
        Layout::C => match kind {
            "image" => "图片",
            "video" => "视频",
            "gif" => "GIF",
            _ => return Err(invalid("媒体类型无效")),
        }
        .into(),
    };
    let path = format!(
        "media/@{account}/{folder}/{date}_{}_{sequence}.{}",
        clean(tweet, 64),
        clean(ext, 10)
    );
    // Leave room for Windows collision suffixes and normal Explorer paths.
    if friendly_path(&root.join(&path))
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .count()
        > 240
    {
        return Err(invalid("数据目录路径过长，请选择较短的目录后重试"));
    }
    Ok(path)
}
pub(super) fn available_path(
    root: &Path,
    db: &Connection,
    desired: &str,
    old: Option<&str>,
) -> Result<String> {
    let (stem, ext) = desired
        .rsplit_once('.')
        .ok_or_else(|| invalid("媒体扩展名无效"))?;
    for n in 1..=100_000 {
        let relative = if n == 1 {
            desired.to_owned()
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let path = media_path(root, &relative)?;
        if old == Some(relative.as_str()) {
            return Ok(relative);
        }
        let occupied: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM media_entities WHERE relative_path=?1) OR EXISTS(SELECT 1 FROM media_rename_log WHERE new_path=?1 AND state<>'done' AND old_path<>coalesce(?2,''))", params![relative,old], |r| r.get(0))?;
        if !path.try_exists()? && !occupied {
            return Ok(relative);
        }
    }
    Err(invalid("媒体重名过多，无法分配文件名"))
}
struct Naming {
    old: String,
    account: String,
    kind: String,
    ext: String,
    desired: String,
}
impl Store {
    pub fn media_directory(&self, account: Option<&str>) -> Result<PathBuf> {
        let path = if let Some(account) = account {
            let id = self.account_id(account)?.unwrap_or_default();
            let screen: Option<String> = self
                .connection
                .query_row(
                    "SELECT screen_name FROM users WHERE user_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()?;
            let current = media_path(
                &self.root,
                &format!(
                    "media/@{}",
                    clean(
                        screen.as_deref().unwrap_or(account).trim_start_matches('@'),
                        40
                    )
                ),
            )?;
            if current.is_dir() {
                current
            } else {
                // Account renames deliberately keep old files in their original
                // folder until organization. Open that folder when it is the
                // only existing media location; v5 flat files still open media.
                let stored:Option<String>=self.connection.query_row("SELECT relative_path FROM media_entities WHERE target_user_id=?1 ORDER BY entity_id LIMIT 1",[&id],|r|r.get(0)).optional()?;
                if let Some(relative) = stored {
                    media_path(&self.root, &relative)?;
                    match relative
                        .split('/')
                        .nth(1)
                        .filter(|part| part.starts_with('@'))
                    {
                        Some(folder) => media_path(&self.root, &format!("media/{folder}"))?,
                        None => self.root.join("media"),
                    }
                } else {
                    current
                }
            }
        } else {
            self.root.join("media")
        };
        check_path(&path)?;
        Ok(path)
    }
    fn naming(&self, entity: i64, refresh_account: bool) -> Result<Naming> {
        let (old, mime, cached, current): (String,String,String,String) = self.connection.query_row("SELECT e.relative_path,e.mime_type,e.naming_account,u.screen_name FROM media_entities e JOIN users u ON u.user_id=e.target_user_id WHERE entity_id=?1", [entity], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let mut q = self.connection.prepare("SELECT s.tweet_id,t.published_at,s.sequence,s.media_type,s.original_url FROM media_sources s JOIN tweets t USING(tweet_id) WHERE entity_id=?1")?;
        let rows = q
            .query_map([entity], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut rows = rows
            .into_iter()
            .map(|row| {
                let time = row
                    .1
                    .as_deref()
                    .map(|raw| source_time(&self.connection, raw))
                    .transpose()?
                    .flatten();
                Ok((time, row))
            })
            .collect::<Result<Vec<_>>>()?;
        rows.sort_by(|a, b| {
            a.0.is_none()
                .cmp(&b.0.is_none())
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.0.len().cmp(&b.1.0.len()))
                .then_with(|| a.1.0.cmp(&b.1.0))
                .then_with(|| a.1.2.cmp(&b.1.2))
        });
        let (_, (tweet, date, seq, kind, url)) = rows
            .into_iter()
            .next()
            .ok_or_else(|| invalid("媒体实体没有来源，无法整理"))?;
        let account = if refresh_account || cached.is_empty() {
            current
        } else {
            cached
        };
        let ext = detect_ext(&media_path(&self.root, &old)?, &url, &mime)?;
        let desired = build_path(
            &self.root,
            &self.connection,
            current_layout(&self.connection)?,
            &account,
            date.as_deref(),
            &tweet,
            sql_unsigned(seq)?.max(1),
            &kind,
            &ext,
        )?;
        Ok(Naming {
            old,
            account,
            kind,
            ext,
            desired,
        })
    }
    fn finish_rename(&mut self, entity: i64) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE media_entities SET relative_path=(SELECT new_path FROM media_rename_log WHERE entity_id=?1),kind=(SELECT kind FROM media_rename_log WHERE entity_id=?1),ext=(SELECT ext FROM media_rename_log WHERE entity_id=?1),naming_account=(SELECT naming_account FROM media_rename_log WHERE entity_id=?1) WHERE entity_id=?1", [entity])?;
        tx.execute(
            "UPDATE media_rename_log SET state='done' WHERE entity_id=?1",
            [entity],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn organize_entity(&mut self, entity: i64, refresh_account: bool) -> Result<bool> {
        let Naming {
            old,
            account,
            kind,
            ext,
            desired,
        } = self.naming(entity, refresh_account)?;
        let new = available_path(&self.root, &self.connection, &desired, Some(&old))?;
        if new == old {
            self.connection.execute(
                "UPDATE media_entities SET kind=?2,ext=?3,naming_account=?4 WHERE entity_id=?1",
                params![entity, kind, ext, account],
            )?;
            self.connection.execute(
                "DELETE FROM media_rename_log WHERE entity_id=?1 AND state<>'done'",
                [entity],
            )?;
            return Ok(false);
        }
        let old_path = media_path(&self.root, &old)?;
        let new_path = media_path(&self.root, &new)?;
        fs::create_dir_all(new_path.parent().ok_or_else(|| invalid("媒体目录无效"))?)?;
        check_path(&new_path)?;
        self.connection.execute("INSERT INTO media_rename_log VALUES(?1,?2,?3,?4,?5,?6,'planned') ON CONFLICT(entity_id) DO UPDATE SET old_path=excluded.old_path,new_path=excluded.new_path,kind=excluded.kind,ext=excluded.ext,naming_account=excluded.naming_account,state='planned'", params![entity,old,new,kind,ext,account])?;
        rename_noclobber(&old_path, &new_path)?;
        self.connection.execute(
            "UPDATE media_rename_log SET state='renamed' WHERE entity_id=?1",
            [entity],
        )?;
        self.finish_rename(entity)?;
        Ok(true)
    }
    pub(super) fn recover_renames(&mut self) -> Result<()> {
        let rows = {
            let mut q = self.connection.prepare("SELECT l.entity_id,l.old_path,l.new_path,e.byte_length,e.sha256 FROM media_rename_log l JOIN media_entities e USING(entity_id) WHERE state<>'done' ORDER BY l.entity_id")?;
            q.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (id, old, new, length, digest) in rows {
            let old = media_path(&self.root, &old)?;
            let new = media_path(&self.root, &new)?;
            match (old.try_exists()?, new.try_exists()?) {
                (false, true)
                    if new.is_file()
                        && new.metadata()?.len() == sql_unsigned(length)?
                        && hash_file(&new)? == digest =>
                {
                    self.finish_rename(id)?
                }
                (true, false) => {
                    self.connection.execute(
                        "UPDATE media_rename_log SET state='planned' WHERE entity_id=?1",
                        [id],
                    )?;
                }
                _ => {
                    return Err(invalid(
                        "重命名恢复发现文件缺失、冲突或内容变化；已保留文件和日志，请核对数据目录",
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn organize_media(
        &mut self,
        layout: Layout,
        cancel: &AtomicBool,
        mut notify: impl FnMut(LayoutProgress),
    ) -> LayoutProgress {
        let mut p = LayoutProgress {
            running: true,
            layout,
            stage: "备份索引".into(),
            ..Default::default()
        };
        notify(p.clone());
        let result = (|| -> Result<()> {
            self.recover_renames()?;
            p.backup = friendly_path(&self.backup_index()?)
                .to_string_lossy()
                .into_owned();
            self.connection.execute(
                "UPDATE storage_layout SET layout=?1 WHERE singleton=1",
                [match layout {
                    Layout::A => "A",
                    Layout::C => "C",
                }],
            )?;
            let ids = {
                let mut q = self
                    .connection
                    .prepare("SELECT entity_id FROM media_entities ORDER BY entity_id")?;
                q.query_map([], |r| r.get::<_, i64>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            p.total = ids.len() as u64;
            p.stage = "整理媒体文件".into();
            let mut last = std::time::Instant::now();
            notify(p.clone());
            for id in ids {
                if cancel.load(Ordering::Relaxed) {
                    p.stopped = true;
                    break;
                }
                p.current = self.connection.query_row(
                    "SELECT relative_path FROM media_entities WHERE entity_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                if self.organize_entity(id, true)? {
                    p.renamed += 1;
                }
                p.done += 1;
                if last.elapsed() >= Duration::from_millis(150) {
                    notify(p.clone());
                    last = std::time::Instant::now();
                }
            }
            cleanup_empty(&self.root.join("media"), true)?;
            Ok(())
        })();
        p.running = false;
        p.stage = if let Err(e) = result {
            p.error = Some(e.to_string());
            "整理未完成"
        } else if p.stopped {
            "已停止整理"
        } else {
            "整理完成"
        }
        .into();
        notify(p.clone());
        p
    }
    pub(super) fn untracked_media(&self) -> Result<Vec<String>> {
        let mut q = self
            .connection
            .prepare("SELECT relative_path FROM media_entities")?;
        let tracked = q
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<std::collections::HashSet<_>, _>>()?;
        Ok(media_files(&self.root)?
            .into_iter()
            .filter(|p| !tracked.contains(p))
            .collect())
    }
}
pub(crate) fn media_files(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        check_path(dir)?;
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            check_path(&path)?;
            if path.is_dir() {
                walk(root, &path, out)?;
            } else if path.is_file() {
                out.push(
                    path.strip_prefix(root)
                        .map_err(|_| invalid("媒体路径越界"))?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, &root.join("media"), &mut out)?;
    out.sort();
    Ok(out)
}
pub(super) fn cleanup_empty(dir: &Path, keep: bool) -> Result<()> {
    check_path(dir)?;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        check_path(&path)?;
        if path.is_dir() {
            cleanup_empty(&path, false)?;
        }
    }
    if !keep && fs::read_dir(dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}
fn rename_noclobber(old: &Path, new: &Path) -> Result<()> {
    check_path(old)?;
    check_path(new)?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let old: Vec<u16> = old.as_os_str().encode_wide().chain(Some(0)).collect();
        let new: Vec<u16> = new.as_os_str().encode_wide().chain(Some(0)).collect();
        // No replace-existing or copy-allowed flags: same-volume, no overwrite.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(old.as_ptr(), new.as_ptr(), 0)
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    #[cfg(not(windows))]
    {
        if new.try_exists()? {
            return Err(invalid("目标媒体文件已存在"));
        }
        fs::rename(old, new)?;
    }
    Ok(())
}

// ISO dates/timestamps (including timezone offsets) and the public Twitter format.
pub(crate) fn timestamp(db: &Connection, raw: &str) -> Result<Option<i64>> {
    if raw.len() >= 10
        && raw.is_char_boundary(10)
        && crate::archive::date_seconds(&raw[..10]).is_some()
    {
        return Ok(
            db.query_row("SELECT CAST(strftime('%s',?1) AS INTEGER)", [raw], |r| {
                r.get(0)
            })?,
        );
    }
    let p: Vec<_> = raw.split_whitespace().collect();
    if p.len() != 6 || p[4] != "+0000" {
        return Ok(None);
    }
    let Some(month) = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == p[1]) else {
        return Ok(None);
    };
    let Ok(day) = p[2].parse::<u32>() else {
        return Ok(None);
    };
    let date = format!("{}-{:02}-{day:02}", p[5], month + 1);
    let Some(midnight) = crate::archive::date_seconds(&date) else {
        return Ok(None);
    };
    let time: Vec<_> = p[3]
        .split(':')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<_, _>>()
        .unwrap_or_default();
    if time.len() != 3 || time[0] > 23 || time[1] > 59 || time[2] > 59 {
        return Ok(None);
    }
    Ok(Some(
        (midnight + time[0] * 3600 + time[1] * 60 + time[2]) as i64,
    ))
}

#[cfg(test)]
mod tests;

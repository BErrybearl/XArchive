use super::*;
use rusqlite::{OptionalExtension, params};

#[derive(Default, Debug)]
pub(super) struct Checkpoint {
    pub cursor: Option<String>,
    pub exhausted: bool,
    pub newest: Option<u64>,
    pub completed_at: Option<u64>,
    pub completed_newest: Option<u64>,
}
fn sql<T>(r: rusqlite::Result<T>) -> Result<T> {
    r.map_err(|_| PlatformError::local())
}
pub(super) fn checkpoint(store: &Store, target: &str, mode: &str) -> Result<Checkpoint> {
    Ok(sql(store.connection().query_row("SELECT cursor,exhausted,scan_newest,completed_at,completed_newest FROM archive_checkpoints WHERE target_user_id=?1 AND mode=?2",params![target,mode],|row|Ok(Checkpoint{cursor:row.get(0)?,exhausted:row.get(1)?,newest:unsigned(row,2)?,completed_at:unsigned(row,3)?,completed_newest:unsigned(row,4)?})).optional())?.unwrap_or_default())
}
pub(super) fn save_checkpoint(
    store: &Store,
    target: &Account,
    mode: &str,
    cp: &Checkpoint,
) -> Result<()> {
    // A transaction also supports an empty account timeline (no tweets have been saved).
    let tx = sql(store.connection().unchecked_transaction())?;
    sql(tx.execute("INSERT INTO users(user_id,screen_name,display_name) VALUES (?1,?2,?3) ON CONFLICT(user_id) DO NOTHING",params![target.user_id,target.screen_name,target.display_name]))?;
    sql(tx.execute("INSERT INTO archive_checkpoints(target_user_id,mode,cursor,exhausted,scan_newest,completed_at,completed_newest) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(target_user_id,mode) DO UPDATE SET cursor=excluded.cursor,exhausted=excluded.exhausted,scan_newest=excluded.scan_newest,completed_at=excluded.completed_at,completed_newest=excluded.completed_newest",params![target.user_id,mode,cp.cursor,cp.exhausted,cp.newest.map(|n|n as i64),cp.completed_at.map(|n|n as i64),cp.completed_newest.map(|n|n as i64)]))?;
    sql(tx.commit())
}
pub(super) fn known_tweet(store: &Store, target: &str, id: &str) -> Result<bool> {
    sql(store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM archive_tweets WHERE target_user_id=?1 AND tweet_id=?2)",
        params![target, id],
        |r| r.get(0),
    ))
}
pub(super) fn source_exists(
    store: &Store,
    target: &str,
    item: &Retry,
    p: &mut Progress,
) -> Result<bool> {
    let mut q=sql(store.connection().prepare("SELECT s.media_key,s.original_filename,s.original_url,e.relative_path,e.byte_length FROM media_sources s JOIN media_entities e ON e.entity_id=s.entity_id WHERE s.target_user_id=?1 AND s.tweet_id=?2"))?;
    let rows = sql(q.query_map(params![target, item.tweet.id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
        ))
    }))?;
    for row in rows {
        let (key, filename, url, path, length) = sql(row)?;
        if key == item.media.key
            || (key.starts_with("legacy-filename:") && filename == item.media.legacy_filename)
            || url == item.media.url
        {
            let path = storage::media_path(store_root(store), &path)
                .map_err(|_| PlatformError::local())?;
            if !path.is_file()
                || path.metadata().map_err(|_| PlatformError::local())?.len()
                    != u64::try_from(length).map_err(|_| PlatformError::local())?
            {
                if p.repair_logged
                    .insert((item.tweet.id.clone(), item.media.key.clone()))
                {
                    append_log(
                        p,
                        format!("本地文件缺失，重新下载 · 推文 {}", item.tweet.id),
                    );
                }
                return Ok(false);
            }
            return Ok(true);
        }
    }
    Ok(false)
}
fn store_root(store: &Store) -> &Path {
    store.root()
}
pub(super) fn queue(store: &Store, target: &str, item: &Retry) -> Result<bool> {
    let payload = serde_json::to_string(item).map_err(|_| PlatformError::local())?;
    Ok(sql(store.connection().execute("INSERT INTO archive_retries(target_user_id,tweet_id,media_key,media_type,payload,reason) VALUES (?1,?2,?3,?4,?5,'等待下载') ON CONFLICT DO NOTHING",params![target,item.tweet.id,item.media.key,item.media.kind,payload]))? > 0)
}
pub(super) fn clear_retry(store: &Store, target: &str, item: &Retry) -> Result<()> {
    sql(store.connection().execute(
        "DELETE FROM archive_retries WHERE target_user_id=?1 AND tweet_id=?2 AND media_key=?3",
        params![target, item.tweet.id, item.media.key],
    ))?;
    Ok(())
}
pub(super) fn retry_attempts(store: &Store, target: &str, item: &Retry) -> Result<Option<u32>> {
    sql(store.connection().query_row("SELECT attempts FROM archive_retries WHERE target_user_id=?1 AND tweet_id=?2 AND media_key=?3",params![target,item.tweet.id,item.media.key],|r|r.get(0)).optional())
}
pub(super) fn fail_retry(
    store: &Store,
    target: &str,
    item: &Retry,
    reason: &str,
    limit: u32,
) -> Result<()> {
    sql(store.connection().execute("UPDATE archive_retries SET attempts=attempts+1,reason=?4,status=CASE WHEN attempts+1>=?5 THEN 'failed' ELSE 'pending' END WHERE target_user_id=?1 AND tweet_id=?2 AND media_key=?3",params![target,item.tweet.id,item.media.key,reason,limit]))?;
    Ok(())
}
pub(super) fn retries(store: &Store, target: &str) -> Result<Vec<Retry>> {
    let mut q=sql(store.connection().prepare("SELECT payload FROM archive_retries WHERE target_user_id=?1 AND status='pending' ORDER BY tweet_id,media_key"))?;
    let rows = sql(q.query_map([target], |r| r.get::<_, String>(0)))?;
    let mut result = Vec::new();
    for row in rows {
        result.push(serde_json::from_str(&sql(row)?).map_err(|_| PlatformError::local())?);
    }
    Ok(result)
}
pub(super) fn failures(store: &Store, target: &str, options: &Options) -> Result<Vec<Failure>> {
    let mut q=sql(store.connection().prepare("SELECT tweet_id,media_type,attempts,reason,status,payload FROM archive_retries WHERE target_user_id=?1 ORDER BY tweet_id,media_key"))?;
    let rows = sql(q.query_map([target], |r| {
        Ok((
            Failure {
                tweet_id: r.get(0)?,
                kind: r.get(1)?,
                attempts: r.get(2)?,
                reason: r.get(3)?,
                terminal: r.get::<_, String>(4)? == "failed",
            },
            r.get::<_, String>(5)?,
        ))
    }))?;
    let mut result = Vec::new();
    for row in rows {
        let (failure, payload) = sql(row)?;
        let item: Retry = serde_json::from_str(&payload).map_err(|_| PlatformError::local())?;
        if options.mode == Mode::Retry
            || (options.accepts(&failure.kind)
                && (options.preferences.include_retweets || !item.tweet.retweet)
                && options.preferences.within_dates(&item.tweet))
        {
            result.push(failure);
        }
    }
    Ok(result)
}

fn unsigned(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|n| u64::try_from(n).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, n)))
        .transpose()
}

pub(super) fn normalize_retries(store: &Store, target: &str, limit: u32) -> Result<()> {
    sql(store.connection().execute("UPDATE archive_retries SET status=CASE WHEN attempts>=?2 THEN 'failed' ELSE 'pending' END WHERE target_user_id=?1",params![target,limit]))?;
    Ok(())
}
pub(super) fn record_run(
    store: &Store,
    target: &Account,
    options: &Options,
    p: &Progress,
    ok: bool,
) -> Result<()> {
    sql(store.connection().execute("INSERT INTO users(user_id,screen_name,display_name) VALUES (?1,?2,?3) ON CONFLICT(user_id) DO UPDATE SET screen_name=excluded.screen_name,display_name=excluded.display_name",params![target.user_id,target.screen_name,target.display_name]))?;
    let full = options.mode == Mode::Full;
    let scope = if ok && p.outcome == "completed" {
        Some(options.baseline_key())
    } else {
        None
    };
    let status = if !ok {
        "已停止或未完成".to_owned()
    } else {
        let label = match p.outcome.as_str() {
            "completed" => "全量完成",
            "updated" => "增量完成",
            "date_range" => "按日期范围结束",
            _ => "未完成",
        };
        if p.failed > 0 && matches!(p.outcome.as_str(), "completed" | "updated") {
            format!("{label}（{} 项失败）", p.failed)
        } else {
            label.to_owned()
        }
    };
    let mut summary = format!(
        "{status} · {} 页 · 新增 {} · 复用 {} · 待重试 {} · 失败 {}",
        p.pages, p.added, p.reused, p.pending, p.failed
    );
    if p.date_ended && p.outcome != "date_range" {
        summary.push_str(" · 按日期范围结束扫描");
    }
    sql(store.connection().execute("INSERT INTO archive_runs(target_user_id,last_full_at,last_incremental_at,full_scope,summary) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(target_user_id) DO UPDATE SET last_full_at=coalesce(excluded.last_full_at,last_full_at),last_incremental_at=coalesce(excluded.last_incremental_at,last_incremental_at),full_scope=coalesce(excluded.full_scope,full_scope),summary=excluded.summary",params![target.user_id,if full {Some(platform::client::now() as i64)} else {None},if full {None} else {Some(platform::client::now() as i64)},scope,summary]))?;
    if scope.is_some() {
        let settings = Settings {
            target: target.screen_name.clone(),
            include_timeline: options.include_timeline,
            preferences: options.preferences.clone(),
        };
        sql(store.connection().execute(
            "UPDATE archive_runs SET full_settings_json=?2 WHERE target_user_id=?1",
            params![
                target.user_id,
                serde_json::to_string(&settings).map_err(|_| PlatformError::local())?
            ],
        ))?;
    }
    Ok(())
}

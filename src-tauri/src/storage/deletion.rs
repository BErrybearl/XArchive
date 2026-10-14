use super::*;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    Images,
    Videos,
    Gifs,
    TextTweets,
    All,
}
impl DeleteScope {
    fn kind(self) -> Option<&'static str> {
        match self {
            Self::Images => Some("image"),
            Self::Videos => Some("video"),
            Self::Gifs => Some("gif"),
            _ => None,
        }
    }
}
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DeleteOptions {
    pub disable_type: bool,
    pub confirmation: Option<String>,
    pub preview_token: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct DeletePreview {
    pub sources: u64,
    pub tweet_metadata: u64,
    pub tweet_links: u64,
    pub files: u64,
    pub bytes: u64,
    pub retained_files: u64,
    pub token: String,
}
#[derive(Debug, Serialize)]
pub struct DeleteResult {
    pub deleted: DeletePreview,
    pub backup: String,
    pub orphaned_files: Vec<String>,
}
#[derive(Debug)]
struct Entity {
    id: i64,
    relative: String,
    length: u64,
    remove: bool,
}
struct Plan {
    preview: DeletePreview,
    sources: Vec<i64>,
    tweets: Vec<String>,
    entities: Vec<Entity>,
    users: HashSet<String>,
}
impl Store {
    fn delete_plan(&self, account: &str, scope: DeleteScope) -> Result<Plan> {
        let id = self.account_id(account)?.unwrap_or_default();
        let mut plan = Plan {
            preview: Default::default(),
            sources: vec![],
            tweets: vec![],
            entities: vec![],
            users: HashSet::new(),
        };
        plan.users.insert(id.clone());
        let mut affected = HashSet::new();
        let mut q = self.connection.prepare(
            "SELECT source_id,entity_id,media_type FROM media_sources WHERE target_user_id=?1",
        )?;
        for row in q.query_map([&id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (source, entity, kind) = row?;
            if scope == DeleteScope::All || scope.kind() == Some(kind.as_str()) {
                plan.sources.push(source);
                affected.insert(entity);
            }
        }
        if scope == DeleteScope::All {
            let mut q = self
                .connection
                .prepare("SELECT entity_id FROM media_entities WHERE target_user_id=?1")?;
            for row in q.query_map([&id], |r| r.get(0))? {
                affected.insert(row?);
            }
        }
        let removed: HashSet<_> = plan.sources.iter().copied().collect();
        for entity in affected {
            let (relative, length): (String, i64) = self.connection.query_row(
                "SELECT relative_path,byte_length FROM media_entities WHERE entity_id=?1",
                [entity],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            // Validate every affected path before either backup or index mutation.
            media_path(&self.root, &relative)?;
            let mut q = self
                .connection
                .prepare("SELECT source_id FROM media_sources WHERE entity_id=?1")?;
            let remaining = q
                .query_map([entity], |r| r.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .iter()
                .any(|s| !removed.contains(s));
            let length = sql_unsigned(length)?;
            if !remaining {
                plan.preview.files += 1;
                plan.preview.bytes += length;
            } else {
                plan.preview.retained_files += 1;
            }
            plan.entities.push(Entity {
                id: entity,
                relative,
                length,
                remove: !remaining,
            });
        }
        if matches!(scope, DeleteScope::All | DeleteScope::TextTweets) {
            let mut q=self.connection.prepare("SELECT a.tweet_id,t.author_user_id FROM archive_tweets a JOIN tweets t USING(tweet_id) WHERE a.target_user_id=?1 AND (?2 OR (NOT EXISTS(SELECT 1 FROM media_sources s WHERE s.tweet_id=a.tweet_id) AND NOT EXISTS(SELECT 1 FROM archive_retries r WHERE r.tweet_id=a.tweet_id)))")?;
            for row in q.query_map(params![id, scope == DeleteScope::All], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })? {
                let (tweet, author) = row?;
                let other:bool=self.connection.query_row("SELECT EXISTS(SELECT 1 FROM archive_tweets WHERE tweet_id=?1 AND target_user_id<>?2)",params![tweet,id],|r|r.get(0))?;
                if !other {
                    plan.preview.tweet_metadata += 1;
                }
                plan.tweets.push(tweet);
                plan.users.insert(author);
            }
        }
        plan.preview.sources = plan.sources.len() as u64;
        plan.preview.tweet_links = plan.tweets.len() as u64;
        plan.sources.sort_unstable();
        plan.tweets.sort();
        plan.entities.sort_by_key(|e| e.id);
        let settings = self.account_settings(account, &crate::archive::Settings::default())?;
        let signature = format!(
            "{}:{id}:{scope:?}:{:?}:{:?}:{:?}:{}:{}",
            self.root.canonicalize()?.display(),
            plan.sources,
            plan.tweets,
            plan.entities,
            serde_json::to_string(&plan.preview).map_err(|_| invalid("无法生成删除预览"))?,
            serde_json::to_string(&settings).map_err(|_| invalid("无法生成删除预览"))?
        );
        plan.preview.token = hex_digest(&Sha256::digest(signature.as_bytes()));
        Ok(plan)
    }
    pub fn preview_account_delete(
        &self,
        account: &str,
        scope: DeleteScope,
    ) -> Result<DeletePreview> {
        Ok(self.delete_plan(account, scope)?.preview)
    }

    pub fn backup_index(&self) -> Result<PathBuf> {
        let dir = self.root.join("backups");
        check_path(&dir)?;
        fs::create_dir_all(&dir)?;
        let mut staged = NamedTempFile::new_in(&dir)?;
        {
            let mut c = Connection::open(staged.path())?;
            rusqlite::backup::Backup::new(&self.connection, &mut c)?.run_to_completion(
                64,
                Duration::from_millis(5),
                None,
            )?;
        }
        staged.as_file_mut().sync_all()?;
        let mut offset = 0;
        let destination = loop {
            let stamp: String = self.connection.query_row(
                "SELECT strftime('%Y%m%d-%H%M%S','now','localtime',?1)",
                [format!("+{offset} seconds")],
                |r| r.get(0),
            )?;
            let p = dir.join(format!("index-{stamp}.sqlite3"));
            check_path(&p)?;
            if !p.exists() {
                break p;
            }
            offset += 1;
        };
        staged
            .persist_noclobber(&destination)
            .map_err(|e| StorageError::Io(e.error))?;
        let dir = dir.canonicalize()?;
        let mut files = vec![];
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let n = entry.file_name().to_string_lossy().into_owned();
            if n.len() == 29
                && n.starts_with("index-")
                && n.ends_with(".sqlite3")
                && n.as_bytes()[14] == b'-'
                && n.as_bytes()[6..14]
                    .iter()
                    .chain(n.as_bytes()[15..21].iter())
                    .all(|b| b.is_ascii_digit())
            {
                check_path(&entry.path())?;
                if entry.file_type()?.is_file() {
                    files.push(entry.path());
                }
            }
        }
        files.sort();
        let old = files.len().saturating_sub(5);
        let current = destination.canonicalize()?;
        for path in files.into_iter().filter(|p| p != &current).take(old) {
            check_path(&path)?;
            let resolved = path.canonicalize()?;
            if resolved.parent() != Some(dir.as_path()) {
                return Err(invalid("备份路径越界，未清理"));
            }
            fs::remove_file(resolved)?;
        }
        Ok(destination)
    }
    pub fn delete_account_data(
        &mut self,
        account: &str,
        scope: DeleteScope,
        options: &DeleteOptions,
    ) -> Result<DeleteResult> {
        self.delete_account_with(account, scope, options, |p| fs::remove_file(p))
    }
    fn delete_account_with(
        &mut self,
        account: &str,
        scope: DeleteScope,
        options: &DeleteOptions,
        mut remove: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<DeleteResult> {
        let n = crate::platform::parser::target_name(account)
            .map_err(|_| invalid("账号格式无效"))?
            .to_ascii_lowercase();
        if scope == DeleteScope::All
            && options
                .confirmation
                .as_deref()
                .is_none_or(|s| !s.trim().trim_start_matches('@').eq_ignore_ascii_case(&n))
        {
            return Err(invalid("请输入账号名确认移除"));
        }
        let id = self.account_id(account)?.unwrap_or_default();
        let plan = self.delete_plan(account, scope)?;
        if options
            .preview_token
            .as_ref()
            .is_some_and(|token| token != &plan.preview.token)
        {
            return Err(invalid("数据或目录已变化，请重新预览后确认删除"));
        }
        let mut settings = self.account_settings(account, &crate::archive::Settings::default())?;
        if options.disable_type {
            match scope {
                DeleteScope::TextTweets => settings.include_timeline = false,
                DeleteScope::Images | DeleteScope::Videos | DeleteScope::Gifs => {
                    let m = settings
                        .preferences
                        .media_types
                        .get_or_insert_with(crate::archive::MediaTypes::default);
                    match scope {
                        DeleteScope::Images => m.image = false,
                        DeleteScope::Videos => m.video = false,
                        _ => m.gif = false,
                    }
                }
                _ => {}
            }
        }
        let backup = self.backup_index()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for source in &plan.sources {
            tx.execute("DELETE FROM media_sources WHERE source_id=?1", [source])?;
        }
        // Drop stale references in the index only. Historical output files remain untouched.
        tx.execute(
            "DELETE FROM export_files WHERE export_id IN
            (SELECT export_id FROM export_directories WHERE target_user_id=?1)
            AND NOT EXISTS(SELECT 1 FROM media_sources s
                WHERE s.target_user_id=?1 AND s.entity_id=export_files.entity_id)",
            [&id],
        )?;
        if scope == DeleteScope::All {
            tx.execute(
                "DELETE FROM export_directories WHERE target_user_id=?1",
                [&id],
            )?;
        }
        if scope == DeleteScope::All {
            tx.execute("DELETE FROM archive_retries WHERE target_user_id=?1", [&id])?;
        } else if let Some(kind) = scope.kind() {
            tx.execute(
                "DELETE FROM archive_retries WHERE target_user_id=?1 AND media_type=?2",
                params![id, kind],
            )?;
        }
        for entity in &plan.entities {
            if entity.remove {
                tx.execute("DELETE FROM export_files WHERE entity_id=?1", [entity.id])?;
                tx.execute("DELETE FROM media_entities WHERE entity_id=?1", [entity.id])?;
            } else if scope == DeleteScope::All {
                tx.execute("UPDATE media_entities SET target_user_id=(SELECT target_user_id FROM media_sources WHERE entity_id=?1 ORDER BY source_id LIMIT 1) WHERE entity_id=?1 AND target_user_id=?2",params![entity.id,id])?;
            }
        }
        for tweet in &plan.tweets {
            tx.execute(
                "DELETE FROM archive_tweets WHERE target_user_id=?1 AND tweet_id=?2",
                params![id, tweet],
            )?;
            tx.execute("DELETE FROM tweets WHERE tweet_id=?1 AND NOT EXISTS(SELECT 1 FROM archive_tweets WHERE tweet_id=?1) AND NOT EXISTS(SELECT 1 FROM media_sources WHERE tweet_id=?1) AND NOT EXISTS(SELECT 1 FROM archive_retries WHERE tweet_id=?1)",[tweet])?;
        }
        // Discard full cursors as well as completion: resuming past deleted data would miss it.
        tx.execute("DELETE FROM archive_checkpoints WHERE target_user_id=?1 AND (?2 OR mode LIKE 'full:%')",params![id,scope==DeleteScope::All])?;
        if scope == DeleteScope::All {
            tx.execute("DELETE FROM archive_runs WHERE target_user_id=?1", [&id])?;
            tx.execute(
                "DELETE FROM account_profiles WHERE screen_name=?1 OR target_user_id=?2",
                params![n, id],
            )?;
            tx.execute("UPDATE workspace_state SET selected_account='' WHERE selected_account=?1 COLLATE NOCASE",[&n])?;
            tx.execute("DELETE FROM archives WHERE target_user_id=?1", [&id])?;
            for user in &plan.users {
                tx.execute("DELETE FROM users WHERE user_id=?1 AND NOT EXISTS(SELECT 1 FROM tweets WHERE author_user_id=?1) AND NOT EXISTS(SELECT 1 FROM archives WHERE target_user_id=?1) AND NOT EXISTS(SELECT 1 FROM archive_checkpoints WHERE target_user_id=?1) AND NOT EXISTS(SELECT 1 FROM archive_runs WHERE target_user_id=?1) AND NOT EXISTS(SELECT 1 FROM account_profiles WHERE target_user_id=?1)",[user])?;
            }
        } else {
            tx.execute("UPDATE archive_runs SET full_scope=NULL,full_settings_json=NULL WHERE target_user_id=?1",[&id])?;
            tx.execute("INSERT INTO account_profiles VALUES (?1,?2,?3) ON CONFLICT(screen_name) DO UPDATE SET settings_json=excluded.settings_json",params![n,if id.is_empty(){None}else{Some(&id)},serde_json::to_string(&settings).map_err(|_|invalid("无法保存账号选项"))?])?;
        }
        tx.commit()?;
        let mut orphaned = vec![];
        for entity in plan.entities.into_iter().filter(|e| e.remove) {
            let _length = entity.length;
            match media_path(&self.root, &entity.relative) {
                Ok(path) => {
                    if let Err(e) = remove(&path) {
                        if e.kind() != io::ErrorKind::NotFound {
                            orphaned.push(entity.relative);
                        }
                    }
                }
                Err(_) => orphaned.push(entity.relative),
            }
        }
        Ok(DeleteResult {
            deleted: plan.preview,
            backup: friendly_path(&backup).to_string_lossy().into_owned(),
            orphaned_files: orphaned,
        })
    }
}

#[cfg(test)]
mod tests;

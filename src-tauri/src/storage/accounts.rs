use super::*;

#[derive(Debug, Serialize)]
pub struct ArchivedAccount {
    pub user_id: String,
    pub screen_name: String,
    pub display_name: String,
    pub media: i64,
    pub pending: i64,
    pub failed: i64,
    pub last_full_at: Option<i64>,
    pub last_incremental_at: Option<i64>,
    pub full_scope: Option<String>,
    pub summary: String,
    pub tweets: i64,
    pub bytes: i64,
    pub full_settings: Option<crate::archive::Settings>,
}
fn list_existing_accounts(root: &Path) -> Result<Vec<ArchivedAccount>> {
    check_path(root)?;
    let path = root.join(DATABASE);
    check_path(&path)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    validate_database(&c)?;
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let targets = if version >= 2 {
        "SELECT target_user_id FROM archives UNION SELECT target_user_id FROM archive_checkpoints"
    } else {
        "SELECT target_user_id FROM archives"
    };
    let mut q=c.prepare(&format!("SELECT u.user_id,u.screen_name,coalesce(u.display_name,u.screen_name),(SELECT count(*) FROM media_entities e WHERE e.target_user_id=u.user_id) FROM users u JOIN ({targets}) a ON a.target_user_id=u.user_id ORDER BY u.screen_name COLLATE NOCASE"))?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut accounts = Vec::new();
    for row in rows {
        let (id, name, display, media) = row?;
        let (pending, failed) = if version >= 2 {
            c.query_row("SELECT count(*) FILTER(WHERE status='pending'),count(*) FILTER(WHERE status='failed') FROM archive_retries WHERE target_user_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?)))?
        } else {
            (0, 0)
        };
        let (mut full, incremental, mut scope, mut summary) = if version >= 3 {
            c.query_row("SELECT last_full_at,last_incremental_at,full_scope,summary FROM archive_runs WHERE target_user_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.unwrap_or((None,None,None,String::new()))
        } else {
            (None, None, None, String::new())
        };
        // Completed v2 checkpoints remain valid after the runs table is introduced.
        if scope.is_none() && version >= 2 {
            let prior: Option<(String,i64)> = c.query_row("SELECT mode,completed_at FROM archive_checkpoints WHERE target_user_id=?1 AND mode LIKE 'full:UserMedia:%' AND completed_at IS NOT NULL ORDER BY completed_at DESC LIMIT 1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((mode, at)) = prior {
                let key = mode.strip_prefix("full:UserMedia:").unwrap_or("all");
                let timeline:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM archive_checkpoints WHERE target_user_id=?1 AND mode=?2 AND completed_at=?3)",params![id,format!("full:UserTweets:{key}"),at],|r|r.get(0))?;
                scope = Some(format!("{key}:timeline{}", timeline as u8));
                if full.is_none() {
                    full = Some(at);
                }
                if summary.is_empty() {
                    summary = "上次全量已完成".into();
                }
            }
        }
        accounts.push(ArchivedAccount {
            user_id: id.clone(),
            screen_name: name,
            display_name: display,
            media,
            pending,
            failed,
            last_full_at: full,
            last_incremental_at: incremental,
            full_scope: scope,
            summary,
            tweets: c.query_row("SELECT count(*) FROM archive_tweets WHERE target_user_id=?1", [&id], |r| r.get(0))?,
            bytes: c.query_row("SELECT coalesce(sum(byte_length),0) FROM media_entities WHERE entity_id IN (SELECT entity_id FROM media_sources WHERE target_user_id=?1)", [&id], |r| r.get(0))?,
            full_settings: if version >= 4 {
                c.query_row("SELECT full_settings_json FROM archive_runs WHERE target_user_id=?1", [&id], |r| r.get::<_,Option<String>>(0)).optional()?.flatten().map(|s| serde_json::from_str(&s).map_err(|_|invalid("账号全量选项格式无效"))).transpose()?
            } else {None},
        });
    }
    Ok(accounts)
}

fn name(raw: &str) -> Result<String> {
    crate::platform::parser::target_name(raw)
        .map(|s| s.to_ascii_lowercase())
        .map_err(|e| StorageError::Invalid(e.message.into()))
}
fn encode(settings: &crate::archive::Settings) -> Result<String> {
    serde_json::to_string(settings).map_err(|_| invalid("无法保存账号选项"))
}
fn decode(raw: &str) -> Result<crate::archive::Settings> {
    serde_json::from_str(raw).map_err(|_| invalid("账号选项格式无效"))
}
pub fn list_archived_accounts(root: &Path) -> Result<Vec<ArchivedAccount>> {
    let mut accounts = list_existing_accounts(root)?;
    let db = root.join(DATABASE);
    if !db.exists() {
        return Ok(accounts);
    }
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for a in &mut accounts {
        a.media = c.query_row(
            "SELECT count(DISTINCT entity_id) FROM media_sources WHERE target_user_id=?1",
            [&a.user_id],
            |r| r.get(0),
        )?;
    }
    if version >= 4 {
        let mut q = c.prepare(
            "SELECT screen_name,target_user_id FROM account_profiles ORDER BY screen_name COLLATE NOCASE",
        )?;
        for row in q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })? {
            let (n, id) = row?;
            if !accounts.iter().any(|a| {
                a.screen_name.eq_ignore_ascii_case(&n) || id.as_deref() == Some(a.user_id.as_str())
            }) {
                accounts.push(ArchivedAccount {
                    user_id: String::new(),
                    screen_name: n,
                    display_name: String::new(),
                    media: 0,
                    pending: 0,
                    failed: 0,
                    last_full_at: None,
                    last_incremental_at: None,
                    full_scope: None,
                    summary: String::new(),
                    tweets: 0,
                    bytes: 0,
                    full_settings: None,
                });
            }
        }
    }
    accounts.sort_by_key(|a| a.screen_name.to_ascii_lowercase());
    Ok(accounts)
}

#[derive(Debug, Serialize)]
pub struct AccountDataRow {
    pub scope: DeleteScope,
    pub sources: i64,
    pub files: i64,
    pub bytes: i64,
}
impl Store {
    pub fn workspace_settings(
        &self,
        fallback: &crate::archive::Settings,
    ) -> Result<crate::archive::Settings> {
        if self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?
            < 4
        {
            return Ok(fallback.clone());
        }
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT selected_account,last_settings_json FROM workspace_state WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((n, s)) = row {
            let mut settings = decode(&s)?;
            settings.target = n;
            Ok(settings)
        } else {
            Ok(fallback.clone())
        }
    }
    pub fn account_settings(
        &self,
        account: &str,
        fallback: &crate::archive::Settings,
    ) -> Result<crate::archive::Settings> {
        let n = name(account)?;
        if self
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?
            < 4
        {
            let mut s = fallback.clone();
            s.target = n;
            return Ok(s);
        }
        check_profile_username(&self.connection, &n)?;
        let mut raw: Option<String> = self
            .connection
            .query_row(
                "SELECT settings_json FROM account_profiles WHERE screen_name=?1",
                [&n],
                |r| r.get(0),
            )
            .optional()?;
        if raw.is_none() {
            raw=self.connection.query_row("SELECT p.settings_json FROM account_profiles p JOIN users u ON u.user_id=p.target_user_id WHERE u.screen_name=?1 COLLATE NOCASE",[&n],|r|r.get(0)).optional()?;
        }
        let mut s = if let Some(raw) = raw {
            decode(&raw)?
        } else {
            self.workspace_settings(fallback)?
        };
        s.target = n;
        Ok(s)
    }
    pub fn save_account_settings(&mut self, settings: &crate::archive::Settings) -> Result<()> {
        settings
            .preferences
            .validate_fields()
            .map_err(StorageError::Invalid)?;
        let n = name(&settings.target)?;
        let mut s = settings.clone();
        s.target = n.clone();
        let raw = encode(&s)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id:Option<String>=tx.query_row("SELECT user_id FROM users WHERE screen_name=?1 COLLATE NOCASE AND (user_id IN (SELECT target_user_id FROM archives) OR user_id IN (SELECT target_user_id FROM archive_checkpoints))",[&n],|r|r.get(0)).optional()?;
        if let Some(id) = &id {
            ensure_profile_identity(&tx, &n, id)?;
            tx.execute("DELETE FROM account_profiles WHERE target_user_id=?1 AND screen_name<>?2 COLLATE NOCASE",params![id,n])?;
        }
        tx.execute("INSERT INTO account_profiles(screen_name,target_user_id,settings_json) VALUES (?1,?2,?3) ON CONFLICT(screen_name) DO UPDATE SET target_user_id=coalesce(excluded.target_user_id,account_profiles.target_user_id),settings_json=excluded.settings_json",params![n,id,raw])?;
        tx.execute("INSERT INTO workspace_state VALUES (1,?1,?2) ON CONFLICT(id) DO UPDATE SET selected_account=excluded.selected_account,last_settings_json=excluded.last_settings_json",params![n,raw])?;
        tx.commit()?;
        Ok(())
    }
    pub fn add_account(
        &mut self,
        raw: &str,
        fallback: &crate::archive::Settings,
    ) -> Result<crate::archive::Settings> {
        let settings = self.account_settings(raw, fallback)?;
        self.save_account_settings(&settings)?;
        Ok(settings)
    }
    pub fn bind_account(
        &mut self,
        account: &crate::auth::Account,
        settings: &crate::archive::Settings,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n = name(&account.screen_name)?;
        ensure_profile_identity(&tx, &n, &account.user_id)?;
        let mut canonical = settings.clone();
        canonical.target = n.clone();
        let raw = encode(&canonical)?;
        tx.execute("INSERT INTO users VALUES (?1,?2,?3) ON CONFLICT(user_id) DO UPDATE SET screen_name=excluded.screen_name,display_name=excluded.display_name",params![account.user_id,account.screen_name,account.display_name])?;
        tx.execute(
            "INSERT INTO archives(target_user_id) VALUES (?1) ON CONFLICT DO NOTHING",
            [&account.user_id],
        )?;
        // Rename a registration by stable identity; an unbound new-name registration can merge.
        tx.execute("UPDATE workspace_state SET selected_account=?1,last_settings_json=?3 WHERE selected_account IN (SELECT screen_name FROM account_profiles WHERE target_user_id=?2) OR selected_account=?1 COLLATE NOCASE",params![n,account.user_id,raw])?;
        tx.execute("DELETE FROM account_profiles WHERE target_user_id=?1 AND screen_name<>?2 COLLATE NOCASE",params![account.user_id,n])?;
        tx.execute("INSERT INTO account_profiles VALUES (?1,?2,?3) ON CONFLICT(screen_name) DO UPDATE SET target_user_id=excluded.target_user_id,settings_json=excluded.settings_json",params![n,account.user_id,raw])?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn account_id(&self, account: &str) -> Result<Option<String>> {
        let n = name(account)?;
        check_profile_username(&self.connection, &n)?;
        let id: Option<String> = self
            .connection
            .query_row(
                "SELECT target_user_id FROM account_profiles WHERE screen_name=?1",
                [&n],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let id = if id.is_some() {
            id
        } else {
            self.connection.query_row("SELECT user_id FROM users WHERE screen_name=?1 COLLATE NOCASE AND (user_id IN (SELECT target_user_id FROM archives) OR user_id IN (SELECT target_user_id FROM archive_checkpoints))",[&n],|r|r.get(0)).optional()?
        };
        let registered: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM account_profiles WHERE screen_name=?1)",
            [n],
            |r| r.get(0),
        )?;
        if id.is_none() && !registered {
            return Err(invalid("账号尚未登记"));
        }
        Ok(id)
    }
    pub fn account_data(&self, account: &str) -> Result<Vec<AccountDataRow>> {
        let id = self.account_id(account)?.unwrap_or_default();
        let mut rows = Vec::new();
        for (scope, kind) in [
            (DeleteScope::Images, "image"),
            (DeleteScope::Videos, "video"),
            (DeleteScope::Gifs, "gif"),
        ] {
            let sources = self.connection.query_row(
                "SELECT count(*) FROM media_sources WHERE target_user_id=?1 AND media_type=?2",
                params![id, kind],
                |r| r.get(0),
            )?;
            let (files,bytes)=self.connection.query_row("SELECT count(*),coalesce(sum(byte_length),0) FROM media_entities WHERE entity_id IN (SELECT entity_id FROM media_sources WHERE target_user_id=?1 AND media_type=?2)",params![id,kind],|r|Ok((r.get(0)?,r.get(1)?)))?;
            rows.push(AccountDataRow {
                scope,
                sources,
                files,
                bytes,
            });
        }
        let sources=self.connection.query_row("SELECT count(*) FROM archive_tweets a WHERE target_user_id=?1 AND NOT EXISTS(SELECT 1 FROM media_sources s WHERE s.tweet_id=a.tweet_id) AND NOT EXISTS(SELECT 1 FROM archive_retries r WHERE r.tweet_id=a.tweet_id)",[id],|r|r.get(0))?;
        rows.push(AccountDataRow {
            scope: DeleteScope::TextTweets,
            sources,
            files: 0,
            bytes: 0,
        });
        Ok(rows)
    }
    pub fn inspect_account(&self, account: &str) -> Result<IntegrityReport> {
        let id = self.account_id(account)?.unwrap_or_default();
        let mut report = IntegrityReport {
            missing_files: vec![],
            changed_files: vec![],
            untracked_files: vec![],
            temporary_files: 0,
        };
        let mut q=self.connection.prepare("SELECT relative_path,byte_length,sha256 FROM media_entities WHERE entity_id IN (SELECT entity_id FROM media_sources WHERE target_user_id=?1)")?;
        for row in q.query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (relative, length, digest) = row?;
            let path = media_path(&self.root, &relative)?;
            if !path.is_file() {
                report.missing_files.push(relative);
            } else if path.metadata()?.len() != sql_unsigned(length)? || hash_file(&path)? != digest
            {
                report.changed_files.push(relative);
            }
        }
        Ok(report)
    }
    pub fn reset_account_progress(&self, account: &str) -> Result<()> {
        let id = self.account_id(account)?.unwrap_or_default();
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM archive_checkpoints WHERE target_user_id=?1 AND mode LIKE 'full:%'",
            [&id],
        )?;
        tx.execute("UPDATE archive_runs SET full_scope=NULL,full_settings_json=NULL WHERE target_user_id=?1",[id])?;
        tx.commit()?;
        Ok(())
    }
}

fn ensure_profile_identity(tx: &rusqlite::Transaction<'_>, n: &str, id: &str) -> Result<()> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT target_user_id FROM account_profiles WHERE screen_name=?1",
            [n],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if existing.as_deref().is_some_and(|old| old != id) {
        return Err(invalid(
            "该用户名已登记为其他账号，已保留原数据，请先核对账号身份",
        ));
    }
    Ok(())
}

fn check_profile_username(c: &Connection, n: &str) -> Result<()> {
    let bound: Option<String> = c
        .query_row(
            "SELECT target_user_id FROM account_profiles WHERE screen_name=?1",
            [n],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    if let Some(id) = bound {
        let conflict:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE screen_name=?1 COLLATE NOCASE AND user_id<>?2 AND (user_id IN (SELECT target_user_id FROM archives) OR user_id IN (SELECT target_user_id FROM archive_checkpoints)))",params![n,id],|r|r.get(0))?;
        if conflict {
            return Err(invalid(
                "该用户名的登记与归档身份不一致，请先核对账号名，已保留原数据",
            ));
        }
    }
    Ok(())
}

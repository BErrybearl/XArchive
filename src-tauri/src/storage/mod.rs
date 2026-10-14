mod accounts;
mod deletion;
pub use accounts::*;
pub use deletion::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tempfile::NamedTempFile;

const DATABASE: &str = "archive.sqlite3";
const APPLICATION_ID: i64 = 1_480_675_907;
const SCHEMA_VERSION: i64 = 5;
const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Sql(rusqlite::Error),
    Invalid(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "无法读写数据目录：{error}"),
            Self::Sql(error) => write!(formatter, "无法读写归档索引：{error}"),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for StorageError {}
impl From<io::Error> for StorageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}
pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Serialize)]
pub struct StorageStatus {
    pub path: String,
    pub initialized: bool,
    pub development: bool,
    pub tweets: u64,
    pub media: u64,
    pub sources: u64,
    pub pending: u64,
    pub bytes: u64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct IntegrityReport {
    pub missing_files: Vec<String>,
    pub changed_files: Vec<String>,
    pub untracked_files: Vec<String>,
    pub temporary_files: usize,
}

pub struct User<'a> {
    pub id: &'a str,
    pub screen_name: &'a str,
    pub display_name: Option<&'a str>,
}

pub struct MediaSource<'a> {
    pub target: User<'a>,
    pub author: User<'a>,
    pub tweet_id: &'a str,
    pub text: Option<&'a str>,
    pub published_at: Option<&'a str>,
    pub media_key: &'a str,
    pub original_filename: &'a str,
    pub original_url: &'a str,
    pub media_type: &'a str,
    pub mime_type: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SaveResult {
    pub entity_id: i64,
    pub reused: bool,
    pub source_already_present: bool,
}

/// Selecting the location never creates directories or opens the daily archive.
pub fn default_root() -> Result<PathBuf> {
    if cfg!(debug_assertions) {
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or_else(|| invalid("无法确定开发数据目录"))?;
        Ok(project.join(".runtime").join("development"))
    } else {
        let executable = std::env::current_exe()?;
        let directory = executable
            .parent()
            .ok_or_else(|| invalid("无法确定程序目录"))?;
        Ok(directory.join("data"))
    }
}

#[derive(Serialize, Deserialize)]
struct DataLocation {
    path: PathBuf,
}

fn location_file() -> Result<PathBuf> {
    let base = default_root()?
        .parent()
        .ok_or_else(|| invalid("无法确定程序目录"))?
        .to_owned();
    Ok(base.join("location.json"))
}

pub fn configured_root() -> Result<PathBuf> {
    read_location(&location_file()?, default_root()?)
}

fn read_location(file: &Path, fallback: PathBuf) -> Result<PathBuf> {
    check_path(file)?;
    if !file.exists() {
        return Ok(fallback);
    }
    let setting: DataLocation = serde_json::from_reader(File::open(file)?)
        .map_err(|_| invalid("数据位置设置无法读取，请检查程序旁的 location.json"))?;
    let root = if setting.path.is_absolute() {
        setting.path
    } else {
        if setting
            .path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(invalid("数据位置设置包含不受支持的相对路径"));
        }
        file.parent()
            .ok_or_else(|| invalid("无法确定数据位置设置目录"))?
            .join(setting.path)
    };
    validate_location(&root)?;
    Ok(friendly_path(&root))
}

fn validate_location(root: &Path) -> Result<()> {
    if !root.is_absolute() {
        return Err(invalid("请选择完整的数据目录路径"));
    }
    check_path(root)?;
    if cfg!(debug_assertions) {
        let allowed = default_root()?.parent().unwrap().to_owned();
        check_path(&allowed)?;
        // Folder selection returns an existing directory; no unresolved parent components are accepted.
        let resolved = root.canonicalize()?;
        if !resolved.starts_with(allowed.canonicalize()?) {
            return Err(invalid("开发版只能使用本仓库 .runtime 内的隔离目录"));
        }
    }
    Ok(())
}

pub fn select_location(root: &Path) -> Result<StorageStatus> {
    validate_location(root)?;
    let result = status(root)?;
    persist_location(&location_file()?, root)?;
    Ok(result)
}

fn persist_location(file: &Path, root: &Path) -> Result<()> {
    check_path(file)?;
    let parent = file
        .parent()
        .ok_or_else(|| invalid("无法保存数据位置设置"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    // Locations alongside the EXE remain portable when the whole directory is moved.
    let root = friendly_path(root);
    let parent = friendly_path(parent);
    let saved_path = root.strip_prefix(&parent).unwrap_or(&root).to_owned();
    serde_json::to_writer(&mut temporary, &DataLocation { path: saved_path })
        .map_err(|_| invalid("无法保存数据位置设置"))?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(file)
        .map_err(|error| StorageError::Io(error.error))?
        .sync_all()?;
    Ok(())
}

pub fn status(root: &Path) -> Result<StorageStatus> {
    check_path(root)?;
    let database = root.join(DATABASE);
    check_path(&database)?;
    let mut info = StorageStatus {
        path: friendly_path(root).to_string_lossy().into_owned(),
        initialized: database.exists(),
        development: cfg!(debug_assertions),
        tweets: 0,
        media: 0,
        sources: 0,
        pending: 0,
        bytes: 0,
    };
    if info.initialized {
        let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&connection)?;
        info.tweets = count(&connection, "SELECT count(*) FROM tweets")?;
        info.bytes = count(
            &connection,
            "SELECT coalesce(sum(byte_length),0) FROM media_entities",
        )? + fs::metadata(&database)?.len();
        info.media = count(&connection, "SELECT count(*) FROM media_entities")?;
        info.sources = count(&connection, "SELECT count(*) FROM media_sources")?;
        if connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? >= 2 {
            info.pending = count(
                &connection,
                "SELECT count(*) FROM archive_retries WHERE status='pending'",
            )?;
        }
    }
    Ok(info)
}

pub struct Store {
    root: PathBuf,
    connection: Connection,
}

impl Store {
    pub fn open_readonly(root: &Path) -> Result<Self> {
        check_path(root)?;
        check_path(&root.join(DATABASE))?;
        let connection =
            Connection::open_with_flags(root.join(DATABASE), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        validate_database(&connection)?;
        Ok(Self {
            root: root.to_owned(),
            connection,
        })
    }
    /// Only an explicit initialization or task may call this write-capable method.
    pub fn open(root: &Path) -> Result<Self> {
        check_path(root)?;
        let database = root.join(DATABASE);
        check_path(&database)?;
        if database.exists() {
            let existing =
                Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let empty: bool = existing.query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%')
                 AND (SELECT user_version FROM pragma_user_version)=0
                 AND (SELECT application_id FROM pragma_application_id)=0",
                [],
                |r| r.get(0),
            )?;
            if !empty {
                validate_database(&existing)?;
            }
        }
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        for folder in ["media", "temporary"] {
            let directory = root.join(folder);
            check_path(&directory)?;
            fs::create_dir_all(directory)?;
        }
        let mut connection = Connection::open(root.join(DATABASE))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")?;
        let table_count: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if table_count == 0 {
            connection.execute_batch(include_str!("schema.sql"))?;
        }
        validate_database(&connection)?;
        let original_version: i64 =
            connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if original_version < SCHEMA_VERSION {
            // One online backup of the original index, including uncheckpointed WAL data.
            if table_count != 0 {
                let backup_file = tempfile::Builder::new()
                    .prefix(&format!("schema-v{original_version}-"))
                    .suffix(".sqlite3")
                    .tempfile_in(root.join("temporary"))?;
                let backup_path = backup_file
                    .into_temp_path()
                    .keep()
                    .map_err(|e| StorageError::Io(e.error))?;
                let mut backup = Connection::open(backup_path)?;
                rusqlite::backup::Backup::new(&connection, &mut backup)?.run_to_completion(
                    64,
                    Duration::from_millis(5),
                    None,
                )?;
            }
            // Apply all remaining steps in one transaction. A late failure also
            // rolls v1/v2 back to their original version, without replacing a live DB.
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (version, schema) in [
                (1, include_str!("archive-schema.sql")),
                (2, include_str!("runs-schema.sql")),
                (3, include_str!("workspace-schema.sql")),
                (4, include_str!("../export/schema.sql")),
            ] {
                if original_version <= version {
                    let body = schema
                        .trim()
                        .strip_prefix("BEGIN IMMEDIATE;")
                        .and_then(|s| s.trim_end().strip_suffix("COMMIT;"))
                        .ok_or_else(|| invalid("数据库迁移脚本缺少事务边界"))?;
                    transaction.execute_batch(body)?;
                }
            }
            transaction.commit()?;
        }
        Ok(Self { root, connection })
    }

    /// Stage and hash a stream without loading a whole video into memory.
    pub fn save_media(
        &mut self,
        source: &MediaSource<'_>,
        mut input: impl Read,
    ) -> Result<SaveResult> {
        validate_source(source)?;
        check_path(&self.root.join("temporary"))?;
        let mut staged = NamedTempFile::new_in(self.root.join("temporary"))?;
        let mut hash = Sha256::new();
        let mut length = 0_u64;
        let mut buffer = [0_u8; BUFFER_SIZE];
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            staged.write_all(&buffer[..read])?;
            hash.update(&buffer[..read]);
            length = length
                .checked_add(read as u64)
                .ok_or_else(|| invalid("媒体文件过大"))?;
        }
        if length == 0 || length > i64::MAX as u64 {
            return Err(invalid("媒体内容为空或长度不受支持"));
        }
        staged.as_file().sync_all()?;
        let digest = hex_digest(&hash.finalize());
        self.commit_media(source, staged, &digest, length)
    }

    fn commit_media(
        &mut self,
        source: &MediaSource<'_>,
        staged: NamedTempFile,
        digest: &str,
        length: u64,
    ) -> Result<SaveResult> {
        let stored_length = i64::try_from(length).map_err(|_| invalid("媒体文件过大"))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT entity_id, relative_path, byte_length FROM media_entities
                 WHERE target_user_id = ?1 AND sha256 = ?2 AND byte_length = ?3 ORDER BY entity_id",
            )?;
            statement
                .query_map(params![source.target.id, digest, stored_length], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut matching = None;
        let mut repair_candidate = None;
        for (entity_id, relative_path, expected_length) in candidates {
            let candidate = media_path(&self.root, &relative_path)?;
            if !candidate.is_file() || candidate.metadata()?.len() != sql_unsigned(expected_length)?
            {
                repair_candidate.get_or_insert(entity_id);
                continue;
            }
            if equal_files(staged.path(), &candidate, length)? {
                matching = Some(entity_id);
                break;
            }
        }
        let existing: Option<i64> = transaction.query_row(
            "SELECT entity_id FROM media_sources WHERE target_user_id = ?1 AND tweet_id = ?2 AND media_key = ?3",
            params![source.target.id, source.tweet_id, source.media_key], |row| row.get(0),
        ).optional()?;
        if let Some(existing_id) = existing {
            let (relative, expected, original_digest): (String, i64, String) = transaction.query_row(
                "SELECT relative_path,byte_length,sha256 FROM media_entities WHERE entity_id=?1",
                [existing_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let path = media_path(&self.root, &relative)?;
            let intact = path.is_file() && path.metadata()?.len() == sql_unsigned(expected)?;
            if !intact && original_digest == digest && expected == stored_length {
                repair_candidate = Some(existing_id);
                // Repair the shared entity itself, even if a healthy duplicate exists.
                matching = None;
            }
            if intact && matching != Some(existing_id) {
                return Err(invalid("同一媒体来源出现不同内容，已保留原归档，未覆盖"));
            }
            if intact {
                transaction.commit()?;
                return Ok(SaveResult {
                    entity_id: existing_id,
                    reused: true,
                    source_already_present: true,
                });
            }
        }

        write_tweet(
            &transaction,
            &source.target,
            &source.author,
            source.tweet_id,
            source.text,
            source.published_at,
        )?;

        let reused = matching.is_some();
        let entity_id = if let Some(entity_id) = matching {
            entity_id
        } else {
            check_path(&self.root.join("media"))?;
            let name = staged
                .path()
                .file_name()
                .ok_or_else(|| invalid("无法确定媒体文件名"))?
                .to_string_lossy();
            let relative_path = format!("media/{}-{name}.bin", &digest[..16]);
            let destination = media_path(&self.root, &relative_path)?;
            let saved = staged
                .persist_noclobber(&destination)
                .map_err(|error| StorageError::Io(error.error))?;
            saved.sync_all()?;
            // If the DB commit fails or the process stops here, keep the untracked file for inspection.
            // Never delete an existing archived file as transaction cleanup.
            if let Some(entity_id) = repair_candidate {
                // Same recorded digest and length: repair the entity's location so
                // every existing source remains attached, including other accounts.
                transaction.execute(
                    "UPDATE media_entities SET relative_path=?2 WHERE entity_id=?1",
                    params![entity_id, relative_path],
                )?;
                entity_id
            } else {
                transaction.execute(
                "INSERT INTO media_entities(target_user_id, sha256, byte_length, relative_path, mime_type)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source.target.id, digest, stored_length, relative_path, source.mime_type],
            )?;
                transaction.last_insert_rowid()
            }
        };
        transaction.execute(
            "INSERT INTO media_sources(target_user_id, tweet_id, media_key, entity_id,
             original_filename, original_url, media_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(target_user_id,tweet_id,media_key) DO UPDATE SET
             entity_id=excluded.entity_id, original_filename=excluded.original_filename,
             original_url=excluded.original_url, media_type=excluded.media_type",
            params![
                source.target.id,
                source.tweet_id,
                source.media_key,
                entity_id,
                source.original_filename,
                source.original_url,
                source.media_type
            ],
        )?;
        transaction.commit()?;
        Ok(SaveResult {
            entity_id,
            reused,
            source_already_present: false,
        })
    }

    /// Import text-only tweets through the same author and account transaction rules.
    pub fn save_tweet(
        &mut self,
        target: &User<'_>,
        author: &User<'_>,
        tweet_id: &str,
        text: Option<&str>,
        published_at: Option<&str>,
    ) -> Result<()> {
        if [
            target.id,
            target.screen_name,
            author.id,
            author.screen_name,
            tweet_id,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
        {
            return Err(invalid("推文或账号缺少必要信息"));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_tweet(&transaction, target, author, tweet_id, text, published_at)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Legacy authors without an archive target remain metadata-only users.
    pub(crate) fn save_tweet_metadata(
        &mut self,
        author: &User<'_>,
        tweet_id: &str,
        text: Option<&str>,
        published_at: Option<&str>,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_tweet_metadata(&transaction, author, tweet_id, text, published_at)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Report leftovers and missing media without deleting or repairing anything.
    pub fn inspect(&self) -> Result<IntegrityReport> {
        let mut report = IntegrityReport {
            missing_files: Vec::new(),
            changed_files: Vec::new(),
            untracked_files: Vec::new(),
            temporary_files: 0,
        };
        let mut tracked = std::collections::HashSet::new();
        let mut statement = self
            .connection
            .prepare("SELECT relative_path, byte_length, sha256 FROM media_entities")?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (relative_path, length, digest) = row?;
            let path = media_path(&self.root, &relative_path)?;
            if !path.is_file() {
                report.missing_files.push(relative_path.clone());
            } else if path.metadata()?.len() != sql_unsigned(length)? || hash_file(&path)? != digest
            {
                report.changed_files.push(relative_path.clone());
            }
            tracked.insert(relative_path);
        }
        check_path(&self.root.join("media"))?;
        for entry in fs::read_dir(self.root.join("media"))? {
            let entry = entry?;
            check_path(&entry.path())?;
            let relative = format!("media/{}", entry.file_name().to_string_lossy());
            if !tracked.contains(&relative) {
                report.untracked_files.push(relative);
            }
        }
        check_path(&self.root.join("temporary"))?;
        report.temporary_files = fs::read_dir(self.root.join("temporary"))?.count();
        Ok(report)
    }
}

fn write_tweet(
    transaction: &rusqlite::Transaction<'_>,
    target: &User<'_>,
    author: &User<'_>,
    tweet_id: &str,
    text: Option<&str>,
    published_at: Option<&str>,
) -> Result<()> {
    for user in [target, author] {
        transaction.execute(
            "INSERT INTO users(user_id, screen_name, display_name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET screen_name=excluded.screen_name,
                 display_name=coalesce(excluded.display_name, users.display_name)",
            params![user.id, user.screen_name, user.display_name],
        )?;
    }
    transaction.execute(
        "INSERT INTO archives(target_user_id) VALUES (?1) ON CONFLICT DO NOTHING",
        [target.id],
    )?;
    write_tweet_metadata(transaction, author, tweet_id, text, published_at)?;
    transaction.execute(
        "INSERT INTO archive_tweets(target_user_id, tweet_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
        params![target.id, tweet_id],
    )?;
    Ok(())
}

fn write_tweet_metadata(
    transaction: &rusqlite::Transaction<'_>,
    author: &User<'_>,
    tweet_id: &str,
    text: Option<&str>,
    published_at: Option<&str>,
) -> Result<()> {
    if [author.id, author.screen_name, tweet_id]
        .iter()
        .any(|v| v.trim().is_empty())
    {
        return Err(invalid("推文或账号缺少必要信息"));
    }
    transaction.execute(
        "INSERT INTO users(user_id,screen_name,display_name) VALUES (?1,?2,?3)
         ON CONFLICT(user_id) DO UPDATE SET screen_name=excluded.screen_name,
         display_name=coalesce(excluded.display_name,users.display_name)",
        params![author.id, author.screen_name, author.display_name],
    )?;
    // Existing tweet identity and original text are retained, rather than overwritten by a later response.
    transaction.execute(
        "INSERT INTO tweets(tweet_id, author_user_id, text, published_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tweet_id) DO NOTHING",
        params![tweet_id, author.id, text, published_at],
    )?;
    let original_author: String = transaction.query_row(
        "SELECT author_user_id FROM tweets WHERE tweet_id=?1",
        [tweet_id],
        |row| row.get(0),
    )?;
    if original_author != author.id {
        return Err(invalid("推文作者与原归档不一致，未保存本次来源"));
    }
    Ok(())
}

fn invalid(message: &str) -> StorageError {
    StorageError::Invalid(message.to_owned())
}

fn count(connection: &Connection, sql: &str) -> Result<u64> {
    sql_unsigned(connection.query_row(sql, [], |row| row.get::<_, i64>(0))?)
}

fn sql_unsigned(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| invalid("归档索引包含无效的数量或文件长度"))
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 15) as usize] as char);
    }
    result
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; BUFFER_SIZE];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    Ok(hex_digest(&hash.finalize()))
}

fn validate_database(connection: &Connection) -> Result<()> {
    let application_id: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id != APPLICATION_ID || !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(invalid(
            "现有数据库不是受支持的新版归档索引，请通过副本导入或升级处理",
        ));
    }
    connection.prepare(
        "SELECT t.tweet_id, e.relative_path, s.original_url FROM tweets t,
                        media_entities e, media_sources s LIMIT 0",
    )?;
    Ok(())
}

fn validate_source(source: &MediaSource<'_>) -> Result<()> {
    for field in [
        source.target.id,
        source.target.screen_name,
        source.author.id,
        source.author.screen_name,
        source.tweet_id,
        source.media_key,
        source.mime_type,
    ] {
        if field.trim().is_empty() {
            return Err(invalid("媒体来源缺少必要信息"));
        }
    }
    if !matches!(source.media_type, "image" | "video" | "gif") {
        return Err(invalid("媒体类型不受支持"));
    }
    Ok(())
}

// Reject symbolic links and Windows junctions before touching the data tree.
// This is a local-data boundary check, not a defense against a malicious concurrent OS user.
pub(crate) fn check_path(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                let mut redirected = metadata.file_type().is_symlink();
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if metadata.file_attributes() & 0x400 != 0 {
                        redirected |= redirecting_tag(reparse_tag(ancestor)?);
                    }
                }
                if redirected {
                    return Err(invalid("数据目录包含链接或重解析路径，请选择实际目录"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn redirecting_tag(tag: u32) -> bool {
    // The name-surrogate bit denotes path redirection (symlinks, junctions, etc.).
    // App execution links also redirect. CLOUD variants and DEDUP have neither.
    tag & 0x2000_0000 != 0 || tag == 0x8000_001b
}

#[cfg(windows)]
fn reparse_tag(path: &Path) -> Result<u32> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FileAttributeTagInfo, GetFileInformationByHandleEx,
    };
    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    // The handle is owned by File; info is writable for exactly the given size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(info.ReparseTag)
}

/// Normal Windows paths for display, persistence, dialogs and Explorer.
pub(crate) fn friendly_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        if wide.starts_with(&prefix) {
            let tail = &wide[4..];
            let unc: Vec<u16> = "UNC\\".encode_utf16().collect();
            let normalized = if tail.starts_with(&unc) {
                "\\\\"
                    .encode_utf16()
                    .chain(tail[4..].iter().copied())
                    .collect::<Vec<_>>()
            } else if tail.len() >= 3 && tail[1] == b':' as u16 && tail[2] == b'\\' as u16 {
                tail.to_vec()
            } else {
                return path.to_owned();
            };
            return PathBuf::from(std::ffi::OsString::from_wide(&normalized));
        }
    }
    path.to_owned()
}

pub(crate) fn media_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    let components: Vec<_> = path.components().collect();
    if relative.contains('\\')
        || relative.contains(':')
        || relative.contains("..")
        || components.len() != 2
        || components[0] != Component::Normal("media".as_ref())
        || !matches!(components[1], Component::Normal(_))
    {
        return Err(invalid("媒体索引包含越界路径，未读写该文件"));
    }
    let joined = root.join(path);
    check_path(&joined)?;
    Ok(joined)
}

fn equal_files(left: &Path, right: &Path, length: u64) -> Result<bool> {
    let mut left = File::open(left)?;
    let mut right = File::open(right)?;
    let mut remaining = length;
    let mut first = [0_u8; BUFFER_SIZE];
    let mut second = [0_u8; BUFFER_SIZE];
    while remaining > 0 {
        let read = remaining.min(BUFFER_SIZE as u64) as usize;
        left.read_exact(&mut first[..read])?;
        right.read_exact(&mut second[..read])?;
        if first[..read] != second[..read] {
            return Ok(false);
        }
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    Ok(left.read(&mut extra)? == 0 && right.read(&mut extra)? == 0)
}

#[cfg(test)]
mod tests;

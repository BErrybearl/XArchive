mod browser;
mod cdp;
mod protect;

use crate::{platform, storage};
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;
use url::Url;
use zeroize::{Zeroize, Zeroizing};

type Result<T> = std::result::Result<T, AuthError>;
const AUTH_FILE: &str = "login.dpapi";
const LOGIN_PROFILE: &str = "private/login-browser";
const MAX_FILE: u64 = 64 * 1024;
const CLEANUP_WARNING: &str =
    "登录浏览器的临时资料尚未清理，请关闭该浏览器；下次启动或登录前会重试";

#[derive(Debug)]
pub enum AuthError {
    Data,
    Io,
    Protection,
    InvalidState,
    Busy,
    BrowserStart,
    BrowserConnection,
    BrowserInUse,
    BrowserClear,
    CredentialsFormat,
    NotReady,
    Authentication,
    Interface,
    Network(String),
    RateLimit,
    Proxy,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Network(detail) = self {
            return write!(f, "无法完成 X 登录检查：{detail}；此次状态尚未保存");
        }
        f.write_str(match self {
            Self::Data => "请先创建有效的新版数据目录，再登录",
            Self::Io => "无法读写本机登录设置，原有登录状态保持不变",
            Self::Protection => "无法加密或解密登录状态，请在原 Windows 用户下重试或重新登录",
            Self::InvalidState => "本机登录状态不完整或格式不兼容，请重新登录",
            Self::Busy => "已有登录操作，请先完成或取消；登录期间不能切换数据目录",
            Self::BrowserStart => "无法启动登录浏览器，请确认已安装 Edge 或 Chrome",
            Self::BrowserConnection => "无法读取登录浏览器的专用资料，此次状态尚未保存；请关闭该浏览器后重试“已完成登录”",
            Self::BrowserInUse => "请先关闭登录用的浏览器窗口",
            Self::BrowserClear => "无法清除登录浏览器资料，请关闭该浏览器后重试；应用已保存的登录状态保持不变",
            Self::CredentialsFormat => "auth_token 和 ct0 均不能为空，只能包含字母、数字、连字符、下划线或百分号，长度不能超过 4096",
            Self::NotReady => {
                "尚未完成登录，专用资料中没有完整的 X 登录凭据；请完成浏览器登录并关闭窗口后重试"
            }
            Self::Authentication => "X 未接受此次登录状态，未保存此次状态；请重试或取消后重新登录",
            Self::Interface => {
                "X 的登录检查接口未返回可确认的账号，未保存此次状态；请稍后重试并反馈此提示"
            }
            Self::Network(_) => unreachable!(),
            Self::RateLimit => "X 暂时限制请求，请稍后再点击“已完成登录”；此次状态尚未保存",
            Self::Proxy => {
                "代理应为 http、https 或 socks5 地址及有效端口，不能包含账号、密码、路径或查询参数"
            }
        })
    }
}
impl std::error::Error for AuthError {}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Account {
    pub user_id: String,
    pub screen_name: String,
    pub display_name: String,
}

#[derive(Serialize)]
pub struct LoginStatus {
    pub saved: bool,
    pub pending: bool,
    pub account: Option<Account>,
    pub verified_at: Option<u64>,
    pub proxy: String,
    pub cleanup_warning: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    auth_token: String,
    ct0: String,
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.auth_token.zeroize();
        self.ct0.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct SavedLogin {
    version: u32,
    credentials: Credentials,
    account: Account,
    verified_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    proxy: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn check(path: &Path) -> Result<()> {
    storage::check_path(path).map_err(|_| AuthError::Data)
}

fn ready(root: &Path) -> Result<()> {
    match storage::status(root) {
        Ok(status) if status.initialized => Ok(()),
        _ => Err(AuthError::Data),
    }
}

fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    check(path)?;
    let mut result = Vec::new();
    fs::File::open(path)
        .map_err(|_| AuthError::Io)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut result)
        .map_err(|_| AuthError::Io)?;
    if result.len() as u64 > MAX_FILE {
        return Err(AuthError::InvalidState);
    }
    Ok(result)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    check(path)?;
    let parent = path.parent().ok_or(AuthError::Io)?;
    fs::create_dir_all(parent).map_err(|_| AuthError::Io)?;
    let mut file = NamedTempFile::new_in(parent).map_err(|_| AuthError::Io)?;
    file.write_all(bytes).map_err(|_| AuthError::Io)?;
    file.as_file().sync_all().map_err(|_| AuthError::Io)?;
    file.persist(path).map_err(|_| AuthError::Io)?;
    // Rename is the commit point. No additional fallible operation reports a false failure.
    Ok(())
}

fn load(root: &Path) -> Result<Option<SavedLogin>> {
    let path = root.join("private").join(AUTH_FILE);
    check(&path)?;
    if !path.exists() {
        return Ok(None);
    }
    let encrypted = bounded_read(&path)?;
    let plain = protect::decrypt(&encrypted)?;
    let saved: SavedLogin = serde_json::from_slice(&plain).map_err(|_| AuthError::InvalidState)?;
    if saved.version != 1
        || !valid_token(&saved.credentials.auth_token)
        || !valid_token(&saved.credentials.ct0)
    {
        return Err(AuthError::InvalidState);
    }
    validate_account(&saved.account)?;
    Ok(Some(saved))
}

// Backend-only view: neither Debug nor Serialize exposes session credentials.
pub(crate) struct PlatformLogin {
    pub auth_token: Zeroizing<String>,
    pub ct0: Zeroizing<String>,
    pub account: Account,
    pub proxy: String,
}

pub(crate) fn platform_login(root: &Path) -> Result<PlatformLogin> {
    let saved = load(root)?.ok_or(AuthError::Authentication)?;
    Ok(PlatformLogin {
        auth_token: Zeroizing::new(saved.credentials.auth_token.clone()),
        ct0: Zeroizing::new(saved.credentials.ct0.clone()),
        account: saved.account.clone(),
        proxy: platform_proxy(root)?,
    })
}

pub(crate) fn platform_proxy(root: &Path) -> Result<String> {
    Ok(settings(root)?.proxy)
}

fn save(root: &Path, saved: &SavedLogin) -> Result<()> {
    ready(root)?;
    let plain = Zeroizing::new(serde_json::to_vec(saved).map_err(|_| AuthError::InvalidState)?);
    let encrypted = protect::encrypt(&plain)?;
    atomic_write(&root.join("private").join(AUTH_FILE), &encrypted)
}

fn settings(root: &Path) -> Result<Settings> {
    let path = root.join("settings.json");
    check(&path)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let settings: Settings =
        serde_json::from_slice(&bounded_read(&path)?).map_err(|_| AuthError::InvalidState)?;
    validate_proxy(&settings.proxy)?;
    Ok(settings)
}

fn validate_proxy(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }
    let url = Url::parse(raw).map_err(|_| AuthError::Proxy)?;
    if !matches!(url.scheme(), "http" | "https" | "socks5")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.port_or_known_default() == Some(0)
        || url.port_or_known_default().is_none()
    {
        return Err(AuthError::Proxy);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_%".contains(&byte))
}

fn validate_account(account: &Account) -> Result<()> {
    if account.user_id.is_empty()
        || account.user_id.len() > 32
        || !account.user_id.bytes().all(|byte| byte.is_ascii_digit())
        || account.screen_name.is_empty()
        || account.screen_name.len() > 15
        || !account
            .screen_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || account.display_name.len() > 1024
        || account.display_name.chars().any(char::is_control)
    {
        return Err(AuthError::Interface);
    }
    Ok(())
}

// Parse only X cookies; unrelated cookies and upstream error bodies never leave the backend.
fn capture_cookies(value: &mut serde_json::Value) -> Result<Credentials> {
    let mut result = Credentials {
        auth_token: String::new(),
        ct0: String::new(),
    };
    let cookies = value
        .get_mut("cookies")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(AuthError::BrowserConnection)?;
    let mut invalid = false;
    for cookie in cookies {
        let accepted = matches!(cookie["domain"].as_str(), Some("x.com" | ".x.com"))
            && cookie["path"] == "/"
            && cookie["secure"] == true
            && cookie["expires"]
                .as_f64()
                .is_none_or(|expiry| expiry <= 0.0 || expiry > now() as f64);
        let field = match cookie["name"].as_str() {
            Some("auth_token") if accepted => Some(&mut result.auth_token),
            Some("ct0") if accepted => Some(&mut result.ct0),
            _ => None,
        };
        if let Some(raw) = cookie["value"].as_str() {
            if let Some(field) = field {
                if !valid_token(raw) || (!field.is_empty() && field != raw) {
                    invalid = true;
                } else {
                    *field = raw.to_owned();
                }
            }
        } else if field.is_some() {
            invalid = true;
        }
        if let Some(serde_json::Value::String(raw)) = cookie.get_mut("value") {
            raw.zeroize();
        }
    }
    if invalid || !valid_token(&result.auth_token) || !valid_token(&result.ct0) {
        return Err(AuthError::NotReady);
    }
    Ok(result)
}

fn manual_credentials(
    auth_token: Zeroizing<String>,
    ct0: Zeroizing<String>,
) -> Result<Credentials> {
    if !valid_token(&auth_token) || !valid_token(&ct0) {
        return Err(AuthError::CredentialsFormat);
    }
    Ok(Credentials {
        auth_token: auth_token.to_string(),
        ct0: ct0.to_string(),
    })
}

fn platform_error(error: platform::PlatformError) -> AuthError {
    match error.category {
        platform::Category::Authentication => AuthError::Authentication,
        platform::Category::ProxyNetwork => AuthError::Network(match error.network_route {
            Some(route) => format!("{}；{route}", error.message),
            None => error.message.to_owned(),
        }),
        platform::Category::RateLimit => AuthError::RateLimit,
        platform::Category::LocalState => AuthError::InvalidState,
        _ => AuthError::Interface,
    }
}

// The reader has exited before platform verification; no live browser can change the candidate.
fn verify_candidate(
    credentials: Credentials,
    verify: impl FnOnce(&Credentials) -> Result<Account>,
) -> Result<SavedLogin> {
    let account = verify(&credentials)?;
    validate_account(&account)?;
    Ok(SavedLogin {
        version: 1,
        credentials,
        account,
        verified_at: now(),
    })
}

fn verify_credentials(
    root: &Path,
    proxy: &str,
    session: &mut platform::Session,
    credentials: Credentials,
) -> Result<SavedLogin> {
    verify_candidate(credentials, |credentials| {
        let login = PlatformLogin {
            auth_token: Zeroizing::new(credentials.auth_token.clone()),
            ct0: Zeroizing::new(credentials.ct0.clone()),
            account: Account {
                user_id: String::new(),
                screen_name: String::new(),
                display_name: String::new(),
            },
            proxy: proxy.to_owned(),
        };
        platform::verify_login(root, session, &login).map_err(platform_error)
    })
}

struct PendingLogin {
    root: PathBuf,
    profile: PathBuf,
    browser: Option<Box<dyn browser::LoginBrowser>>,
    proxy: String,
}
impl PendingLogin {
    fn prepare(root: &Path, proxy: &str) -> Result<Self> {
        ready(root)?;
        let profile = root.join(LOGIN_PROFILE);
        safe_profile(root, &profile)?;
        fs::create_dir_all(&profile).map_err(|_| AuthError::Io)?;
        safe_profile(root, &profile)?;
        Ok(Self {
            root: root.to_owned(),
            profile,
            browser: None,
            proxy: proxy.to_owned(),
        })
    }
    fn open(&mut self) -> Result<()> {
        safe_profile(&self.root, &self.profile)?;
        if browser::profile_in_use(&self.profile)? {
            return Err(AuthError::BrowserInUse);
        }
        self.browser = Some(Box::new(browser::Browser::open(
            &self.profile,
            &self.proxy,
        )?));
        Ok(())
    }
    fn require_idle(&mut self) -> Result<()> {
        safe_profile(&self.root, &self.profile)?;
        if self
            .browser
            .as_mut()
            .ok_or(AuthError::BrowserConnection)?
            .in_use(&self.profile)?
        {
            return Err(AuthError::BrowserInUse);
        }
        Ok(())
    }
    fn verify(&mut self, session: &mut platform::Session) -> Result<SavedLogin> {
        let credentials = self
            .browser
            .as_mut()
            .ok_or(AuthError::BrowserConnection)?
            .read(&self.profile)?;
        verify_credentials(&self.root, &self.proxy, session, credentials)
    }
    fn commit(&mut self, saved: &SavedLogin) -> Result<()> {
        self.require_idle()?;
        save(&self.root, saved)
    }
}

fn cleanup_profile(
    root: &Path,
    profile: &Path,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<()> {
    safe_legacy_profile(root, profile)?;
    remove(profile).map_err(|_| AuthError::Io)
}

// The retained directory itself is the retry record. Only immediate owned profiles are scanned.
fn retry_cleanup(
    root: &Path,
    active: Option<&Path>,
    mut remove: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<()> {
    let parent = root.join("temporary");
    check(&parent)?;
    if !parent.exists() {
        return Ok(());
    }
    let mut failed = false;
    for entry in fs::read_dir(parent).map_err(|_| AuthError::Io)? {
        let path = entry.map_err(|_| AuthError::Io)?.path();
        if active == Some(path.as_path())
            || !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("xarchive-login-"))
        {
            continue;
        }
        if cleanup_profile(root, &path, &mut remove).is_err() {
            failed = true;
        }
    }
    if failed { Err(AuthError::Io) } else { Ok(()) }
}

fn safe_legacy_profile(root: &Path, profile: &Path) -> Result<()> {
    if profile.parent() != Some(root.join("temporary").as_path())
        || !profile
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("xarchive-login-"))
    {
        return Err(AuthError::Data);
    }
    safe_profile_tree(profile)
}

fn safe_profile(root: &Path, profile: &Path) -> Result<()> {
    if profile != root.join(LOGIN_PROFILE) {
        return Err(AuthError::Data);
    }
    safe_profile_tree(profile)
}

fn safe_profile_tree(profile: &Path) -> Result<()> {
    check(profile)?;
    if !profile.exists() {
        return Ok(());
    }
    // Refuse recursive removal if anything within the owned profile is a reparse path.
    let mut directories = vec![profile.to_owned()];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).map_err(|_| AuthError::Io)? {
            let entry = entry.map_err(|_| AuthError::Io)?;
            check(&entry.path())?;
            if entry.file_type().map_err(|_| AuthError::Io)?.is_dir() {
                directories.push(entry.path());
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct LoginManager {
    pending: Option<PendingLogin>,
    cleanup_warning: Option<String>,
}

impl LoginManager {
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn status(&mut self, root: &Path) -> Result<LoginStatus> {
        let login = load(root)?;
        let settings = settings(root)?;
        Ok(LoginStatus {
            saved: login.is_some(),
            pending: self.is_pending(),
            account: login.as_ref().map(|value| value.account.clone()),
            verified_at: login.as_ref().map(|value| value.verified_at),
            proxy: settings.proxy,
            cleanup_warning: self.cleanup_warning.clone(),
        })
    }

    pub fn retry_cleanup(&mut self, root: &Path) {
        self.cleanup_warning = retry_cleanup(
            root,
            self.pending.as_ref().map(|p| p.profile.as_path()),
            |path| match browser::profile_in_use(path) {
                Ok(false) => fs::remove_dir_all(path),
                _ => Err(std::io::ErrorKind::PermissionDenied.into()),
            },
        )
        .err()
        .map(|_| CLEANUP_WARNING.to_owned());
        if self.cleanup_warning.is_some() {
            eprintln!("{CLEANUP_WARNING}");
        }
    }

    fn release(&mut self) {
        // Drop only our handle. Keep cache and X device trust, including on cancel/exit.
        self.pending = None;
    }

    pub fn clear_browser_profile(&mut self, root: &Path) -> Result<()> {
        self.clear_browser_profile_with(root, browser::profile_in_use)
    }

    fn clear_browser_profile_with(
        &mut self,
        root: &Path,
        probe: impl FnOnce(&Path) -> Result<bool>,
    ) -> Result<()> {
        ready(root)?;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.root != root)
        {
            return Err(AuthError::Busy);
        }
        let profile = root.join(LOGIN_PROFILE);
        safe_profile(root, &profile)?;
        if profile.exists() {
            let busy = match self.pending.as_mut() {
                Some(pending) => pending
                    .browser
                    .as_mut()
                    .ok_or(AuthError::BrowserConnection)?
                    .in_use(&profile)?,
                None => probe(&profile)?,
            };
            if busy {
                return Err(AuthError::BrowserInUse);
            }
            // Validate the complete tree again before deleting exactly this directory.
            safe_profile(root, &profile)?;
            fs::remove_dir_all(&profile).map_err(|_| AuthError::BrowserClear)?;
        }
        self.release();
        Ok(())
    }

    pub fn begin(&mut self, root: &Path, proxy: &str) -> Result<LoginStatus> {
        if self.is_pending() {
            return Err(AuthError::Busy);
        }
        let proxy = validate_proxy(proxy)?;
        self.retry_cleanup(root);
        let bytes = serde_json::to_vec(&Settings {
            proxy: proxy.clone(),
        })
        .map_err(|_| AuthError::Io)?;
        let mut pending = PendingLogin::prepare(root, &proxy)?;
        pending.open()?;
        atomic_write(&root.join("settings.json"), &bytes)?;
        self.pending = Some(pending);
        // Unreadable previous state must not prevent re-login; do not decrypt it here.
        Ok(LoginStatus {
            saved: false,
            pending: true,
            account: None,
            verified_at: None,
            proxy,
            cleanup_warning: self.cleanup_warning.clone(),
        })
    }

    pub fn confirm(&mut self, root: &Path, session: &mut platform::Session) -> Result<LoginStatus> {
        self.confirm_with(root, |pending| pending.verify(session))
    }

    fn confirm_with(
        &mut self,
        root: &Path,
        verify: impl FnOnce(&mut PendingLogin) -> Result<SavedLogin>,
    ) -> Result<LoginStatus> {
        let pending = self.pending.as_mut().ok_or(AuthError::NotReady)?;
        if pending.root != root {
            return Err(AuthError::Busy);
        }
        pending.require_idle()?;
        let saved = verify(pending)?;
        pending.commit(&saved)?;
        self.release();
        self.status(root)
    }

    pub fn manual(
        &mut self,
        root: &Path,
        proxy: &str,
        auth_token: Zeroizing<String>,
        ct0: Zeroizing<String>,
        session: &mut platform::Session,
    ) -> Result<LoginStatus> {
        let proxy = validate_proxy(proxy)?;
        self.manual_with(root, &proxy, auth_token, ct0, |credentials| {
            verify_credentials(root, &proxy, session, credentials)
        })
    }
    fn manual_with(
        &mut self,
        root: &Path,
        proxy: &str,
        auth_token: Zeroizing<String>,
        ct0: Zeroizing<String>,
        verify: impl FnOnce(Credentials) -> Result<SavedLogin>,
    ) -> Result<LoginStatus> {
        if self.is_pending() {
            return Err(AuthError::Busy);
        }
        ready(root)?;
        let proxy = validate_proxy(proxy)?;
        let saved = verify(manual_credentials(auth_token, ct0)?)?;
        atomic_write(
            &root.join("settings.json"),
            &serde_json::to_vec(&Settings { proxy }).map_err(|_| AuthError::Io)?,
        )?;
        save(root, &saved)?;
        self.status(root)
    }

    pub fn cancel(&mut self, root: &Path) -> Result<LoginStatus> {
        self.release();
        self.status(root)
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn save_proxy(root: &Path, proxy: &str) -> Result<()> {
    let proxy = validate_proxy(proxy)?;
    ready(root)?;
    atomic_write(
        &root.join("settings.json"),
        &serde_json::to_vec(&Settings { proxy }).map_err(|_| AuthError::Io)?,
    )
}

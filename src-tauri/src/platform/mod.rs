pub(crate) mod client;
mod diagnostics;
pub use diagnostics::{DiagnosticReport, diagnose};
pub(crate) mod error;
pub(crate) mod params;
pub(crate) mod parser;
pub(crate) mod proxy;

use crate::{auth, storage};
pub use client::Session;
use client::{Http, RateLimit, Requests};
pub use error::{Category, PlatformError};
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};

// Candidate credentials are checked independently of the previous DPAPI state.
pub(crate) fn verify_login(
    root: &Path,
    session: &mut Session,
    login: &auth::PlatformLogin,
) -> error::Result<auth::Account> {
    session.for_account("login-candidate");
    verify_login_with(
        root,
        session,
        login,
        Http::new(&login.proxy)?,
        Duration::from_secs(1),
    )
}

pub(crate) fn verify_login_with<T: client::Transport>(
    root: &Path,
    session: &mut Session,
    login: &auth::PlatformLogin,
    transport: T,
    delay: Duration,
) -> error::Result<auth::Account> {
    let (mut params, _) = params::load(root);
    let mut requests = Requests {
        transport,
        session,
        delay,
    };
    if params.public_bearer.is_empty() {
        let found = requests.discover(Some(login), true)?;
        params.query_ids.extend(found.ids);
        params.public_bearer = found.bearer.ok_or_else(PlatformError::interface)?;
    }
    parser::viewer(&requests.query("Viewer", json!({}), &params, login)?)
}

#[derive(Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub stage: String,
    pub current_account: Option<auth::Account>,
    pub target_account: Option<auth::Account>,
    pub stats: Option<parser::MediaStats>,
    pub rate_limits: BTreeMap<String, RateLimit>,
    pub notices: Vec<String>,
    pub error: Option<PlatformError>,
}

impl ProbeResult {
    pub fn failure(error: PlatformError) -> Self {
        Self {
            ok: false,
            stage: "本机状态".into(),
            current_account: None,
            target_account: None,
            stats: None,
            rate_limits: BTreeMap::new(),
            notices: Vec::new(),
            error: Some(error),
        }
    }
}

fn uncertainty() -> String {
    "Viewer、目标解析和 UserMedia 首页已通过用户真实验证，无需 x-client-transaction-id；UserTweets 及后续分页仍需验证，参数有效性以本次请求为准".into()
}

pub fn probe(root: &Path, session: &mut Session, raw_target: &str) -> ProbeResult {
    let mut report = ProbeResult::failure(PlatformError::local());
    report.error = None;
    let result = (|| -> error::Result<()> {
        report.stage = "目标输入".into();
        let name = parser::target_name(raw_target)?;
        report.stage = "本机状态".into();
        storage::check_path(root).map_err(|_| PlatformError::local())?;
        let login = auth::platform_login(root).map_err(|error| match error {
            auth::AuthError::Authentication => PlatformError::new(Category::Authentication),
            _ => PlatformError::local(),
        })?;
        let (mut params, notices) = params::load(root);
        report.notices = notices;
        report.notices.push(uncertainty());
        session.for_account(&login.account.user_id);
        let mut requests = Requests {
            transport: Http::new(&login.proxy)?,
            session,
            delay: Duration::from_secs(1),
        };
        probe_requests(&mut requests, &mut params, &login, &name, &mut report)?;
        Ok(())
    })();
    report.rate_limits = session.limits.clone();
    match result {
        Ok(()) => {
            report.ok = true;
            report.stage = "探测完成".into();
        }
        Err(error) => report.error = Some(error),
    }
    report
}

fn probe_requests<T: client::Transport>(
    requests: &mut Requests<'_, T>,
    params: &mut params::Params,
    login: &auth::PlatformLogin,
    name: &str,
    report: &mut ProbeResult,
) -> error::Result<()> {
    if params.public_bearer.is_empty() {
        report.stage = "公开接口参数".into();
        report.notices.push(
            "内置公开 Bearer 留空，沿用旧版运行时发现方式；本次只在内存中发现，不写 platform.json"
                .into(),
        );
        let found = requests.discover(Some(login), true)?;
        params.query_ids.extend(found.ids);
        params.public_bearer = found.bearer.ok_or_else(PlatformError::interface)?;
    }
    report.stage = "当前登录账号".into();
    let current = parser::viewer(&requests.query("Viewer", json!({}), params, login)?)?;
    if current.user_id != login.account.user_id {
        return Err(PlatformError::new(Category::Authentication));
    }
    report.current_account = Some(current);
    report.stage = "目标账号".into();
    let target = parser::target(&requests.query(
        "UserByScreenName",
        json!({"screen_name": name, "withSafetyModeUserFields": true}),
        params,
        login,
    )?)?;
    report.target_account = Some(target.clone());
    report.stage = "媒体页首页".into();
    let page = requests.query("UserMedia", json!({"userId": target.user_id, "count": 20, "includePromotedContent": false,
            "withClientEventToken": false, "withBirdwatchNotes": false, "withVoice": true, "withV2Timeline": true}), params, login)?;
    report.stats = Some(parser::media_page(&page)?);
    Ok(())
}

#[derive(Serialize)]
pub struct RefreshResult {
    pub ok: bool,
    pub updated_operations: Vec<String>,
    pub notices: Vec<String>,
    pub error: Option<PlatformError>,
}

impl RefreshResult {
    pub fn failure(error: PlatformError) -> Self {
        Self {
            ok: false,
            updated_operations: Vec::new(),
            notices: vec!["刷新未保存，原 platform.json 保持不变".into()],
            error: Some(error),
        }
    }
}

pub fn refresh(root: &Path, session: &mut Session) -> RefreshResult {
    let (original, mut notices) = params::load(root);
    let result = (|| -> error::Result<()> {
        storage::check_path(root).map_err(|_| PlatformError::local())?;
        let proxy = auth::platform_proxy(root).map_err(|_| PlatformError::local())?;
        let login = auth::platform_login(root).ok();
        let mut requests = Requests {
            transport: Http::new(&proxy)?,
            session,
            delay: Duration::from_secs(1),
        };
        let discovered = requests.discover(login.as_ref(), original.public_bearer.is_empty());
        params::commit_refresh(root, &original, discovered)?;
        Ok(())
    })();
    notices.push(uncertainty());
    match result {
        Ok(()) => {
            notices.push("已原子保存四个查询 ID 和公开 Bearer；保留现有 feature/fieldToggles，刷新不验证其在线有效性".into());
            RefreshResult {
                ok: true,
                updated_operations: params::OPERATIONS.iter().map(|s| s.to_string()).collect(),
                notices,
                error: None,
            }
        }
        Err(error) => {
            notices.push("刷新未保存，原 platform.json 保持不变".into());
            RefreshResult {
                ok: false,
                updated_operations: Vec::new(),
                notices,
                error: Some(error),
            }
        }
    }
}

#[cfg(test)]
mod tests;

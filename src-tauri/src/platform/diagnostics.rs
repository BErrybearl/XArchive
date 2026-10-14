use super::{
    auth,
    client::{Http, RateLimit, Requests, Session, Transport},
    error::{Category, PlatformError, Result},
    params::{self, Params},
    parser,
    proxy::ProxyPlan,
    storage,
};
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::json;
use std::{collections::BTreeMap, path::Path, time::Duration};
use url::Url;

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticItem {
    pub label: String,
    pub status: String,
    pub detail: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticReport {
    pub items: Vec<DiagnosticItem>,
    pub rate_limits: BTreeMap<String, RateLimit>,
    pub error: Option<PlatformError>,
}
impl Default for DiagnosticReport {
    fn default() -> Self {
        Self {
            items: [
                "代理路由",
                "x.com 可达性",
                "登录状态（Viewer）",
                "接口参数有效性",
                "当前限流余量",
            ]
            .into_iter()
            .map(|label| DiagnosticItem {
                label: label.into(),
                status: "未检查".into(),
                detail: String::new(),
            })
            .collect(),
            rate_limits: BTreeMap::new(),
            error: None,
        }
    }
}
impl DiagnosticReport {
    pub fn failure(error: PlatformError) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }
    fn pass(&mut self, i: usize, detail: impl Into<String>) {
        self.items[i].status = "通过".into();
        self.items[i].detail = detail.into();
    }
    fn fail(&mut self, i: usize, error: PlatformError) {
        self.items[i].status = "未通过".into();
        self.items[i].detail = error.message.into();
        self.error = Some(error);
    }
}

pub fn diagnose(
    root: &Path,
    session: &mut Session,
    target: &str,
    include_timeline: bool,
) -> DiagnosticReport {
    let mut report = DiagnosticReport::default();
    let result = (|| -> Result<()> {
        storage::check_path(root).map_err(|_| PlatformError::local())?;
        let proxy = auth::platform_proxy(root).map_err(|_| PlatformError::local())?;
        let routing = ProxyPlan::resolve(&proxy)?;
        report.pass(
            0,
            format!(
                "{}；连接将在下一项验证",
                routing.route(&Url::parse("https://x.com/").expect("static URL"))
            ),
        );
        let login = auth::platform_login(root).map_err(|e| match e {
            auth::AuthError::Authentication => PlatformError::new(Category::Authentication),
            _ => PlatformError::local(),
        });
        if let Ok(login) = &login {
            session.for_account(&login.account.user_id);
        }
        let mut requests = Requests {
            transport: Http::new(&proxy)?,
            session,
            delay: Duration::from_secs(1),
        };
        diagnose_requests(
            &mut requests,
            &mut params::load(root).0,
            login,
            target,
            include_timeline,
            &mut report,
        );
        Ok(())
    })();
    if let Err(e) = result {
        report.fail(0, e);
    }
    report.rate_limits = session.limits.clone();
    report
}

fn diagnose_requests<T: Transport>(
    requests: &mut Requests<'_, T>,
    params: &mut Params,
    login: Result<auth::PlatformLogin>,
    target: &str,
    include_timeline: bool,
    report: &mut DiagnosticReport,
) {
    // No Cookie is attached to the reachability request.
    match requests.get(
        "Reachability",
        Url::parse("https://x.com/").expect("static URL"),
        HeaderMap::new(),
    ) {
        Ok(r) => report.pass(1, format!("收到 x.com 响应：HTTP {}", r.status)),
        Err(e) if e.http_status.is_some_and(|s| s != 407 && s != 429) => report.pass(
            1,
            format!(
                "收到 x.com 响应：HTTP {}；后续登录检查另行判断",
                e.http_status.unwrap()
            ),
        ),
        Err(e) => {
            report.fail(1, e);
            report.rate_limits = requests.session.limits.clone();
            return;
        }
    }
    let login = match login {
        Ok(login) => login,
        Err(e) => {
            report.fail(2, e);
            return;
        }
    };
    let result = (|| -> Result<()> {
        if params.public_bearer.is_empty() {
            let found = requests.discover(Some(&login), true)?;
            params.query_ids.extend(found.ids);
            params.public_bearer = found.bearer.ok_or_else(PlatformError::interface)?;
        }
        let current = parser::viewer(&requests.query("Viewer", json!({}), params, &login)?)?;
        if current.user_id != login.account.user_id {
            return Err(PlatformError::new(Category::Authentication));
        }
        report.pass(2, format!("登录状态有效：@{}", current.screen_name));
        Ok(())
    })();
    if let Err(e) = result {
        report.fail(2, e);
        report.rate_limits = requests.session.limits.clone();
        return;
    }
    let result = (|| -> Result<()> {
        if let Some(rate) =
            requests.session.limits.values().find(|r| {
                r.remaining == Some(0) && r.reset_at.is_none_or(|r| r > super::client::now())
            })
        {
            return Err(PlatformError::rate(rate.reset_at));
        }
        let name = if target.trim().is_empty() {
            login.account.screen_name.clone()
        } else {
            parser::target_name(target)?
        };
        let target = parser::target(&requests.query(
            "UserByScreenName",
            json!({"screen_name":name,"withSafetyModeUserFields":true}),
            params,
            &login,
        )?)?;
        if let Some(rate) =
            requests.session.limits.values().find(|r| {
                r.remaining == Some(0) && r.reset_at.is_none_or(|r| r > super::client::now())
            })
        {
            return Err(PlatformError::rate(rate.reset_at));
        }
        let media=requests.query("UserMedia",json!({"userId":target.user_id,"count":20,"includePromotedContent":false,"withClientEventToken":false,"withBirdwatchNotes":false,"withVoice":true,"withV2Timeline":true}),params,&login)?;
        parser::media_page(&media)?;
        if include_timeline {
            if let Some(rate) = requests.session.limits.values().find(|r| {
                r.remaining == Some(0) && r.reset_at.is_none_or(|r| r > super::client::now())
            }) {
                return Err(PlatformError::rate(rate.reset_at));
            }
            parser::media_page(&requests.query("UserTweets",json!({"userId":target.user_id,"count":20,"includePromotedContent":false,"withQuickPromoteEligibilityTweetFields":true,"withVoice":true,"withV2Timeline":true}),params,&login)?)?;
        }
        report.pass(
            3,
            if include_timeline {
                "Viewer、目标解析、UserMedia 与 UserTweets 首页有效"
            } else {
                "Viewer、目标解析与 UserMedia 首页有效；UserTweets 未检查"
            },
        );
        Ok(())
    })();
    if let Err(e) = result {
        report.fail(3, e);
    }
    report.rate_limits = requests.session.limits.clone();
    if let Some(rate) = report
        .rate_limits
        .values()
        .find(|r| r.remaining == Some(0) && r.reset_at.is_none_or(|r| r > super::client::now()))
    {
        let e = PlatformError::rate(rate.reset_at);
        report.items[4].status = "限流".into();
        report.items[4].detail = "额度已耗尽，等待恢复时间后重试".into();
        if report.error.is_none() {
            report.error = Some(e);
        }
    } else if report.rate_limits.values().any(|r| r.remaining.is_some()) {
        report.pass(4, "已读取响应中的剩余额度与恢复时间");
    } else {
        report.items[4].status = "未知".into();
        report.items[4].detail = "平台未返回限流余量，不能据此判断额度充足".into();
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{MEDIA, Mock, TARGET, VIEWER, configured, login, response};
    use super::*;
    #[test]
    fn diagnostics_run_in_order_read_only_and_redact_credentials() {
        let mut session = Session::default();
        let mut requests = Requests {
            transport: Mock::new(vec![
                response("reachable"),
                response(VIEWER),
                response(TARGET),
                response(MEDIA),
            ]),
            session: &mut session,
            delay: Duration::ZERO,
        };
        let mut report = DiagnosticReport::default();
        report.pass(0, "合成代理路由");
        diagnose_requests(
            &mut requests,
            &mut configured(),
            Ok(login()),
            "media_sample",
            false,
            &mut report,
        );
        assert!(report.items[..4].iter().all(|i| i.status == "通过"));
        assert_eq!(report.items[4].status, "未知");
        assert_eq!(requests.transport.calls.len(), 4);
        assert!(requests.transport.calls[0].1.is_empty());
        let text = serde_json::to_string(&report).unwrap();
        for secret in [
            "synthetic_session_not_real",
            "synthetic_csrf_not_real",
            "synthetic_public_not_real",
        ] {
            assert!(!text.contains(secret));
        }
    }
    #[test]
    fn diagnostics_stop_on_auth_and_rate_limit() {
        let mut session = Session::default();
        let mut requests = Requests {
            transport: Mock::new(vec![
                response("reachable"),
                Ok(super::super::client::Response {
                    status: 401,
                    rate: RateLimit::default(),
                    body: b"private secret".to_vec(),
                }),
            ]),
            session: &mut session,
            delay: Duration::ZERO,
        };
        let mut report = DiagnosticReport::default();
        diagnose_requests(
            &mut requests,
            &mut configured(),
            Ok(login()),
            "",
            false,
            &mut report,
        );
        assert_eq!(report.items[2].status, "未通过");
        assert_eq!(report.items[3].status, "未检查");
        assert_eq!(requests.transport.calls.len(), 2);
        let mut session = Session::default();
        let mut r = response(VIEWER).unwrap();
        r.rate.remaining = Some(0);
        r.rate.reset_at = Some(super::super::client::now() + 100);
        let mut requests = Requests {
            transport: Mock::new(vec![response("reachable"), Ok(r)]),
            session: &mut session,
            delay: Duration::ZERO,
        };
        let mut report = DiagnosticReport::default();
        diagnose_requests(
            &mut requests,
            &mut configured(),
            Ok(login()),
            "",
            false,
            &mut report,
        );
        assert_eq!(requests.transport.calls.len(), 2);
        assert_eq!(report.items[4].status, "限流");
        assert_eq!(report.error.unwrap().category, Category::RateLimit);
    }
}

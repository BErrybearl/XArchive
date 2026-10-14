use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    ProxyNetwork,
    Authentication,
    InterfaceChanged,
    RateLimit,
    InvalidTarget,
    TargetUnavailable,
    LocalState,
    Busy,
}

// Diagnostic text is fixed; network_route contains only a source and host:port.
// Never format upstream bodies, URLs,
// reqwest errors, JSON errors or credentials into a frontend/log message.
#[derive(Clone, Debug, Serialize)]
pub struct PlatformError {
    pub category: Category,
    pub message: &'static str,
    pub suggestion: &'static str,
    pub reset_at: Option<u64>,
    pub http_status: Option<u16>,
    pub network_route: Option<String>,
}

impl PlatformError {
    pub fn new(category: Category) -> Self {
        let (message, suggestion) = match category {
            Category::ProxyNetwork => (
                "代理或网络请求失败",
                "检查已保存的代理、网络和 TLS 连接后手动重试",
            ),
            Category::Authentication => (
                "X 未接受登录状态（401/403 或认证错误）",
                "重新登录后手动重试；403 也可能来自网页验证或缺少必要请求头",
            ),
            Category::InterfaceChanged => (
                "查询 ID、接口参数或响应结构可能已变化",
                "先刷新接口参数；仍失败请反馈错误分类、阶段及状态码",
            ),
            Category::RateLimit => (
                "X 请求额度已耗尽，当前任务已停止",
                "等待显示的恢复时间后手动重试；未返回恢复时间时稍后再试",
            ),
            Category::InvalidTarget => (
                "目标应为有效的 X 用户名或主页链接",
                "填写 @用户名 或 https://x.com/用户名，不能填写推文链接",
            ),
            Category::TargetUnavailable => (
                "目标账号不存在、已停用或当前登录账号无法访问",
                "确认目标用户名及当前账号的访问权限",
            ),
            Category::LocalState => (
                "无法读取本机设置、登录状态或保存接口参数",
                "检查数据目录和设置；无法解密登录状态时重新登录",
            ),
            Category::Busy => ("已有登录、数据或探测操作", "等待当前操作结束后再试"),
        };
        Self {
            category,
            message,
            suggestion,
            reset_at: None,
            http_status: None,
            network_route: None,
        }
    }
    pub fn interface() -> Self {
        Self::new(Category::InterfaceChanged)
    }
    pub fn local() -> Self {
        Self::new(Category::LocalState)
    }
    pub fn rate(reset_at: Option<u64>) -> Self {
        Self {
            reset_at: Some(reset_at.unwrap_or_else(|| super::client::now().saturating_add(900))),
            ..Self::new(Category::RateLimit)
        }
    }
}

pub type Result<T> = std::result::Result<T, PlatformError>;

pub fn status_error(status: u16, reset: Option<u64>) -> Option<PlatformError> {
    let mut error = match status {
        200..=299 => return None,
        401 | 403 => PlatformError::new(Category::Authentication),
        407 => {
            let mut error = PlatformError::new(Category::ProxyNetwork);
            error.message = "代理认证失败，请检查代理的认证要求";
            error
        }
        429 => PlatformError::rate(reset),
        500..=599 => PlatformError::new(Category::ProxyNetwork),
        _ => PlatformError::interface(),
    };
    error.http_status = Some(status);
    Some(error)
}

pub fn decode_json(status: u16, reset: Option<u64>, body: &[u8]) -> Result<Value> {
    if let Some(error) = status_error(status, reset) {
        return Err(error);
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| PlatformError::interface())?;
    if let Some(errors) = value
        .get("errors")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        // Numeric codes only; never echo a platform error message.
        let codes: Vec<i64> = errors
            .iter()
            .filter_map(|e| e.get("code").and_then(Value::as_i64))
            .collect();
        return Err(if codes.contains(&88) {
            PlatformError::rate(reset)
        } else if codes.iter().any(|c| [32, 89, 215, 239].contains(c)) {
            PlatformError::new(Category::Authentication)
        } else {
            PlatformError::interface()
        });
    }
    Ok(value)
}

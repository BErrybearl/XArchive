use super::error::{Category, PlatformError, Result};
use std::{error::Error, io, net::IpAddr};
use url::Url;

// Do not derive Debug: registry values and proxy URLs may contain credentials.
#[derive(Default)]
struct SystemValues {
    enabled: bool,
    server: String,
    bypass: String,
    pac: String,
}

#[derive(Clone, Copy)]
enum Source {
    Settings,
    System,
}

#[derive(Clone)]
pub(crate) struct ProxyPlan {
    source: Source,
    http: Option<Url>,
    https: Option<Url>,
    bypass: Vec<String>,
}

impl ProxyPlan {
    // A deterministic system snapshot: never read the developer's registry in E2E tests.
    #[cfg(test)]
    pub(crate) fn for_test(server: Option<&str>, bypass: &str) -> Self {
        Self::select(
            "",
            &SystemValues {
                enabled: server.is_some(),
                server: server.unwrap_or_default().into(),
                bypass: bypass.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    pub fn resolve(explicit: &str) -> Result<Self> {
        // An explicit setting must not depend on registry access or PAC.
        if !explicit.trim().is_empty() {
            return Self::select(explicit, &SystemValues::default());
        }
        Self::select(explicit, &read_system()?)
    }

    fn select(explicit: &str, system: &SystemValues) -> Result<Self> {
        let mut plan = Self {
            source: Source::System,
            http: None,
            https: None,
            bypass: Vec::new(),
        };
        if !explicit.trim().is_empty() {
            plan.source = Source::Settings;
            let endpoint = parse_endpoint(explicit, "http").map_err(|_| {
                configuration_error("设置中的代理地址无效", "设置中的代理（未发起请求）")
            })?;
            plan.http = Some(endpoint.clone());
            plan.https = Some(endpoint);
            return Ok(plan);
        }
        if !system.pac.trim().is_empty() {
            // Browsers can evaluate PAC; this HTTP client cannot. Never silently
            // make a direct request or guess a route from a configured script.
            return Err(configuration_error(
                "检测到自动配置脚本，请在设置中填写代理地址",
                "系统代理（自动配置脚本未执行，未发起请求）",
            ));
        }
        if !system.enabled {
            return Ok(plan);
        }
        let server = system.server.trim();
        let invalid = || {
            configuration_error(
                "系统代理地址无效，请在设置中填写代理地址",
                "系统代理（未发起请求）",
            )
        };
        if server.is_empty() {
            return Err(invalid());
        }
        if server.split_once('=').is_some_and(|(protocol, _)| {
            !protocol.trim().is_empty() && protocol.trim().bytes().all(|b| b.is_ascii_alphabetic())
        }) {
            let mut socks = None;
            for rule in server.split(';').filter(|part| !part.trim().is_empty()) {
                let (protocol, value) = rule.split_once('=').ok_or_else(invalid)?;
                match protocol.trim().to_ascii_lowercase().as_str() {
                    "http" => {
                        plan.http = Some(parse_endpoint(value, "http").map_err(|_| invalid())?)
                    }
                    // The left side is the destination protocol. A bare HTTPS
                    // mapping still connects to an HTTP CONNECT proxy.
                    "https" => {
                        plan.https = Some(parse_endpoint(value, "http").map_err(|_| invalid())?)
                    }
                    // Windows/Chromium's bare socks=host:port denotes SOCKS4.
                    // An explicit socks5:// or socks5h:// keeps its own scheme.
                    "socks" => {
                        socks = Some(parse_endpoint(value, "socks4").map_err(|_| invalid())?)
                    }
                    _ => {} // FTP and other non-HTTP destination protocols are unused.
                }
            }
            plan.http = plan.http.or_else(|| socks.clone());
            plan.https = plan.https.or(socks);
        } else {
            let endpoint = parse_endpoint(server, "http").map_err(|_| invalid())?;
            plan.http = Some(endpoint.clone());
            plan.https = Some(endpoint);
        }
        plan.bypass = system
            .bypass
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        Ok(plan)
    }

    pub fn endpoint(&self, destination: &Url) -> Option<&Url> {
        if self.bypasses(destination) {
            return None;
        }
        match destination.scheme() {
            "http" => self.http.as_ref(),
            "https" => self.https.as_ref(),
            _ => None,
        }
    }

    fn bypasses(&self, destination: &Url) -> bool {
        let Some(host) = destination.host_str() else {
            return false;
        };
        let host = host.to_ascii_lowercase();
        self.bypass.iter().any(|entry| {
            if entry == "<local>" {
                return !host.contains('.')
                    && host.trim_matches(['[', ']']).parse::<IpAddr>().is_err();
            }
            let mut pattern = entry.as_str();
            if let Some((scheme, rest)) = pattern.split_once("://") {
                if scheme != destination.scheme() {
                    return false;
                }
                pattern = rest;
            }
            if let Some((name, port)) = pattern.rsplit_once(':') {
                // Bracketed IPv6 may have a port; unbracketed IPv6 is a host.
                if (!pattern.contains('[') && pattern.matches(':').count() == 1)
                    || name.ends_with(']')
                {
                    if let Ok(port) = port.parse::<u16>() {
                        if destination.port_or_known_default() != Some(port) {
                            return false;
                        }
                        pattern = name;
                    }
                }
            }
            wildcard_matches(pattern.as_bytes(), host.as_bytes())
        })
    }

    pub fn route(&self, destination: &Url) -> String {
        match self.endpoint(destination) {
            Some(endpoint) => {
                let source = match self.source {
                    Source::Settings => "设置中的代理",
                    Source::System => "系统代理",
                };
                // Deliberately extract only host and port, never URL/userinfo.
                format!(
                    "本次使用：{source}（{}:{}）",
                    endpoint.host_str().unwrap_or("未知主机"),
                    endpoint.port_or_known_default().unwrap_or(1080)
                )
            }
            None if self.bypasses(destination) => "本次使用：直连（系统代理绕过列表）".into(),
            None => "本次使用：直连".into(),
        }
    }

    pub(super) fn failure(&self, destination: &Url, kind: Failure) -> PlatformError {
        transport_error(self.route(destination), kind)
    }
}

fn parse_endpoint(raw: &str, default_scheme: &str) -> std::result::Result<Url, ()> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        return Err(());
    }
    let normalized = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("{default_scheme}://{raw}")
    };
    let mut url = Url::parse(&normalized).map_err(|_| ())?;
    if !matches!(
        url.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    if url.scheme().starts_with("socks") && url.port().is_none() {
        url.set_port(Some(1080)).map_err(|_| ())?;
    }
    if url.port_or_known_default().is_none_or(|port| port == 0) {
        return Err(());
    }
    Ok(url)
}

// Windows ProxyOverride uses shell-style host wildcards, not NO_PROXY's
// implicit domain suffix matching. No regex or network resolution is needed.
fn wildcard_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if let Some(s) = star {
            retry += 1;
            v = retry;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

pub(super) fn transport_error(route: String, kind: Failure) -> PlatformError {
    let mut error = PlatformError::new(Category::ProxyNetwork);
    error.message = kind.message();
    error.network_route = Some(route);
    error
}

fn configuration_error(message: &'static str, route: &str) -> PlatformError {
    let mut error = PlatformError::new(Category::ProxyNetwork);
    error.message = message;
    error.network_route = Some(format!("本次选择：{route}"));
    error
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Connect,
    Timeout,
    Tls,
    ProxyAuthentication,
    Other,
}

impl Failure {
    fn message(self) -> &'static str {
        match self {
            Self::Connect => "连接被拒绝或无法连接，请检查网络和代理地址",
            Self::Timeout => "网络请求超时，请检查网络和代理",
            Self::Tls => "TLS 连接失败，请检查证书和代理的 HTTPS 连接",
            Self::ProxyAuthentication => "代理认证失败，请检查代理的认证要求",
            Self::Other => "网络请求失败，请检查网络和代理",
        }
    }
}

pub(super) fn classify(error: &(dyn Error + 'static), connect: bool, timeout: bool) -> Failure {
    let mut current = Some(error);
    let (mut timed_out, mut tls, mut cannot_connect) = (timeout, false, connect);
    while let Some(cause) = current {
        // reqwest exposes CONNECT 407 as a tunnel error, not an HTTP response.
        // Match known dependency messages internally; never return their text.
        let text = cause.to_string().to_ascii_lowercase();
        if text.contains("proxy authorization required")
            || text.contains("credentials not accepted")
            || text.contains("server does not support user/pass authentication")
            || text.contains("server implements authentication incorrectly")
            || text.contains("proxy authentication required")
            || text.contains("proxy authentication failed")
            || text.contains("socks authentication failed")
            || text.contains("username/password authentication failed")
        {
            return Failure::ProxyAuthentication;
        }
        tls |= cause.is::<rustls::Error>()
            || text.contains("tls handshake")
            || text.contains("invalid peer certificate");
        if let Some(io) = cause.downcast_ref::<io::Error>() {
            timed_out |= io.kind() == io::ErrorKind::TimedOut;
            cannot_connect |= matches!(
                io.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::AddrNotAvailable
            );
            tls |= io
                .get_ref()
                .is_some_and(|inner| inner.is::<rustls::Error>());
        }
        if let Some(reqwest) = cause.downcast_ref::<reqwest::Error>() {
            timed_out |= reqwest.is_timeout();
            cannot_connect |= reqwest.is_connect();
        }
        // io::Error::source can skip its immediate inner error. Inspect that
        // error too so wrapped reqwest timeouts and rustls errors stay classified.
        current = if let Some(io) = cause.downcast_ref::<io::Error>() {
            io.get_ref()
                .map(|inner| inner as &(dyn Error + 'static))
                .or_else(|| cause.source())
        } else {
            cause.source()
        };
    }
    if tls {
        Failure::Tls
    } else if timed_out {
        Failure::Timeout
    } else if cannot_connect {
        Failure::Connect
    } else {
        Failure::Other
    }
}

#[cfg(not(windows))]
fn read_system() -> Result<SystemValues> {
    Ok(SystemValues::default())
}

#[cfg(windows)]
fn read_system() -> Result<SystemValues> {
    use std::ptr;
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
    };
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let failed = || {
        configuration_error(
            "无法读取系统代理，请在设置中填写代理地址",
            "系统代理（未发起请求）",
        )
    };
    let read = |name: &str, flags, data: *mut std::ffi::c_void, size: &mut u32| -> Result<bool> {
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // Read-only HKCU query. No key creation or registry write APIs.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                flags,
                ptr::null_mut(),
                data,
                size,
            )
        };
        match status {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(false),
            _ => Err(failed()),
        }
    };
    let mut enabled = 0u32;
    read(
        "ProxyEnable",
        RRF_RT_REG_DWORD,
        (&mut enabled as *mut u32).cast(),
        &mut 4,
    )?;
    let string = |name: &str| -> Result<String> {
        // Bound the allocation and fail safely on a malformed/oversized value.
        let mut buffer = vec![0u16; 16 * 1024];
        let mut bytes = (buffer.len() * 2) as u32;
        if !read(name, RRF_RT_REG_SZ, buffer.as_mut_ptr().cast(), &mut bytes)? {
            return Ok(String::new());
        }
        if bytes % 2 != 0 || bytes as usize > buffer.len() * 2 {
            return Err(failed());
        }
        buffer.truncate(bytes as usize / 2);
        while buffer.last() == Some(&0) {
            buffer.pop();
        }
        String::from_utf16(&buffer).map_err(|_| failed())
    };
    Ok(SystemValues {
        enabled: enabled != 0,
        server: if enabled != 0 {
            string("ProxyServer")?
        } else {
            String::new()
        },
        bypass: if enabled != 0 {
            string("ProxyOverride")?
        } else {
            String::new()
        },
        pac: string("AutoConfigURL")?,
    })
}

#[cfg(test)]
mod tests;

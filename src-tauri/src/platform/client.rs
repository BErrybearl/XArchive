use super::{
    error::{Category, PlatformError, Result, decode_json, status_error},
    params::{Discovery, Params, script_urls},
};
use crate::auth::PlatformLogin;
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Read,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use url::Url;
use zeroize::Zeroizing;

const MAX_BODY: u64 = 16 * 1024 * 1024;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Default)]
pub struct Session {
    last_request: Option<Instant>,
    pub limits: BTreeMap<String, RateLimit>,
    account_scope: Option<String>,
}

impl Session {
    pub fn for_account(&mut self, account: &str) {
        if self.account_scope.as_deref() != Some(account) {
            self.limits.clear();
            self.account_scope = Some(account.to_owned());
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct RateLimit {
    pub remaining: Option<u64>,
    pub reset_at: Option<u64>,
}
impl RateLimit {
    pub(crate) fn ensure_reset(&mut self) {
        if self.remaining == Some(0) && self.reset_at.is_none() {
            self.reset_at = Some(now().saturating_add(900));
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn rate_headers(headers: &HeaderMap) -> RateLimit {
    let number = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
    };
    RateLimit {
        remaining: number("x-rate-limit-remaining"),
        reset_at: number("x-rate-limit-reset"),
    }
}

pub struct Response {
    pub status: u16,
    pub rate: RateLimit,
    pub body: Vec<u8>,
}

pub trait Transport {
    fn get(&mut self, url: Url, headers: HeaderMap) -> Result<Response>;
    #[cfg(test)]
    fn graphql_base(&self) -> Option<&Url> {
        None
    }
}

pub struct Http {
    client: Client,
    proxy: super::proxy::ProxyPlan,
    #[cfg(test)]
    graphql_base: Option<Url>,
}

impl Http {
    pub fn new(proxy: &str) -> Result<Self> {
        let proxy = super::proxy::ProxyPlan::resolve(proxy)?;
        Self::with_routing(proxy)
    }

    fn with_routing(proxy: super::proxy::ProxyPlan) -> Result<Self> {
        // Use the explicitly enabled rustls ring provider, with normal certificate verification.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let routing = proxy.clone();
        let client = Client::builder()
            // Avoid environment proxies overriding Windows settings or explicit settings.
            // Our custom matcher is the sole source of proxy and bypass decisions.
            .no_proxy()
            .proxy(reqwest::Proxy::custom(move |url| {
                routing.endpoint(url).cloned()
            }))
            .user_agent(UA)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| {
                proxy.failure(
                    &Url::parse("https://x.com/").expect("static URL"),
                    super::proxy::Failure::Other,
                )
            })?;
        Ok(Self {
            client,
            proxy,
            #[cfg(test)]
            graphql_base: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(proxy: super::proxy::ProxyPlan, base: Url) -> Result<Self> {
        assert_eq!(base.scheme(), "http");
        assert_eq!(base.host_str(), Some("127.0.0.1"));
        let mut http = Self::with_routing(proxy)?;
        http.graphql_base = Some(base);
        Ok(http)
    }
}

impl Transport for Http {
    #[cfg(test)]
    fn graphql_base(&self) -> Option<&Url> {
        self.graphql_base.as_ref()
    }

    fn get(&mut self, url: Url, headers: HeaderMap) -> Result<Response> {
        let response = self
            .client
            .get(url.clone())
            .headers(headers)
            .send()
            .map_err(|e| {
                self.proxy.failure(
                    &url,
                    super::proxy::classify(&e, e.is_connect(), e.is_timeout()),
                )
            })?;
        let status = response.status().as_u16();
        let rate = rate_headers(response.headers());
        // Classify HTTP failures before reading potentially huge/error bodies.
        if let Some(mut error) = status_error(status, rate.reset_at) {
            if error.category == Category::ProxyNetwork {
                error.network_route = Some(self.proxy.route(&url));
                return Err(error);
            }
            return Ok(Response {
                status,
                rate,
                body: Vec::new(),
            });
        }
        if response.content_length().is_some_and(|n| n > MAX_BODY) {
            return Err(PlatformError::interface());
        }
        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|e| {
                self.proxy
                    .failure(&url, super::proxy::classify(&e, false, false))
            })?;
        if body.len() as u64 > MAX_BODY {
            return Err(PlatformError::interface());
        }
        Ok(Response { status, rate, body })
    }
}

fn sensitive(raw: &str) -> Result<HeaderValue> {
    let mut value = HeaderValue::from_str(raw).map_err(|_| PlatformError::local())?;
    value.set_sensitive(true);
    Ok(value)
}

pub fn api_headers(login: &PlatformLogin, bearer: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        sensitive(&Zeroizing::new(format!("Bearer {bearer}")))?,
    );
    headers.insert(
        "cookie",
        sensitive(&Zeroizing::new(format!(
            "auth_token={}; ct0={}",
            *login.auth_token, *login.ct0
        )))?,
    );
    headers.insert("x-csrf-token", sensitive(&login.ct0)?);
    headers.insert("x-twitter-active-user", HeaderValue::from_static("yes"));
    headers.insert(
        "x-twitter-auth-type",
        HeaderValue::from_static("OAuth2Session"),
    );
    headers.insert("x-twitter-client-language", HeaderValue::from_static("en"));
    headers.insert("accept", HeaderValue::from_static("application/json"));
    headers.insert("origin", HeaderValue::from_static("https://x.com"));
    headers.insert("referer", HeaderValue::from_static("https://x.com/"));
    // The preserved Python source/patch has no transaction-ID implementation.
    // Do not invent a UUID/static value. A future verified implementation needs
    // the current site-verification key and on-demand JS/animation algorithm.
    Ok(headers)
}

pub struct Requests<'a, T> {
    pub transport: T,
    pub session: &'a mut Session,
    pub delay: Duration,
}

impl<T: Transport> Requests<'_, T> {
    pub(super) fn get(
        &mut self,
        operation: &str,
        url: Url,
        headers: HeaderMap,
    ) -> Result<Response> {
        if let Some(rate) = self.session.limits.get_mut(operation) {
            rate.ensure_reset();
            if rate.remaining == Some(0) && rate.reset_at.is_none_or(|reset| reset > now()) {
                return Err(PlatformError::rate(rate.reset_at));
            }
        }
        if let Some(previous) = self.session.last_request {
            let wait = self.delay.saturating_sub(previous.elapsed());
            if !wait.is_zero() {
                thread::sleep(wait);
            }
        }
        self.session.last_request = Some(Instant::now());
        let mut response = self.transport.get(url, headers)?;
        if response.status == 429 {
            response.rate.remaining = Some(0);
        }
        response.rate.ensure_reset();
        self.session
            .limits
            .insert(operation.to_owned(), response.rate.clone());
        if let Some(error) = status_error(response.status, response.rate.reset_at) {
            return Err(error);
        }
        Ok(response)
    }

    pub fn query(
        &mut self,
        operation: &str,
        variables: Value,
        params: &Params,
        login: &PlatformLogin,
    ) -> Result<Value> {
        let id = params
            .query_ids
            .get(operation)
            .ok_or_else(PlatformError::interface)?;
        let mut url = Url::parse(&format!("https://x.com/i/api/graphql/{id}/{operation}"))
            .map_err(|_| PlatformError::interface())?;
        #[cfg(test)]
        if let Some(base) = self.transport.graphql_base() {
            url = base
                .join(&format!("i/api/graphql/{id}/{operation}"))
                .map_err(|_| PlatformError::interface())?;
        }
        let features = serde_json::to_string(
            params
                .features
                .get(operation)
                .ok_or_else(PlatformError::interface)?,
        )
        .map_err(|_| PlatformError::interface())?;
        let toggles = serde_json::to_string(
            params
                .field_toggles
                .get(operation)
                .ok_or_else(PlatformError::interface)?,
        )
        .map_err(|_| PlatformError::interface())?;
        url.query_pairs_mut()
            .append_pair("variables", &variables.to_string())
            .append_pair("features", &features)
            .append_pair("fieldToggles", &toggles);
        let response = self.get(operation, url, api_headers(login, &params.public_bearer)?)?;
        let decoded = decode_json(response.status, response.rate.reset_at, &response.body);
        if decoded
            .as_ref()
            .is_err_and(|e| e.category == Category::RateLimit)
        {
            self.session.limits.insert(
                operation.to_owned(),
                RateLimit {
                    remaining: Some(0),
                    reset_at: decoded.as_ref().err().and_then(|e| e.reset_at),
                },
            );
        }
        decoded
    }

    pub fn discover(
        &mut self,
        login: Option<&PlatformLogin>,
        require_bearer: bool,
    ) -> Result<Discovery> {
        let mut urls = Vec::new();
        // The authenticated document can expose the responsive-web bundle (as in Python).
        // Only x.com gets Cookie, never API Authorization/CSRF. CDN requests have
        // no session headers. No redirects; scripts use the exact CDN HTTPS origin.
        for page in ["https://x.com/home", "https://x.com/"] {
            let mut headers = HeaderMap::new();
            headers.insert("accept", HeaderValue::from_static("text/html"));
            if let Some(login) = login {
                headers.insert(
                    "cookie",
                    sensitive(&Zeroizing::new(format!(
                        "auth_token={}; ct0={}",
                        *login.auth_token, *login.ct0
                    )))?,
                );
            }
            let response =
                self.get("PublicPage", Url::parse(page).expect("static URL"), headers)?;
            let html = String::from_utf8(response.body).map_err(|_| PlatformError::interface())?;
            urls = script_urls(&html);
            if !urls.is_empty() {
                break;
            }
        }
        if urls.is_empty() {
            return Err(PlatformError::interface());
        }
        let mut found = Discovery::default();
        for url in urls {
            let response = self.get("PublicScript", url, HeaderMap::new())?;
            let text = String::from_utf8(response.body).map_err(|_| PlatformError::interface())?;
            found.scan(&text);
            if found.complete()
                || (!require_bearer
                    && super::params::OPERATIONS
                        .iter()
                        .all(|op| found.ids.contains_key(*op)))
            {
                return Ok(found);
            }
        }
        let mut error = PlatformError::interface();
        error.message = "公开脚本中未发现全部查询 ID 或所需公开 Bearer，原参数未改动";
        Err(error)
    }
}

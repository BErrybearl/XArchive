use crate::platform::proxy::ProxyPlan;
use reqwest::{blocking::Client, header::HeaderMap};
use serde::Serialize;
use std::{io::Read, time::Duration};
use url::Url;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadError {
    UnsafeUrl,
    Redirect,
    Http,
    Network,
    EmptyOrTruncated,
}
impl DownloadError {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnsafeUrl => "媒体地址不在 HTTPS 下载白名单",
            Self::Redirect => "媒体重定向已拒绝",
            Self::Http => "媒体服务器返回失败状态",
            Self::Network => "媒体连接或读取失败",
            Self::EmptyOrTruncated => "媒体为空、截断或未完整读取",
        }
    }
}
pub fn allowed(raw: &str) -> Result<Url, DownloadError> {
    let url = Url::parse(raw).map_err(|_| DownloadError::UnsafeUrl)?;
    if url.scheme() != "https"
        || !matches!(url.host_str(), Some("pbs.twimg.com" | "video.twimg.com"))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return Err(DownloadError::UnsafeUrl);
    }
    Ok(url)
}
// The media transport has no access to login credentials. All redirects are refused,
// including same-host redirects, so it cannot send a follow-up request to another host.
pub trait MediaTransport {
    fn url(&self, raw: &str) -> Result<Url, DownloadError> {
        allowed(raw)
    }
    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        None
    }
    fn open(&mut self, url: Url, headers: HeaderMap) -> Result<Box<dyn Read>, DownloadError>;
}
#[derive(Clone)]
pub struct HttpMedia {
    client: Client,
    #[cfg(test)]
    allowed_origin: Option<Url>,
}
impl HttpMedia {
    pub fn new(proxy: &str) -> crate::platform::error::Result<Self> {
        let routing = ProxyPlan::resolve(proxy)?;
        Self::with_routing(routing)
    }

    fn with_routing(routing: ProxyPlan) -> crate::platform::error::Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = Client::builder()
            .no_proxy()
            .proxy(reqwest::Proxy::custom(move |url| {
                routing.endpoint(url).cloned()
            }))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|_| crate::platform::PlatformError::local())?;
        Ok(Self {
            client,
            #[cfg(test)]
            allowed_origin: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        routing: ProxyPlan,
        origin: Url,
    ) -> crate::platform::error::Result<Self> {
        assert_eq!(origin.scheme(), "http");
        assert_eq!(origin.host_str(), Some("127.0.0.1"));
        let mut media = Self::with_routing(routing)?;
        media.allowed_origin = Some(origin);
        Ok(media)
    }
}
pub fn response_status(status: u16) -> Result<(), DownloadError> {
    match status {
        200 => Ok(()),
        300..=399 => Err(DownloadError::Redirect),
        _ => Err(DownloadError::Http),
    }
}
impl MediaTransport for HttpMedia {
    fn url(&self, raw: &str) -> Result<Url, DownloadError> {
        #[cfg(test)]
        if let Some(origin) = &self.allowed_origin {
            let url = Url::parse(raw).map_err(|_| DownloadError::UnsafeUrl)?;
            return if url.origin() == origin.origin()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
            {
                Ok(url)
            } else {
                Err(DownloadError::UnsafeUrl)
            };
        }
        allowed(raw)
    }

    fn worker(&self) -> Option<Box<dyn MediaTransport + Send>> {
        Some(Box::new(self.clone()))
    }
    fn open(&mut self, url: Url, headers: HeaderMap) -> Result<Box<dyn Read>, DownloadError> {
        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .map_err(|_| DownloadError::Network)?;
        response_status(response.status().as_u16())?;
        let expected = response.content_length();
        Ok(Box::new(CheckedBody {
            body: response,
            expected,
            read: 0,
        }))
    }
}
struct CheckedBody<R> {
    body: R,
    expected: Option<u64>,
    read: u64,
}
impl<R: Read> Read for CheckedBody<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.body.read(buf)?;
        self.read += n as u64;
        if n == 0 && (self.read == 0 || self.expected.is_some_and(|n| n != self.read)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "媒体未完整读取",
            ));
        }
        Ok(n)
    }
}

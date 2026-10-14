use super::*;
use client::{Requests, Response, Transport, rate_headers};
use error::{Category, decode_json, status_error};
use proxy::{Failure, transport_error};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::{collections::VecDeque, fs, path::Path, time::Duration};
use url::Url;
use zeroize::Zeroizing;

pub(super) const MEDIA: &str = include_str!("fixtures/media-page.json");
pub(super) const VIEWER: &str = include_str!("fixtures/viewer.json");
pub(super) const TARGET: &str = include_str!("fixtures/target.json");
const SCRIPT: &str = include_str!("fixtures/public-script.js");
const PAGE: &str = include_str!("fixtures/public-page.html");

fn value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap()
}
fn directory() -> tempfile::TempDir {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/platform-tests");
    fs::create_dir_all(&path).unwrap();
    tempfile::tempdir_in(path).unwrap()
}
pub(super) fn login() -> auth::PlatformLogin {
    auth::PlatformLogin {
        auth_token: Zeroizing::new("synthetic_session_not_real".into()),
        ct0: Zeroizing::new("synthetic_csrf_not_real".into()),
        account: parser::viewer(&value(VIEWER)).unwrap(),
        proxy: String::new(),
    }
}
pub(super) fn configured() -> params::Params {
    let mut params = params::defaults();
    params.public_bearer = "synthetic_public_not_real".into();
    params
}
pub(super) fn response(body: &str) -> error::Result<Response> {
    Ok(Response {
        status: 200,
        rate: RateLimit::default(),
        body: body.as_bytes().to_vec(),
    })
}
#[derive(Default)]
pub(super) struct Mock {
    responses: VecDeque<error::Result<Response>>,
    pub(super) calls: Vec<(Url, HeaderMap)>,
}
impl Mock {
    pub(super) fn new(responses: Vec<error::Result<Response>>) -> Self {
        Self {
            responses: responses.into(),
            calls: Vec::new(),
        }
    }
}
impl Transport for Mock {
    fn get(&mut self, url: Url, headers: HeaderMap) -> error::Result<Response> {
        self.calls.push((url, headers));
        self.responses
            .pop_front()
            .expect("unexpected request: fixture queue exhausted")
    }
}
fn script_with_synthetic_bearer() -> String {
    format!("{SCRIPT}\n\"AAAAA{}\"", "0".repeat(45))
}

#[test]
fn missing_rate_reset_defaults_to_fifteen_minutes_and_expires() {
    for status in [200, 429] {
        let mut session = Session::default();
        let before = client::now();
        let mut requests = Requests {
            transport: Mock::new(vec![
                Ok(Response {
                    status,
                    rate: RateLimit::default(),
                    body: br#"{"errors":[{"code":88}]}"#.to_vec(),
                }),
                response(VIEWER),
            ]),
            session: &mut session,
            delay: Duration::ZERO,
        };
        let params = configured();
        let error = requests
            .query("Viewer", json!({}), &params, &login())
            .unwrap_err();
        assert_eq!(error.category, Category::RateLimit);
        let reset = error.reset_at.unwrap();
        assert!(reset >= before + 900 && reset <= client::now() + 900);
        assert_eq!(requests.session.limits["Viewer"].reset_at, Some(reset));
        assert!(
            requests
                .query("Viewer", json!({}), &params, &login())
                .is_err()
        );
        assert_eq!(requests.transport.calls.len(), 1);
        requests.session.limits.get_mut("Viewer").unwrap().reset_at = Some(client::now() - 1);
        assert!(
            requests
                .query("Viewer", json!({}), &params, &login())
                .is_ok()
        );
        assert_eq!(requests.transport.calls.len(), 2);
    }
}

#[test]
fn media_types_retweets_unavailable_and_cursor() {
    assert_eq!(
        parser::media_page(&value(MEDIA)).unwrap(),
        parser::MediaStats {
            tweets: 4,
            images: 3,
            videos: 1,
            gifs: 1,
            unavailable: 2,
            has_next_page: true,
        }
    );
    let sample = value(MEDIA);
    let video = sample.pointer("/data/user/result/timeline_v2/timeline/instructions/0/entries/0/content/items/1/item/itemContent/tweet_results/result/legacy/extended_entities/media/0").unwrap();
    assert_eq!(
        parser::best_mp4(video),
        Some("https://example.invalid/high.mp4")
    );
    let gif = sample.pointer("/data/user/result/timeline_v2/timeline/instructions/0/entries/0/content/items/2/item/itemContent/tweet_results/result/tweet/legacy/extended_entities/media/0").unwrap();
    assert_eq!(
        parser::best_mp4(gif),
        Some("https://example.invalid/animation.mp4")
    );
}

#[test]
fn empty_page_unknown_structure_and_alternate_timeline() {
    assert_eq!(
        parser::media_page(
            &json!({"data":{"user":{"result":{"timeline":{"timeline":{"instructions":[]}}}}}})
        )
        .unwrap(),
        parser::MediaStats::default()
    );
    assert!(parser::media_page(&json!({"data":{}})).is_err());
    assert!(parser::media_page(&json!({"data":{"user":{"result":{"timeline":{"instructions":[{"type":"NewUnknownFormat"}]}}}}})).is_err());
    let sample = value(MEDIA);
    let entries = sample
        .pointer("/data/user/result/timeline_v2/timeline/instructions/0/entries/0/content/items")
        .unwrap();
    let page = json!({"data":{"user":{"result":{"timeline":{"instructions":[
        {"type":"TimelineAddToModule", "moduleItems":entries},
        {"type":"TimelineReplaceEntry", "entry":{"content":{"cursorType":"Bottom","value":""}}}
    ]}}}}});
    let stats = parser::media_page(&page).unwrap();
    assert_eq!(stats.tweets, 4);
    assert!(!stats.has_next_page);
}

#[test]
fn current_and_target_accounts_and_target_input() {
    assert_eq!(
        parser::viewer(&value(VIEWER)).unwrap().screen_name,
        "probe_sample"
    );
    assert_eq!(
        parser::target(&value(TARGET)).unwrap().screen_name,
        "media_sample"
    );
    assert_eq!(
        parser::target_name(" @media_sample ").unwrap(),
        "media_sample"
    );
    assert_eq!(
        parser::target_name("https://x.com/media_sample/?s=20").unwrap(),
        "media_sample"
    );
    for raw in [
        "https://evil.invalid/name",
        "https://x.com/name/status/1",
        "https://user:password@x.com/name",
        "@bad-name",
        "",
        "https://x.com/home",
    ] {
        assert_eq!(
            parser::target_name(raw).unwrap_err().category,
            Category::InvalidTarget
        );
    }
    assert_eq!(
        parser::target(&json!({"data":{"user":{"result":{"__typename":"UserUnavailable"}}}}))
            .unwrap_err()
            .category,
        Category::TargetUnavailable
    );
}

#[test]
fn http_and_json_errors_are_classified_without_upstream_text() {
    for (status, category) in [
        (401, Category::Authentication),
        (403, Category::Authentication),
        (404, Category::InterfaceChanged),
        (400, Category::InterfaceChanged),
        (429, Category::RateLimit),
        (503, Category::ProxyNetwork),
    ] {
        let error = decode_json(
            status,
            Some(1800000000),
            b"private response synthetic_session_not_real",
        )
        .unwrap_err();
        assert_eq!(error.category, category);
        assert_eq!(error.http_status, Some(status));
        if status == 429 {
            assert_eq!(error.reset_at, Some(1800000000));
        }
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("synthetic_session_not_real")
        );
    }
    assert_eq!(
        decode_json(200, None, b"<html>private challenge</html>")
            .unwrap_err()
            .category,
        Category::InterfaceChanged
    );
    assert_eq!(
        decode_json(
            200,
            None,
            br#"{"errors":[{"code":89,"message":"private token"}]}"#
        )
        .unwrap_err()
        .category,
        Category::Authentication
    );
    assert_eq!(
        decode_json(200, Some(123), br#"{"errors":[{"code":88}]}"#)
            .unwrap_err()
            .reset_at,
        Some(123)
    );
    assert_eq!(
        decode_json(
            200,
            None,
            br#"{"errors":[{"code":999,"message":"query ID unknown"}]}"#
        )
        .unwrap_err()
        .category,
        Category::InterfaceChanged
    );
    assert!(status_error(200, None).is_none());
    assert_eq!(
        transport_error(
            "本次使用：设置中的代理（proxy.example:8080）".into(),
            Failure::Connect
        )
        .category,
        Category::ProxyNetwork
    );
    assert!(
        transport_error(
            "本次使用：设置中的代理（proxy.example:8080）".into(),
            Failure::Connect
        )
        .message
        .contains("代理")
    );
}

#[test]
fn rate_limit_headers_and_no_retry_429() {
    let mut headers = HeaderMap::new();
    headers.insert("x-rate-limit-remaining", HeaderValue::from_static("0"));
    let reset = client::now() + 1000;
    headers.insert(
        "x-rate-limit-reset",
        HeaderValue::from_str(&reset.to_string()).unwrap(),
    );
    let rate = rate_headers(&headers);
    assert_eq!(rate.remaining, Some(0));
    assert_eq!(rate.reset_at, Some(reset));
    headers.insert("x-rate-limit-reset", HeaderValue::from_static("invalid"));
    assert_eq!(rate_headers(&headers).reset_at, None);
    let mock = Mock::new(vec![Ok(Response {
        status: 429,
        rate,
        body: Vec::new(),
    })]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    let error = requests
        .query("Viewer", json!({}), &configured(), &login())
        .unwrap_err();
    assert_eq!(error.category, Category::RateLimit);
    assert_eq!(error.reset_at, Some(reset));
    assert_eq!(requests.transport.calls.len(), 1);
    assert_eq!(
        requests
            .query("Viewer", json!({}), &configured(), &login())
            .unwrap_err()
            .category,
        Category::RateLimit
    );
    assert_eq!(requests.transport.calls.len(), 1);
}

#[test]
fn authenticated_headers_are_sensitive_public_scripts_receive_none() {
    let login = login();
    let headers = client::api_headers(&login, &configured().public_bearer).unwrap();
    for name in ["cookie", "authorization", "x-csrf-token"] {
        assert!(headers.get(name).unwrap().is_sensitive());
    }
    let debug = format!("{headers:?}");
    assert!(!debug.contains(login.auth_token.as_str()));
    assert!(!debug.contains(login.ct0.as_str()));
    let mock = Mock::new(vec![
        response(PAGE),
        response(&script_with_synthetic_bearer()),
    ]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    assert!(requests.discover(None, true).unwrap().complete());
    assert_eq!(requests.transport.calls.len(), 2);
    for (_, headers) in &requests.transport.calls {
        for name in ["cookie", "authorization", "x-csrf-token"] {
            assert!(!headers.contains_key(name));
        }
    }
    assert_eq!(
        requests.transport.calls[1].0.host_str(),
        Some("abs.twimg.com")
    );
}

#[test]
fn parameter_overrides_merge_and_corruption_falls_back_without_writes() {
    let dir = directory();
    let path = dir.path().join("platform.json");
    let (params, notices) = params::load(dir.path());
    assert_eq!(params.query_ids, params::defaults().query_ids);
    assert!(notices[0].contains("缺失"));
    assert!(!path.exists());
    let override_text = br#"{"query_ids":{"UserMedia":"override_id"},"features":{"UserMedia":{"new_flag":true}},"field_toggles":{"UserMedia":{"with_new_field":false}}}"#;
    fs::write(&path, override_text).unwrap();
    let (params, _) = params::load(dir.path());
    assert_eq!(params.query_ids["UserMedia"], "override_id");
    assert_eq!(
        params.query_ids["Viewer"],
        params::defaults().query_ids["Viewer"]
    );
    assert!(params.features["UserMedia"]["new_flag"]);
    assert!(params.features["UserMedia"].len() > 1);
    assert!(!params.field_toggles["UserMedia"]["with_new_field"]);
    assert_eq!(fs::read(&path).unwrap(), override_text);
    for invalid in [
        b"{broken".as_slice(),
        br#"{"version":99}"#,
        br#"{"query_ids":{"Viewer":"../evil"}}"#,
        br#"{"features":{"UserMedia":{"new_flag":"wrong"}}}"#,
        br#"{"public_bearer":"bad\r\nheader"}"#,
        b"null",
    ] {
        fs::write(&path, invalid).unwrap();
        let (fallback, notices) = params::load(dir.path());
        assert_eq!(fallback.query_ids, params::defaults().query_ids);
        assert!(notices[0].contains("损坏"));
        assert_eq!(fs::read(&path).unwrap(), invalid);
    }
}

#[test]
fn discovery_supports_both_orders_and_restricts_script_origins() {
    let mut discovery = params::Discovery::default();
    discovery.scan(SCRIPT);
    assert_eq!(discovery.ids.len(), 4);
    assert_eq!(discovery.ids["UserByScreenName"], "synthetic_user");
    assert!(!discovery.complete());
    discovery.scan(&script_with_synthetic_bearer());
    assert!(discovery.complete());
    let html = format!(
        "{PAGE} https://evil.invalid/main.js https://abs.twimg.com.evil.invalid/main.js https://abs.twimg.com/other/main.js https://abs.twimg.com/x-web/client/extra.js"
    );
    let urls = params::script_urls(&html);
    assert_eq!(urls.len(), 2);
    assert!(urls.iter().all(|u| u.host_str() == Some("abs.twimg.com")));
}

#[test]
fn successful_refresh_commits_and_failure_preserves_original_exactly() {
    let dir = directory();
    let path = dir.path().join("platform.json");
    let original_bytes = b"{broken original bytes}";
    fs::write(&path, original_bytes).unwrap();
    let original = configured();
    for failure in [
        Err(PlatformError::new(Category::ProxyNetwork)),
        Err(PlatformError::interface()),
        Ok(params::Discovery::default()),
    ] {
        assert!(params::commit_refresh(dir.path(), &original, failure).is_err());
        assert_eq!(fs::read(&path).unwrap(), original_bytes);
    }
    let mut discovered = params::Discovery::default();
    discovered.scan(&script_with_synthetic_bearer());
    params::commit_refresh(dir.path(), &original, Ok(discovered)).unwrap();
    let (updated, _) = params::load(dir.path());
    assert_eq!(updated.query_ids["UserMedia"], "synthetic_media");
    assert_eq!(updated.features, original.features);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1); // Temporary file removed after commit.
}

#[test]
fn offline_probe_flow_is_read_only_and_ipc_contains_only_statistics() {
    let dir = directory();
    let store = storage::Store::open(dir.path()).unwrap();
    drop(store);
    let database = dir.path().join("archive.sqlite3");
    let before = fs::read(&database).unwrap();
    let mock = Mock::new(vec![response(VIEWER), response(TARGET), response(MEDIA)]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    let mut report = ProbeResult::failure(PlatformError::local());
    report.error = None;
    probe_requests(
        &mut requests,
        &mut configured(),
        &login(),
        "media_sample",
        &mut report,
    )
    .unwrap();
    assert_eq!(requests.transport.calls.len(), 3);
    assert!(requests.transport.responses.is_empty());
    assert_eq!(report.stats.as_ref().unwrap().tweets, 4);
    assert_eq!(fs::read(database).unwrap(), before);
    assert!(!dir.path().join("platform.json").exists());
    assert!(
        fs::read_dir(dir.path().join("media"))
            .unwrap()
            .next()
            .is_none()
    );
    let ipc = serde_json::to_string(&report).unwrap();
    for secret in [
        "synthetic_session_not_real",
        "synthetic_csrf_not_real",
        "synthetic_public_not_real",
        "auth_token",
        "ct0",
        "video_info",
        "example.invalid",
    ] {
        assert!(!ipc.contains(secret));
    }
    assert!(
        !serde_json::to_string(&RefreshResult::failure(PlatformError::interface()))
            .unwrap()
            .contains("public_bearer")
    );
}

#[test]
fn probe_stops_on_auth_failure_or_account_mismatch() {
    let reset = client::now() + 1000;
    for status in [401, 403, 404, 429] {
        let mock = Mock::new(vec![Ok(Response {
            status,
            rate: RateLimit {
                remaining: None,
                reset_at: Some(reset),
            },
            body: Vec::new(),
        })]);
        let mut session = Session::default();
        let mut requests = Requests {
            transport: mock,
            session: &mut session,
            delay: Duration::ZERO,
        };
        let mut report = ProbeResult::failure(PlatformError::local());
        let error = probe_requests(
            &mut requests,
            &mut configured(),
            &login(),
            "media_sample",
            &mut report,
        )
        .unwrap_err();
        assert!(report.stats.is_none());
        assert_eq!(requests.transport.calls.len(), 1);
        if status == 429 {
            assert_eq!(error.reset_at, Some(reset));
        }
    }
    let mut wrong = login();
    wrong.account.user_id = "99999".into();
    let mock = Mock::new(vec![response(VIEWER)]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    let mut report = ProbeResult::failure(PlatformError::local());
    assert_eq!(
        probe_requests(
            &mut requests,
            &mut configured(),
            &wrong,
            "media_sample",
            &mut report
        )
        .unwrap_err()
        .category,
        Category::Authentication
    );
    assert_eq!(requests.transport.calls.len(), 1);
}

#[test]
fn public_discovery_failures_never_use_api_credentials_or_retry() {
    let mock = Mock::new(vec![
        response("<html>no scripts</html>"),
        response("<html>no scripts</html>"),
    ]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    assert!(requests.discover(None, true).is_err());
    assert_eq!(requests.transport.calls.len(), 2);
    let mock = Mock::new(vec![Err(transport_error(
        "本次使用：设置中的代理（proxy.example:8080）".into(),
        Failure::Connect,
    ))]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    assert_eq!(
        requests.discover(None, true).err().unwrap().category,
        Category::ProxyNetwork
    );
    assert_eq!(requests.transport.calls.len(), 1);
}

#[test]
fn production_platform_diagnostics_never_log_raw_values() {
    for source in [
        include_str!("mod.rs"),
        include_str!("client.rs"),
        include_str!("error.rs"),
        include_str!("params.rs"),
        include_str!("parser.rs"),
    ] {
        for logger in ["println!", "eprintln!", "dbg!", "log::", "tracing::"] {
            assert!(!source.contains(logger));
        }
        assert!(!source.contains("error.to_string()"));
    }
    for fixture in [MEDIA, VIEWER, TARGET, PAGE, SCRIPT] {
        for credential in ["auth_token", "ct0", "Cookie", "Bearer", "AAAAA"] {
            assert!(!fixture.contains(credential));
        }
    }
}

#[test]
fn authenticated_document_cookie_stays_on_x_and_bootstrap_is_memory_only() {
    let mock = Mock::new(vec![
        response(PAGE),
        response(&script_with_synthetic_bearer()),
        response(VIEWER),
        response(TARGET),
        response(MEDIA),
    ]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    let mut report = ProbeResult::failure(PlatformError::local());
    let mut params = params::defaults();
    probe_requests(
        &mut requests,
        &mut params,
        &login(),
        "media_sample",
        &mut report,
    )
    .unwrap();
    assert_eq!(requests.transport.calls.len(), 5);
    let (document, headers) = &requests.transport.calls[0];
    assert_eq!(document.host_str(), Some("x.com"));
    assert!(headers.get("cookie").unwrap().is_sensitive());
    assert!(!headers.contains_key("authorization"));
    assert!(!headers.contains_key("x-csrf-token"));
    assert_eq!(
        requests.transport.calls[1].0.host_str(),
        Some("abs.twimg.com")
    );
    assert!(requests.transport.calls[1].1.is_empty());
    assert_eq!(params.query_ids["Viewer"], "synthetic_viewer");
    assert!(!params.public_bearer.is_empty());
    assert_eq!(report.stats.as_ref().unwrap().tweets, 4);
    assert_eq!(params::defaults().public_bearer, "");
}

#[test]
fn refreshing_ids_can_keep_existing_bearer_and_unknown_posts_fail_visibly() {
    let mock = Mock::new(vec![response(PAGE), response(SCRIPT)]);
    let mut session = Session::default();
    let mut requests = Requests {
        transport: mock,
        session: &mut session,
        delay: Duration::ZERO,
    };
    let found = requests.discover(None, false).unwrap();
    assert_eq!(found.ids.len(), 4);
    assert!(found.bearer.is_none());
    let dir = directory();
    let original = configured();
    let saved = params::commit_refresh(dir.path(), &original, Ok(found)).unwrap();
    assert_eq!(saved.public_bearer, original.public_bearer);
    assert_eq!(
        parser::target(&json!({"data":{}})).unwrap_err().category,
        Category::InterfaceChanged
    );
    let broken = json!({"data":{"user":{"result":{"timeline":{"instructions":[{"type":"TimelineAddEntries","entries":[{"content":{"newUnknownItem":true}}]}]}}}}});
    assert_eq!(
        parser::media_page(&broken).unwrap_err().category,
        Category::InterfaceChanged
    );
    session.for_account("90000");
    session.limits.insert(
        "Viewer".into(),
        RateLimit {
            remaining: Some(0),
            reset_at: Some(client::now() + 1000),
        },
    );
    session.for_account("90100");
    assert!(session.limits.is_empty());
}

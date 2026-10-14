use super::*;
use crate::platform::client::{RateLimit, Response, Transport};
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

fn fixture() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".runtime/auth-tests");
    fs::create_dir_all(&root).unwrap();
    let directory = tempfile::tempdir_in(root).unwrap();
    storage::Store::open(directory.path()).unwrap();
    directory
}
fn sample() -> SavedLogin {
    SavedLogin {
        version: 1,
        credentials: Credentials {
            auth_token: "synthetic_auth_not_real".into(),
            ct0: "synthetic_csrf_not_real".into(),
        },
        account: Account {
            user_id: "20000000000000000001".into(),
            screen_name: "sample_user".into(),
            display_name: "合成账号".into(),
        },
        verified_at: 1234,
    }
}
fn cookie(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value, "domain": ".x.com", "path": "/", "secure": true, "expires": -1})
}
fn cookies() -> Value {
    json!({"cookies": [cookie("auth_token", "synthetic_auth_not_real"), cookie("ct0", "synthetic_csrf_not_real")]})
}
struct FakeBrowser {
    busy: Arc<AtomicBool>,
    reads: Arc<AtomicUsize>,
    fail: bool,
}
impl browser::LoginBrowser for FakeBrowser {
    fn in_use(&mut self, _: &Path) -> Result<bool> {
        Ok(self.busy.load(Ordering::Relaxed))
    }
    fn read(&mut self, _: &Path) -> Result<Credentials> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            Err(AuthError::BrowserConnection)
        } else {
            capture_cookies(&mut cookies())
        }
    }
}
fn pending(root: &Path) -> PendingLogin {
    let mut pending = PendingLogin::prepare(root, "").unwrap();
    pending.browser = Some(Box::new(FakeBrowser {
        busy: Arc::new(AtomicBool::new(false)),
        reads: Arc::new(AtomicUsize::new(0)),
        fail: false,
    }));
    pending
}
fn manager(root: &Path) -> LoginManager {
    LoginManager {
        pending: Some(pending(root)),
        cleanup_warning: None,
    }
}
struct FakeTransport {
    responses: VecDeque<Response>,
}
impl Transport for FakeTransport {
    fn get(
        &mut self,
        url: Url,
        headers: HeaderMap,
    ) -> std::result::Result<Response, platform::PlatformError> {
        assert_eq!(url.scheme(), "https");
        if url.host_str() == Some("abs.twimg.com") {
            for name in ["cookie", "authorization", "x-csrf-token"] {
                assert!(!headers.contains_key(name));
            }
        }
        Ok(self
            .responses
            .pop_front()
            .expect("unexpected synthetic request"))
    }
}
fn response(status: u16, body: &str) -> Response {
    Response {
        status,
        body: body.as_bytes().to_vec(),
        rate: RateLimit::default(),
    }
}
fn verify_with(root: &Path, status: u16, credentials: Credentials) -> Result<SavedLogin> {
    verify_candidate(credentials, |credentials| {
        let login = PlatformLogin {
            auth_token: Zeroizing::new(credentials.auth_token.clone()),
            ct0: Zeroizing::new(credentials.ct0.clone()),
            account: sample().account,
            proxy: String::new(),
        };
        let transport = FakeTransport {
            responses: vec![
                response(200, include_str!("../platform/fixtures/public-page.html")),
                response(
                    200,
                    &format!(
                        "{} \"AAAAA{}\"",
                        include_str!("../platform/fixtures/public-script.js"),
                        "0".repeat(45)
                    ),
                ),
                response(
                    status,
                    if status == 200 {
                        include_str!("../platform/fixtures/viewer.json")
                    } else {
                        "synthetic_auth_not_real synthetic_csrf_not_real private upstream body"
                    },
                ),
            ]
            .into(),
        };
        platform::verify_login_with(
            root,
            &mut platform::Session::default(),
            &login,
            transport,
            Duration::ZERO,
        )
        .map_err(platform_error)
    })
}
fn verify(root: &Path, status: u16) -> Result<SavedLogin> {
    verify_with(root, status, sample().credentials)
}
fn browser_verify(pending: &mut PendingLogin, status: u16) -> Result<SavedLogin> {
    let credentials = pending.browser.as_mut().unwrap().read(&pending.profile)?;
    verify_with(&pending.root, status, credentials)
}

#[test]
fn storage_cookies_accept_only_x_and_clear_values() {
    let mut value = cookies();
    value["cookies"].as_array_mut().unwrap().extend([
        json!({"name": "auth_token", "value": "foreign", "domain": "evil.x.com", "path": "/", "secure": true}),
        json!({"name": "auth_token", "value": "foreign", "domain": "twitter.com", "path": "/", "secure": true}),
        json!({"name": "ct0", "value": "foreign", "domain": "x.com.evil", "path": "/", "secure": true}),
    ]);
    assert_eq!(
        capture_cookies(&mut value).unwrap().auth_token,
        "synthetic_auth_not_real"
    );
    for cookie in value["cookies"].as_array().unwrap() {
        assert_eq!(cookie["value"], "");
    }
    let mut host_only = cookies();
    host_only["cookies"][0]["domain"] = json!("x.com");
    assert!(capture_cookies(&mut host_only).is_ok());
}
#[test]
fn storage_cookies_reject_missing_conflicting_expired_insecure_and_invalid_values() {
    for mut malformed in [
        json!({}),
        json!(null),
        json!("synthetic invalid body"),
        json!([]),
        json!(123),
        json!({"cookies": "invalid"}),
    ] {
        assert!(matches!(
            capture_cookies(&mut malformed),
            Err(AuthError::BrowserConnection)
        ));
    }
    for value in [
        json!({"cookies": []}),
        json!({"cookies": [cookie("ct0", "csrf")]}),
        json!({"cookies": [cookie("auth_token", "token")]}),
    ] {
        assert!(matches!(
            capture_cookies(&mut value.clone()),
            Err(AuthError::NotReady)
        ));
    }
    for (field, bad) in [
        ("value", json!("bad;header")),
        ("value", json!("")),
        ("value", Value::Null),
        ("secure", json!(false)),
        ("expires", json!(1)),
        ("path", json!("/i/")),
        ("domain", json!("twitter.com")),
    ] {
        let mut value = cookies();
        value["cookies"][0][field] = bad;
        assert!(matches!(
            capture_cookies(&mut value),
            Err(AuthError::NotReady)
        ));
    }
    let mut value = cookies();
    value["cookies"]
        .as_array_mut()
        .unwrap()
        .push(cookie("auth_token", "other"));
    assert!(matches!(
        capture_cookies(&mut value),
        Err(AuthError::NotReady)
    ));
}
#[test]
fn launch_args_are_minimal_and_capability_is_main_only() {
    let profile = Path::new("C:/isolated folder/xarchive-login-synthetic");
    let args = browser::login_args(profile, "");
    assert_eq!(
        args,
        vec![
            OsString::from("--user-data-dir=C:/isolated folder/xarchive-login-synthetic"),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-sync".into(),
            "--disable-background-networking".into(),
            "--disable-component-update".into(),
            "--disable-default-apps".into(),
            "https://x.com/login".into()
        ]
    );
    for proxy in ["", "http://proxy.example:8080"] {
        for arg in browser::login_args(profile, proxy) {
            let arg = arg.to_string_lossy();
            for forbidden in [
                "remote-debugging",
                "automation",
                "disable-blink-features",
                "headless",
            ] {
                assert!(!arg.contains(forbidden), "unexpected login argument: {arg}");
            }
        }
    }
    let args = browser::login_args(profile, "http://proxy.example:8080");
    assert_eq!(args.len(), 9);
    assert_eq!(args[7], "--proxy-server=http://proxy.example:8080");
    assert_eq!(
        browser::headless_args(profile),
        vec![
            OsString::from("--headless=new"),
            "--remote-debugging-port=0".into(),
            "--remote-debugging-address=127.0.0.1".into(),
            "--user-data-dir=C:/isolated folder/xarchive-login-synthetic".into(),
            "about:blank".into()
        ]
    );
    for proxy in [
        "http://user:pass@proxy.example:8080",
        "http://proxy.example:0",
        "http://proxy.example:8080/path",
        "http://proxy.example:8080 --remote-debugging-port=1",
    ] {
        assert!(validate_proxy(proxy).is_err());
    }
    let capabilities: Value =
        serde_json::from_str(include_str!("../../capabilities/default.json")).unwrap();
    assert_eq!(capabilities["windows"], json!(["main"]));
    assert!(capabilities.get("remote").is_none());
    assert_eq!(capabilities["permissions"], json!([]));
    let config: Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    assert_eq!(config["app"]["windows"].as_array().unwrap().len(), 1);
    assert_eq!(config["app"]["windows"][0]["label"], "main");
}
use std::ffi::OsString;
#[test]
fn devtools_port_is_only_a_loopback_browser_endpoint() {
    assert_eq!(
        cdp::active_port("12345\n/devtools/browser/synthetic-id\n").unwrap(),
        (12345, "/devtools/browser/synthetic-id".into())
    );
    for raw in [
        "0\n/devtools/browser/a",
        "65536\n/devtools/browser/a",
        "1234\n/devtools/page/a",
        "1234\nws://evil/a",
        "1234\n/devtools/browser/../a",
        "1234\n/devtools/browser/a?token=secret",
        "1234\n/devtools/browser/a\nextra",
        "1234",
    ] {
        assert!(cdp::active_port(raw).is_err());
    }
}
#[test]
fn occupancy_combines_process_and_profile_signals_and_refuses_unknown_state() {
    assert!(browser::occupied(Ok(true), || panic!("no need to probe an active process")).unwrap());
    assert!(browser::occupied(Ok(false), || Ok(true)).unwrap());
    assert!(!browser::occupied(Ok(false), || Ok(false)).unwrap());
    assert!(browser::occupied(Err(AuthError::BrowserConnection), || Ok(false)).is_err());
    assert!(browser::occupied(Ok(false), || Err(AuthError::BrowserConnection)).is_err());
    let directory = fixture();
    let profile = pending(directory.path());
    fs::write(profile.profile.join("lockfile"), "stale").unwrap();
    assert!(!browser::locks_in_use(&profile.profile).unwrap());
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(profile.profile.join("lockfile"))
            .unwrap();
        assert!(browser::locks_in_use(&profile.profile).unwrap());
        drop(held);
        assert!(!browser::locks_in_use(&profile.profile).unwrap());
    }
}
#[test]
fn busy_browser_prevents_read_save_and_forced_cleanup() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let busy = Arc::new(AtomicBool::new(true));
    let reads = Arc::new(AtomicUsize::new(0));
    manager.pending.as_mut().unwrap().browser = Some(Box::new(FakeBrowser {
        busy: busy.clone(),
        reads: reads.clone(),
        fail: false,
    }));
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    assert!(matches!(
        manager.confirm_with(directory.path(), |p| browser_verify(p, 200)),
        Err(AuthError::BrowserInUse)
    ));
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    assert!(!directory.path().join("private").join(AUTH_FILE).exists());
    let status = manager.cancel(directory.path()).unwrap();
    assert!(!status.pending && status.cleanup_warning.is_none() && profile.exists());
    busy.store(false, Ordering::Relaxed);
    retry_cleanup(directory.path(), None, |path| fs::remove_dir_all(path)).unwrap();
    assert!(profile.exists());
}
struct FakeReader {
    response: Option<Result<Value>>,
    close_error: bool,
    closed: bool,
}
impl browser::CookieReader for FakeReader {
    fn cookies(&mut self) -> Result<Value> {
        assert!(!self.closed);
        self.response.take().unwrap()
    }
    fn close(&mut self) -> Result<()> {
        self.closed = true;
        if self.close_error {
            Err(AuthError::BrowserConnection)
        } else {
            Ok(())
        }
    }
}
#[test]
fn reader_always_closes_before_success_or_failure_is_returned() {
    for response in [
        Ok(cookies()),
        Ok(json!({"cookies": []})),
        Ok(json!({})),
        Err(AuthError::BrowserConnection),
    ] {
        let expected_success = response.as_ref().is_ok_and(|v| {
            v.get("cookies")
                .and_then(Value::as_array)
                .is_some_and(|v| v.len() == 2)
        });
        let mut reader = FakeReader {
            response: Some(response),
            close_error: false,
            closed: false,
        };
        assert_eq!(
            browser::read_and_close(&mut reader).is_ok(),
            expected_success
        );
        assert!(reader.closed);
    }
    let mut reader = FakeReader {
        response: Some(Ok(cookies())),
        close_error: true,
        closed: false,
    };
    assert!(browser::read_and_close(&mut reader).is_err());
    assert!(reader.closed);
}
#[test]
fn headless_read_failure_preserves_saved_bytes_and_profile_for_retry() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    let file = directory.path().join("private").join(AUTH_FILE);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "synthetic previous encrypted state").unwrap();
    let original = fs::read(&file).unwrap();
    manager.pending.as_mut().unwrap().browser = Some(Box::new(FakeBrowser {
        busy: Arc::new(AtomicBool::new(false)),
        reads: Arc::new(AtomicUsize::new(0)),
        fail: true,
    }));
    assert!(matches!(
        manager.confirm_with(directory.path(), |p| browser_verify(p, 200)),
        Err(AuthError::BrowserConnection)
    ));
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(profile.exists() && manager.is_pending());
    manager.release();
    assert!(profile.exists());
}
#[test]
fn verification_failures_preserve_previous_file_and_profile_without_dpapi_or_network() {
    let directory = fixture();
    let file = directory.path().join("private").join(AUTH_FILE);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"synthetic previous encrypted state").unwrap();
    let original = fs::read(&file).unwrap();
    let mut manager = manager(directory.path());
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    for (status, expected) in [
        (401, "Authentication"),
        (403, "Authentication"),
        (429, "RateLimit"),
        (404, "Interface"),
        (503, "Network"),
    ] {
        let error = manager
            .confirm_with(directory.path(), |p| browser_verify(p, status))
            .err()
            .unwrap();
        let category = match &error {
            AuthError::Network(_) => "Network".to_owned(),
            _ => format!("{error:?}"),
        };
        assert_eq!(category, expected);
        assert!(manager.is_pending() && profile.exists());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!error.to_string().contains("synthetic"));
    }
    manager.release();
}
#[test]
fn cancel_keeps_dedicated_profile_and_closing_browser_keeps_pending() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    fs::write(profile.join("synthetic-cookie-file"), "sample").unwrap();
    let other = directory.path().join("temporary/other-job");
    fs::create_dir_all(&other).unwrap();
    let status = manager.status(directory.path()).unwrap();
    assert!(status.pending);
    let status = manager.cancel(directory.path()).unwrap();
    assert!(!status.saved && !status.pending && profile.exists() && other.exists());
}
#[test]
fn failed_cleanup_is_retained_and_retried_without_touching_unrelated_or_active_data() {
    let directory = fixture();
    let profile = directory.path().join("temporary/xarchive-login-abandoned");
    let active = directory.path().join("temporary/xarchive-login-active");
    let other = directory.path().join("temporary/unrelated");
    for path in [&profile, &active, &other] {
        fs::create_dir_all(path).unwrap();
    }
    assert!(
        retry_cleanup(directory.path(), Some(&active), |_| Err(
            std::io::ErrorKind::PermissionDenied.into()
        ))
        .is_err()
    );
    assert!(profile.exists() && active.exists() && other.exists());
    retry_cleanup(directory.path(), Some(&active), |path| {
        fs::remove_dir_all(path)
    })
    .unwrap();
    assert!(!profile.exists() && active.exists() && other.exists());
    for path in [
        &other,
        &directory.path().join("private"),
        &directory
            .path()
            .join("temporary/xarchive-login-/../../private"),
    ] {
        assert!(safe_legacy_profile(directory.path(), path).is_err());
    }
}
#[test]
fn manual_credentials_validate_before_verification_and_do_not_echo() {
    let directory = fixture();
    let mut manager = LoginManager::default();
    for raw in [
        "",
        "bad;header",
        "token\n",
        " token",
        "with space",
        "凭据",
        &"a".repeat(4097),
    ] {
        for (token, csrf) in [(raw, "valid"), ("valid", raw)] {
            let error = manager
                .manual_with(
                    directory.path(),
                    "",
                    Zeroizing::new(token.into()),
                    Zeroizing::new(csrf.into()),
                    |_| panic!("invalid input must not reach verification"),
                )
                .err()
                .unwrap();
            assert!(matches!(error, AuthError::CredentialsFormat));
            assert!(!format!("{error:?} {error}").contains("bad;header"));
        }
    }
    assert!(
        manual_credentials(
            Zeroizing::new("ABCdef_012-%".into()),
            Zeroizing::new("csrf_123".into())
        )
        .is_ok()
    );
    let status = serde_json::to_string(&manager.status(directory.path()).unwrap()).unwrap();
    assert!(!status.contains("auth_token") && !status.contains("ct0"));
    assert!(!directory.path().join("private").join(AUTH_FILE).exists());
}
#[test]
fn manual_verification_failure_preserves_old_state_and_rejects_during_browser_login() {
    let directory = fixture();
    let file = directory.path().join("private").join(AUTH_FILE);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"synthetic previous encrypted state").unwrap();
    let original = fs::read(&file).unwrap();
    let mut manager = LoginManager::default();
    let error = manager
        .manual_with(
            directory.path(),
            "",
            Zeroizing::new("synthetic_auth_not_real".into()),
            Zeroizing::new("synthetic_csrf_not_real".into()),
            |credentials| verify_with(directory.path(), 401, credentials),
        )
        .err()
        .unwrap();
    assert!(matches!(error, AuthError::Authentication));
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(!error.to_string().contains("synthetic"));
    manager.pending = Some(pending(directory.path()));
    assert!(matches!(
        manager.manual_with(
            directory.path(),
            "",
            Zeroizing::new("valid".into()),
            Zeroizing::new("csrf".into()),
            |_| panic!("pending browser login")
        ),
        Err(AuthError::Busy)
    ));
}
#[test]
fn diagnostics_never_include_synthetic_credentials_or_upstream_body() {
    let directory = fixture();
    for status in [401, 403, 404, 429, 503] {
        let error = verify(directory.path(), status).err().unwrap();
        for credential in [
            "synthetic_auth_not_real",
            "synthetic_csrf_not_real",
            "AAAAA",
            "private upstream body",
        ] {
            assert!(!format!("{error:?} {error}").contains(credential));
        }
    }
    let malformed =
        json!({"cookies": [cookie("auth_token", "secret;invalid"), cookie("ct0", "csrf")]});
    let error = capture_cookies(&mut malformed.clone()).err().unwrap();
    assert!(!error.to_string().contains("secret"));
}

#[test]
#[cfg(windows)]
fn encrypted_roundtrip_and_corrupt_state() {
    let directory = fixture();
    let value = sample();
    save(directory.path(), &value).unwrap();
    let file = directory.path().join("private").join(AUTH_FILE);
    let mut bytes = fs::read(&file).unwrap();
    assert!(
        !bytes
            .windows(value.credentials.auth_token.len())
            .any(|part| part == value.credentials.auth_token.as_bytes())
    );
    let loaded = load(directory.path()).unwrap().unwrap();
    assert_eq!(loaded.account, value.account);
    assert_eq!(loaded.credentials.ct0, value.credentials.ct0);
    let status =
        serde_json::to_string(&LoginManager::default().status(directory.path()).unwrap()).unwrap();
    assert!(
        !status.contains(&value.credentials.auth_token) && !status.contains(&value.credentials.ct0)
    );
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::write(&file, bytes).unwrap();
    assert!(matches!(load(directory.path()), Err(AuthError::Protection)));
    let mut newer = sample();
    newer.version = 999;
    save(directory.path(), &newer).unwrap();
    assert!(matches!(
        load(directory.path()),
        Err(AuthError::InvalidState)
    ));
}

#[test]
#[cfg(windows)]
fn failed_confirmation_preserves_saved_state_and_can_cancel() {
    let directory = fixture();
    save(directory.path(), &sample()).unwrap();
    let file = directory.path().join("private").join(AUTH_FILE);
    let original = fs::read(&file).unwrap();
    let mut manager = manager(directory.path());
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    assert!(matches!(
        manager.confirm_with(directory.path(), |p| browser_verify(p, 401)),
        Err(AuthError::Authentication)
    ));
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(profile.exists());
    let status = manager.cancel(directory.path()).unwrap();
    assert!(status.saved && !status.pending && profile.exists());
}
#[test]
#[cfg(windows)]
fn verified_session_saves_only_after_check_and_detects_switch() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let saved = verify(directory.path(), 200).unwrap();
    assert!(!directory.path().join("private").join(AUTH_FILE).exists());
    let status = manager
        .confirm_with(directory.path(), |_| Ok(saved))
        .unwrap();
    assert!(status.saved && !status.pending);
    let original = fs::read(directory.path().join("private").join(AUTH_FILE)).unwrap();
    manager.pending = Some(pending(directory.path()));
    let other = fixture();
    assert!(matches!(
        manager.confirm_with(other.path(), |_| panic!("different data root")),
        Err(AuthError::Busy)
    ));
    assert_eq!(
        fs::read(directory.path().join("private").join(AUTH_FILE)).unwrap(),
        original
    );
    manager.cancel(directory.path()).unwrap();
    let status = manager
        .manual_with(
            directory.path(),
            "",
            Zeroizing::new("synthetic_auth_not_real".into()),
            Zeroizing::new("synthetic_csrf_not_real".into()),
            |credentials| verify_with(directory.path(), 200, credentials),
        )
        .unwrap();
    let status = serde_json::to_string(&status).unwrap();
    assert!(
        !status.contains("synthetic_auth_not_real")
            && !status.contains("synthetic_csrf_not_real")
            && !status.contains("auth_token")
            && !status.contains("ct0")
    );
}

#[test]
fn login_network_diagnostics_preserve_cause_and_safe_proxy_source() {
    let mut error = platform::PlatformError::new(platform::Category::ProxyNetwork);
    error.message = "TLS 连接失败，请检查证书和代理的 HTTPS 连接";
    error.network_route = Some("本次使用：系统代理（proxy.example:8080）".into());
    let error = platform_error(error);
    assert!(matches!(&error, AuthError::Network(_)));
    let message = error.to_string();
    assert!(message.contains("TLS 连接失败"));
    assert!(message.contains("系统代理（proxy.example:8080）"));
    assert!(message.contains("此次状态尚未保存"));
}

#[test]
fn browser_reopened_during_verification_prevents_commit() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let busy = Arc::new(AtomicBool::new(false));
    manager.pending.as_mut().unwrap().browser = Some(Box::new(FakeBrowser {
        busy: busy.clone(),
        reads: Arc::new(AtomicUsize::new(0)),
        fail: false,
    }));
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    let error = manager
        .confirm_with(directory.path(), |p| {
            let saved = browser_verify(p, 200)?;
            busy.store(true, Ordering::Relaxed);
            Ok(saved)
        })
        .err()
        .unwrap();
    assert!(matches!(error, AuthError::BrowserInUse));
    assert!(manager.is_pending() && profile.exists());
    assert!(!directory.path().join("private").join(AUTH_FILE).exists());
    busy.store(false, Ordering::Relaxed);
    manager.cancel(directory.path()).unwrap();
}
#[test]
fn storage_cdp_envelope_does_not_expose_errors_or_copy_unrelated_replies() {
    let mut result = cdp::response(json!({"id": 1, "result": cookies()}), 1)
        .unwrap()
        .unwrap();
    assert_eq!(
        capture_cookies(&mut result).unwrap().ct0,
        "synthetic_csrf_not_real"
    );
    assert!(
        cdp::response(json!({"method":"event", "params": {}}), 1)
            .unwrap()
            .is_none()
    );
    assert!(
        cdp::response(json!({"id":2,"result":cookies()}), 1)
            .unwrap()
            .is_none()
    );
    for value in [
        json!({"id":1}),
        json!({"id":1,"error":{"message":"synthetic_auth_not_real synthetic_csrf_not_real"}}),
    ] {
        let error = cdp::response(value, 1).err().unwrap();
        assert!(!format!("{error:?} {error}").contains("synthetic"));
    }
}
struct FakeReaderProcess {
    exits: VecDeque<Result<bool>>,
    events: Vec<&'static str>,
}
impl browser::ReaderProcess for FakeReaderProcess {
    fn request_close(&mut self) {
        self.events.push("Browser.close");
    }
    fn wait(&mut self, _: Duration) -> Result<bool> {
        self.events.push("wait");
        self.exits.pop_front().unwrap()
    }
    fn terminate(&mut self) -> Result<()> {
        self.events.push("kill-reader");
        Ok(())
    }
}
#[test]
fn headless_shutdown_tries_browser_close_then_kills_only_on_timeout() {
    let mut process = FakeReaderProcess {
        exits: vec![Ok(true)].into(),
        events: vec![],
    };
    browser::stop_reader(&mut process).unwrap();
    assert_eq!(process.events, ["Browser.close", "wait"]);
    let mut process = FakeReaderProcess {
        exits: vec![Ok(false), Ok(true)].into(),
        events: vec![],
    };
    browser::stop_reader(&mut process).unwrap();
    assert_eq!(
        process.events,
        ["Browser.close", "wait", "kill-reader", "wait"]
    );
    let mut process = FakeReaderProcess {
        exits: vec![Ok(false), Ok(false)].into(),
        events: vec![],
    };
    assert!(browser::stop_reader(&mut process).is_err());
    assert_eq!(
        process.events,
        ["Browser.close", "wait", "kill-reader", "wait"]
    );
}

#[test]
fn dedicated_profile_is_fixed_reused_and_retained_after_release() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    assert_eq!(profile, directory.path().join(LOGIN_PROFILE));
    fs::write(profile.join("cache-sample"), b"synthetic cache").unwrap();
    manager.release(); // Successful confirmation and cancellation share this release path.
    assert!(profile.exists());
    let next = pending(directory.path());
    assert_eq!(next.profile, profile);
    drop(next);
    assert_eq!(
        fs::read(profile.join("cache-sample")).unwrap(),
        b"synthetic cache"
    );
    retry_cleanup(directory.path(), None, |_| {
        panic!("private profile must not be scanned")
    })
    .unwrap();
    for path in [
        directory.path().join("private"),
        directory.path().join("private/login-browser-other"),
        directory
            .path()
            .join("private/login-browser/../login.dpapi"),
        directory.path().join("temporary/xarchive-login-other"),
        directory.path().join("other/login-browser"),
    ] {
        assert!(safe_profile(directory.path(), &path).is_err());
    }
}

#[test]
fn clear_profile_refuses_busy_and_unknown_then_removes_only_dedicated_data() {
    let directory = fixture();
    let mut manager = LoginManager::default();
    let profile = PendingLogin::prepare(directory.path(), "").unwrap().profile;
    fs::write(profile.join("cache-sample"), b"synthetic cache").unwrap();
    let untouched = [
        directory.path().join("private").join(AUTH_FILE),
        directory.path().join("private/other-state"),
        directory.path().join("temporary/other-job/keep"),
        directory.path().join("media/keep"),
        directory.path().join("settings.json"),
    ];
    for path in &untouched {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"synthetic retained bytes").unwrap();
    }
    assert!(matches!(
        manager.clear_browser_profile_with(directory.path(), |_| Ok(true)),
        Err(AuthError::BrowserInUse)
    ));
    assert!(
        manager
            .clear_browser_profile_with(directory.path(), |_| Err(AuthError::BrowserConnection))
            .is_err()
    );
    assert!(profile.join("cache-sample").exists());
    manager
        .clear_browser_profile_with(directory.path(), |path| {
            assert_eq!(path, profile);
            Ok(false)
        })
        .unwrap();
    assert!(!profile.exists());
    for path in &untouched {
        assert_eq!(fs::read(path).unwrap(), b"synthetic retained bytes");
    }
    manager
        .clear_browser_profile_with(directory.path(), |_| {
            panic!("absent profile is already clear")
        })
        .unwrap();
}

#[test]
fn clear_pending_profile_checks_its_browser_and_releases_only_after_deletion() {
    let directory = fixture();
    let mut manager = manager(directory.path());
    let busy = Arc::new(AtomicBool::new(true));
    manager.pending.as_mut().unwrap().browser = Some(Box::new(FakeBrowser {
        busy: busy.clone(),
        reads: Arc::new(AtomicUsize::new(0)),
        fail: false,
    }));
    let profile = manager.pending.as_ref().unwrap().profile.clone();
    assert!(matches!(
        manager.clear_browser_profile_with(directory.path(), |_| panic!("use tracked browser")),
        Err(AuthError::BrowserInUse)
    ));
    assert!(manager.is_pending() && profile.exists());
    let other = fixture();
    assert!(matches!(
        manager.clear_browser_profile_with(other.path(), |_| panic!("wrong root")),
        Err(AuthError::Busy)
    ));
    busy.store(false, Ordering::Relaxed);
    manager
        .clear_browser_profile_with(directory.path(), |_| panic!("use tracked browser"))
        .unwrap();
    assert!(!manager.is_pending() && !profile.exists());
}

#[test]
#[cfg(windows)]
fn dedicated_profile_rejects_junctions_before_prepare_or_clear() {
    use std::os::windows::process::CommandExt;
    let directory = fixture();
    let target = directory.path().join("untouched");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep"), b"synthetic retained bytes").unwrap();
    let profile = directory.path().join(LOGIN_PROFILE);
    fs::create_dir_all(profile.parent().unwrap()).unwrap();
    let junction = |link: &Path| {
        let output = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(link.to_str().unwrap().replace('/', "\\"))
            .arg(target.to_str().unwrap().replace('/', "\\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction fixture failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    junction(&profile);
    assert!(PendingLogin::prepare(directory.path(), "").is_err());
    let mut manager = LoginManager::default();
    assert!(
        manager
            .clear_browser_profile_with(directory.path(), |_| panic!("unsafe path"))
            .is_err()
    );
    fs::remove_dir(&profile).unwrap(); // Remove only the test junction, never its target.
    fs::create_dir(&profile).unwrap();
    let nested = profile.join("nested");
    junction(&nested);
    assert!(safe_profile(directory.path(), &profile).is_err());
    assert!(
        manager
            .clear_browser_profile_with(directory.path(), |_| panic!("unsafe tree"))
            .is_err()
    );
    fs::remove_dir(&nested).unwrap();
    assert_eq!(
        fs::read(target.join("keep")).unwrap(),
        b"synthetic retained bytes"
    );
}

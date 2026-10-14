use super::*;

fn values(server: &str, bypass: &str) -> SystemValues {
    SystemValues {
        enabled: true,
        server: server.into(),
        bypass: bypass.into(),
        pac: String::new(),
    }
}
fn url(raw: &str) -> Url {
    Url::parse(raw).unwrap()
}
fn endpoint(plan: &ProxyPlan, raw: &str) -> Option<String> {
    plan.endpoint(&url(raw)).map(ToString::to_string)
}

#[test]
fn single_address_and_protocol_mapping() {
    let plan = ProxyPlan::select("", &values(" proxy.example:8080 ", "")).unwrap();
    for destination in ["http://x.com/", "https://x.com/"] {
        assert_eq!(
            endpoint(&plan, destination).as_deref(),
            Some("http://proxy.example:8080/")
        );
        assert_eq!(
            plan.route(&url(destination)),
            "本次使用：系统代理（proxy.example:8080）"
        );
    }
    let plan = ProxyPlan::select(
        "",
        &values(
            " http=plain.example:8080 ; HTTPS=secure.example:8443;ftp=unused.example:21 ",
            "",
        ),
    )
    .unwrap();
    assert_eq!(
        endpoint(&plan, "http://x.com/").as_deref(),
        Some("http://plain.example:8080/")
    );
    // HTTPS means destination scheme, not the scheme used to reach the proxy.
    assert_eq!(
        endpoint(&plan, "https://x.com/").as_deref(),
        Some("http://secure.example:8443/")
    );
    assert!(
        plan.route(&url("https://x.com/"))
            .contains("secure.example:8443")
    );
    let plan = ProxyPlan::select("", &values("http=plain.example:8080", "")).unwrap();
    assert_eq!(endpoint(&plan, "https://x.com/"), None);
    assert_eq!(plan.route(&url("https://x.com/")), "本次使用：直连");
}

#[test]
fn socks_mapping_and_fallback_keep_protocol_and_port() {
    for (raw, expected) in [
        ("socks=proxy.example:1081", "socks4://proxy.example:1081"),
        (
            "socks=socks5://proxy.example:1082",
            "socks5://proxy.example:1082",
        ),
        ("socks5h://proxy.example", "socks5h://proxy.example:1080"),
    ] {
        let plan = ProxyPlan::select("", &values(raw, "")).unwrap();
        for destination in ["http://x.com/", "https://x.com/"] {
            assert_eq!(endpoint(&plan, destination).as_deref(), Some(expected));
        }
    }
    let plan = ProxyPlan::select(
        "",
        &values(
            "https=proxy.example:8080;socks=socks5h://proxy.example:1080",
            "",
        ),
    )
    .unwrap();
    assert_eq!(
        endpoint(&plan, "https://x.com/").as_deref(),
        Some("http://proxy.example:8080/")
    );
    assert_eq!(
        endpoint(&plan, "http://x.com/").as_deref(),
        Some("socks5h://proxy.example:1080")
    );
}

#[test]
fn bypass_list_uses_windows_wildcards_local_names_and_ports() {
    let plan = ProxyPlan::select("", &values("proxy.example:8080",
        " <local> ; *.EXAMPLE.test ; 127.* ; [::1] ; exact.test ; port.test:443 ; https://scheme.test "))
        .unwrap();
    for destination in [
        "http://printer/",
        "https://a.example.test/",
        "https://127.0.0.2/",
        "http://[::1]/",
        "https://exact.test/",
        "https://port.test/",
        "https://scheme.test/",
    ] {
        assert_eq!(endpoint(&plan, destination), None, "{destination}");
        assert_eq!(
            plan.route(&url(destination)),
            "本次使用：直连（系统代理绕过列表）"
        );
    }
    for destination in [
        "https://x.com/",
        "https://example.test/",
        "https://sub.exact.test/",
        "https://port.test:444/",
        "http://scheme.test/",
    ] {
        assert!(endpoint(&plan, destination).is_some(), "{destination}");
    }
    let local = ProxyPlan::select("", &values("proxy.example:8080", "<local>")).unwrap();
    assert!(local.bypasses(&url("https://printer/")));
    for ip in ["https://127.0.0.1/", "https://[::1]/"] {
        assert!(!local.bypasses(&url(ip)));
    }
}

#[test]
fn source_selection_prefers_settings_and_disabled_system_is_direct() {
    let mut system = values("bad address", "*");
    system.pac = "https://secret.example/config.pac?private=secret".into();
    let plan = ProxyPlan::select("http://override.example:9000", &system).unwrap();
    assert_eq!(
        plan.route(&url("https://x.com/")),
        "本次使用：设置中的代理（override.example:9000）"
    );
    assert_eq!(
        endpoint(&plan, "https://x.com/").as_deref(),
        Some("http://override.example:9000/")
    );
    system.enabled = false;
    system.pac.clear();
    let plan = ProxyPlan::select("  ", &system).unwrap();
    assert_eq!(plan.route(&url("https://x.com/")), "本次使用：直连");
    assert_eq!(endpoint(&plan, "https://x.com/"), None);
    let plan = ProxyPlan::select("", &SystemValues::default()).unwrap();
    assert_eq!(endpoint(&plan, "https://x.com/"), None);
}

#[test]
fn pac_and_invalid_system_values_fail_without_exposing_raw_values() {
    for enabled in [true, false] {
        let mut system = values("proxy.example:8080", "");
        system.enabled = enabled;
        system.pac = "https://user:secret@private.example/config.pac?token=hidden".into();
        let error = ProxyPlan::select("", &system).err().unwrap();
        assert_eq!(error.message, "检测到自动配置脚本，请在设置中填写代理地址");
        let json = serde_json::to_string(&error).unwrap();
        for private in ["user", "secret", "private.example", "hidden"] {
            assert!(!json.contains(private));
        }
    }
    for server in [
        "",
        "https=bad address",
        "http://user:secret@proxy.example:0",
        "https://proxy.example/path",
        "https://proxy.example/?token=secret",
    ] {
        let error = ProxyPlan::select("", &values(server, "")).err().unwrap();
        assert_eq!(error.category, Category::ProxyNetwork);
        assert!(!serde_json::to_string(&error).unwrap().contains("secret"));
    }
}

#[test]
fn diagnostics_redact_userinfo_and_destination_query() {
    let plan = ProxyPlan::select(
        "",
        &values(
            "https=http://synthetic_user:synthetic_password@proxy.example:8080",
            "",
        ),
    )
    .unwrap();
    let destination = url("https://x.com/api?auth_token=synthetic_cookie");
    for kind in [
        Failure::Connect,
        Failure::Timeout,
        Failure::Tls,
        Failure::ProxyAuthentication,
        Failure::Other,
    ] {
        let error = plan.failure(&destination, kind);
        assert_eq!(
            error.network_route.as_deref(),
            Some("本次使用：系统代理（proxy.example:8080）")
        );
        let json = serde_json::to_string(&error).unwrap();
        for secret in [
            "synthetic_user",
            "synthetic_password",
            "synthetic_cookie",
            "auth_token",
            "/api",
        ] {
            assert!(!json.contains(secret));
        }
    }
    let ipv6 = ProxyPlan::select("", &values("[2001:db8::1]:8080", "")).unwrap();
    assert_eq!(
        ipv6.route(&destination),
        "本次使用：系统代理（[2001:db8::1]:8080）"
    );
}

#[derive(Debug)]
struct Wrapped(Box<dyn Error + Send + Sync>);
impl std::fmt::Display for Wrapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("synthetic wrapper")
    }
}
impl Error for Wrapped {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[test]
fn classifies_connection_timeout_tls_and_proxy_authentication() {
    assert_eq!(
        classify(
            &io::Error::from(io::ErrorKind::ConnectionRefused),
            false,
            false
        ),
        Failure::Connect
    );
    assert_eq!(
        classify(
            &io::Error::other(Wrapped(Box::new(io::Error::from(io::ErrorKind::TimedOut)))),
            true,
            false
        ),
        Failure::Timeout
    );
    assert_eq!(
        classify(
            &io::Error::other(rustls::Error::NoCertificatesPresented),
            true,
            false
        ),
        Failure::Tls
    );
    assert_eq!(
        classify(
            &Wrapped(Box::new(rustls::Error::NoCertificatesPresented)),
            true,
            false
        ),
        Failure::Tls
    );
    for text in [
        "tunnel error: proxy authorization required",
        "credentials not accepted",
        "server does not support user/pass authentication",
    ] {
        assert_eq!(
            classify(&Wrapped(Box::new(io::Error::other(text))), true, false),
            Failure::ProxyAuthentication
        );
    }
    assert_eq!(
        classify(&io::Error::other("synthetic upstream body"), false, true),
        Failure::Timeout
    );
    assert_eq!(
        classify(&io::Error::other("synthetic upstream body"), false, false),
        Failure::Other
    );
    let error = super::super::error::status_error(407, None).unwrap();
    assert_eq!(error.category, Category::ProxyNetwork);
    assert_eq!(error.http_status, Some(407));
    assert!(error.message.contains("代理认证失败"));
}

//! Deliberately small HTTP/1.1 origin/proxy; no test Transport or HTTP mocking crate.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use url::Url;

#[derive(Clone, Debug)]
pub struct Request {
    pub target: String,
    pub url: Url,
    pub headers: BTreeMap<String, String>,
}
impl Request {
    pub fn operation(&self) -> &str {
        self.url.path().rsplit('/').next().unwrap()
    }
    pub fn variables(&self) -> Value {
        serde_json::from_str(
            &self
                .url
                .query_pairs()
                .find(|(k, _)| k == "variables")
                .unwrap()
                .1,
        )
        .unwrap()
    }
    pub fn cursor(&self) -> Option<String> {
        self.variables()["cursor"].as_str().map(str::to_owned)
    }
}

#[derive(Default)]
pub struct State {
    pub requests: Vec<Request>,
    pub faults: bool,
    pub new_tweet: bool,
    pub rate_on_second: bool,
    pub rate_sent: bool,
    pub reset_at: u64,
}
pub struct Server {
    pub base: Url,
    pub state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (state2, stop2, base2) = (state.clone(), stop.clone(), base.clone());
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let request = read_request(&mut stream, &base2);
                let reply = {
                    let mut state = state2.lock().unwrap();
                    state.requests.push(request.clone());
                    respond(&request, &base2, &mut state)
                };
                // A failed download may close its socket before consuming an error body.
                let _ = write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                    reply.status,
                    reply.advertised.unwrap_or(reply.body.len()),
                    reply.headers
                );
                let _ = stream.write_all(&reply.body);
            }
        });
        Self {
            base,
            state,
            stop,
            thread: Some(thread),
        }
    }
    pub fn requests(&self) -> Vec<Request> {
        self.state.lock().unwrap().requests.clone()
    }
    pub fn clear_requests(&self) {
        self.state.lock().unwrap().requests.clear();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.base.socket_addrs(|| None).unwrap()[0]);
        let result = self.thread.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream, base: &Url) -> Request {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let parts: Vec<_> = line.split_whitespace().collect();
    assert_eq!(parts[0], "GET");
    let target = parts[1].to_owned();
    let url = if target.starts_with("http://") {
        Url::parse(&target).unwrap()
    } else {
        base.join(&target).unwrap()
    };
    let mut headers = BTreeMap::new();
    let mut length = line.len();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        length += line.len();
        assert!(length < 32 * 1024, "oversized test request");
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(key.to_ascii_lowercase(), value.trim().into());
    }
    Request {
        target,
        url,
        headers,
    }
}
struct Reply {
    status: &'static str,
    headers: String,
    body: Vec<u8>,
    advertised: Option<usize>,
}
fn body(bytes: Vec<u8>) -> Reply {
    Reply {
        status: "200 OK",
        headers: String::new(),
        body: bytes,
        advertised: None,
    }
}
fn json_body(value: Value) -> Reply {
    let mut reply = body(serde_json::to_vec(&value).unwrap());
    reply.headers = format!(
        "Content-Type: application/json\r\nx-rate-limit-remaining: 99\r\nx-rate-limit-reset: {}\r\n",
        crate::platform::client::now() + 900
    );
    reply
}
pub fn media_bytes(path: &str) -> Vec<u8> {
    // Synthetic image/video bytes: transport and storage do not decode media.
    format!("synthetic media contents for {path}\0\x01\x02").into_bytes()
}
fn user(id: &str, name: &str) -> Value {
    json!({"__typename":"User","rest_id":id,"legacy":{"screen_name":name,"name":format!("Synthetic {name}")}})
}
fn photo(base: &Url, key: &str) -> Value {
    json!({"type":"photo","media_key":key,"media_url_https":base.join(&format!("cdn/{key}.jpg?name=small")).unwrap().as_str()})
}
fn movie(base: &Url, gif: bool) -> Value {
    let key = if gif { "gif" } else { "video" };
    let variant = |name: &str, bitrate| json!({"content_type":"video/mp4","bitrate":bitrate,"url":base.join(&format!("cdn/{name}.mp4")).unwrap().as_str()});
    let variants = if gif {
        vec![variant("gif", 0)]
    } else {
        vec![
            json!({"content_type":"application/x-mpegURL","url":base.join("cdn/playlist.m3u8").unwrap().as_str()}),
            variant("low", 256000),
            variant("high", 2176000),
        ]
    };
    json!({"type":if gif {"animated_gif"} else {"video"},"media_key":key,"video_info":{"variants":variants}})
}
fn tweet(id: &str, day: u8, author: Value, media: Vec<Value>) -> Value {
    json!({"__typename":"Tweet","rest_id":id,"core":{"user_results":{"result":author}},"legacy":{"id_str":id,"full_text":format!("Synthetic, \"tweet\" {id}"),"created_at":format!("Wed Oct {day:02} 12:00:00 +0000 2026"),"extended_entities":{"media":media}}})
}
fn entry(tweet: Value) -> Value {
    json!({"content":{"itemContent":{"itemType":"TimelineTweet","tweet_results":{"result":tweet}}}})
}
fn page(tweets: Vec<Value>, next: Option<&str>, pinned: Option<Value>) -> Value {
    let mut entries: Vec<_> = tweets.into_iter().map(entry).collect();
    if let Some(cursor) = next {
        entries.push(json!({"content":{"cursorType":"Bottom","value":cursor}}));
    }
    let mut instructions = vec![json!({"type":"TimelineAddEntries","entries":entries})];
    if let Some(pinned) = pinned {
        instructions.push(json!({"type":"TimelinePinEntry","entry":entry(pinned)}));
    }
    json!({"data":{"user":{"result":{"timeline_v2":{"timeline":{"instructions":instructions}}}}}})
}
fn respond(request: &Request, base: &Url, state: &mut State) -> Reply {
    if request.url.path().starts_with("/cdn/") {
        for key in ["cookie", "authorization", "x-csrf-token"] {
            assert!(
                !request.headers.contains_key(key),
                "credentials reached CDN"
            );
        }
        let path = request.url.path();
        return match path {
            "/cdn/missing.jpg" => Reply {
                status: "404 Not Found",
                ..body(vec![])
            },
            "/cdn/redirect.jpg" => Reply {
                status: "302 Found",
                headers: format!("Location: {}\r\n", base.join("cdn/landing.jpg").unwrap()),
                ..body(vec![])
            },
            "/cdn/truncated.jpg"
                if state
                    .requests
                    .iter()
                    .filter(|r| r.url.path() == path)
                    .count()
                    == 1 =>
            {
                Reply {
                    advertised: Some(4096),
                    ..body(b"partial".to_vec())
                }
            }
            _ => body(media_bytes(path)),
        };
    }
    assert!(request.url.path().starts_with("/i/api/graphql/"));
    assert_eq!(
        request.headers["authorization"],
        "Bearer synthetic_public_bearer"
    );
    assert_eq!(
        request.headers["cookie"],
        "auth_token=synthetic_auth; ct0=synthetic_csrf"
    );
    assert_eq!(request.headers["x-csrf-token"], "synthetic_csrf");
    for key in ["features", "fieldToggles"] {
        let raw = request.url.query_pairs().find(|(k, _)| k == key).unwrap().1;
        assert!(serde_json::from_str::<Value>(&raw).unwrap().is_object());
    }
    match request.operation() {
        "Viewer" => json_body(
            json!({"data":{"viewer":{"user_results":{"result":user("90000", "viewer")}}}}),
        ),
        "UserByScreenName" => {
            assert_eq!(request.variables()["screen_name"], "fixture");
            json_body(json!({"data":{"user":{"result":user("1", "fixture")}}}))
        }
        op @ ("UserMedia" | "UserTweets") => {
            assert_eq!(request.variables()["userId"], "1");
            let cursor = request.cursor();
            if op == "UserMedia"
                && cursor.as_deref() == Some("m2")
                && state.rate_on_second
                && !state.rate_sent
            {
                state.rate_sent = true;
                state.reset_at = crate::platform::client::now() + 4;
                return Reply {
                    status: "429 Too Many Requests",
                    headers: format!(
                        "x-rate-limit-remaining: 0\r\nx-rate-limit-reset: {}\r\n",
                        state.reset_at
                    ),
                    ..body(vec![])
                };
            }
            let own = || user("1", "fixture");
            let pinned = || tweet("100", 1, own(), vec![photo(base, "pinned")]);
            let recent = || tweet("300", 13, own(), vec![photo(base, "image")]);
            let new = || tweet("400", 14, own(), vec![photo(base, "new")]);
            let value = match (op, cursor.as_deref()) {
                ("UserMedia", None) => {
                    let mut tweets = vec![
                        pinned(),
                        recent(),
                        tweet("290", 12, own(), vec![movie(base, false)]),
                    ];
                    if state.new_tweet {
                        tweets.insert(0, new());
                    }
                    // Duplicate pinned entry intentionally tests identity merging.
                    page(tweets, Some("m2"), Some(pinned()))
                }
                ("UserMedia", Some("m2")) => {
                    let mut retweet = tweet("280", 11, own(), vec![]);
                    retweet["legacy"]["retweeted_status_result"] = json!({"result":tweet("180",3,user("2","original"),vec![photo(base,"retweet")])});
                    let mut tweets =
                        vec![retweet, tweet("270", 10, own(), vec![movie(base, true)])];
                    if state.faults {
                        for (id, key) in [
                            ("260", "missing"),
                            ("250", "truncated"),
                            ("240", "redirect"),
                        ] {
                            tweets.push(tweet(id, 9, own(), vec![photo(base, key)]));
                        }
                    }
                    page(tweets, Some("tail-0"), None)
                }
                ("UserMedia", Some(cursor)) if cursor.starts_with("tail-") => {
                    let n: u32 = cursor.strip_prefix("tail-").unwrap().parse().unwrap();
                    assert!(n < 10, "archive did not bound the empty-page tail");
                    page(vec![], Some(&format!("tail-{}", n + 1)), None)
                }
                ("UserTweets", None) => {
                    let mut tweets = vec![pinned(), recent(), tweet("230", 6, own(), vec![])];
                    if state.new_tweet {
                        tweets.insert(0, new());
                    }
                    page(tweets, Some("t2"), Some(pinned()))
                }
                ("UserTweets", Some("t2")) => {
                    page(vec![tweet("220", 5, own(), vec![])], None, None)
                }
                _ => panic!("unexpected {op} cursor {cursor:?}"),
            };
            json_body(value)
        }
        op => panic!("unexpected operation {op}"),
    }
}

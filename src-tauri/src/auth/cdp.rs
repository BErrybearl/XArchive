use super::{AuthError, Result};
use serde_json::{Value, json};
use std::{
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};

const LIMIT: usize = 16 * 1024 * 1024;

fn stream(port: u16) -> Result<TcpStream> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|_| AuthError::BrowserConnection)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| AuthError::BrowserConnection)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| AuthError::BrowserConnection)?;
    Ok(stream)
}

pub(super) struct Cdp {
    socket: WebSocket<TcpStream>,
    id: u64,
}

impl Cdp {
    pub(super) fn connect(port: u16, path: &str) -> Result<Self> {
        let address = format!("ws://127.0.0.1:{port}{path}");
        endpoint_path(path)?;
        let socket = stream(port)?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(LIMIT))
            .max_frame_size(Some(LIMIT));
        let (socket, _) =
            tungstenite::client::client_with_config(address.as_str(), socket, Some(config))
                .map_err(|_| AuthError::BrowserConnection)?;
        Ok(Self { socket, id: 0 })
    }

    pub(super) fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.id += 1;
        self.socket
            .send(Message::Text(
                json!({"id": self.id, "method": method, "params": params})
                    .to_string()
                    .into(),
            ))
            .map_err(|_| AuthError::BrowserConnection)?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let value: Value =
                        serde_json::from_str(zeroize::Zeroizing::new(text.to_string()).as_str())
                            .map_err(|_| AuthError::BrowserConnection)?;
                    if let Some(result) = response(value, self.id)? {
                        return Ok(result);
                    }
                }
                Ok(Message::Ping(_)) => {
                    self.socket
                        .flush()
                        .map_err(|_| AuthError::BrowserConnection)?;
                }
                Ok(Message::Close(_)) => return Err(AuthError::BrowserConnection),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => return Err(AuthError::BrowserConnection),
            }
        }
        Err(AuthError::BrowserConnection)
    }

    pub(super) fn close_browser(&mut self) {
        let _ = self.socket.send(Message::Text(
            json!({"id": 9999, "method": "Browser.close"})
                .to_string()
                .into(),
        ));
    }
}

// DevToolsActivePort must identify the browser endpoint on our loopback port.
fn endpoint_path(path: &str) -> Result<()> {
    let id = path
        .strip_prefix("/devtools/browser/")
        .ok_or(AuthError::BrowserConnection)?;
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return Err(AuthError::BrowserConnection);
    }
    Ok(())
}
pub(super) fn active_port(raw: &str) -> Result<(u16, String)> {
    let mut lines = raw.lines();
    let port = lines
        .next()
        .ok_or(AuthError::BrowserConnection)?
        .parse::<u16>()
        .map_err(|_| AuthError::BrowserConnection)?;
    let path = lines.next().ok_or(AuthError::BrowserConnection)?;
    if port == 0 || lines.any(|line| !line.is_empty()) {
        return Err(AuthError::BrowserConnection);
    }
    endpoint_path(path)?;
    Ok((port, path.to_owned()))
}

// Consume the result without copying cookies; ignore unrelated CDP events and replies.
pub(super) fn response(mut value: Value, id: u64) -> Result<Option<Value>> {
    if value["id"].as_u64() != Some(id) {
        return Ok(None);
    }
    if value.get("error").is_some() {
        return Err(AuthError::BrowserConnection);
    }
    value
        .get_mut("result")
        .map(|result| Some(result.take()))
        .ok_or(AuthError::BrowserConnection)
}

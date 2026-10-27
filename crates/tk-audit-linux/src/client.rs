//! A small client for the daemon's socket (`contracts/daemon-protocol.md`),
//! shared by `tk run` (crates/tk-cli/src/run) and `tk-audit-feed`. It
//! speaks the wire format with `serde_json` values rather than lane B's Rust
//! types, so it builds on the contract alone; the drift test in
//! `tests/client_contract.rs` checks it against the recorded exchange and
//! lane B's `socket_path`.
//!
//! Every read and write has a timeout: a daemon that does not answer is an
//! error, never a hang (rules case 9).

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tk_core::event::Event;

/// The longest line the daemon sends or takes (contract section 2).
pub const MAX_LINE: usize = 1024 * 1024;
pub const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum ClientError {
    /// No socket, or nothing listening on it.
    NotRunning(String),
    Io(std::io::Error),
    /// The daemon answered `ok: false`.
    Refused(String),
    /// A line that is not the contract's shape.
    Protocol(String),
    /// The token file is missing, malformed or readable by others.
    Token(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning(s) => write!(f, "the daemon is not running ({s})"),
            ClientError::Io(e) => write!(f, "daemon socket: {e}"),
            ClientError::Refused(s) => write!(f, "the daemon refused: {s}"),
            ClientError::Protocol(s) => write!(f, "daemon protocol: {s}"),
            ClientError::Token(s) => write!(f, "daemon token: {s}"),
        }
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> ClientError {
        ClientError::Io(e)
    }
}

/// Where the daemon's socket lives (contract section 1). Kept equal to
/// `tk_daemon::socket_path` by `tests/client_contract.rs`.
pub fn socket_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/tk/daemon.sock"))
    } else {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".tk/run")))?;
        Some(dir.join("tk/daemon.sock"))
    }
}

/// Reads `daemon.token` beside the socket. Refused unless it is a regular
/// file owned by this user with no group or other bits, holding a
/// well-formed token: a token another user could have written is not ours.
pub fn read_daemon_token(dir: &Path) -> Result<String, ClientError> {
    let path = dir.join("daemon.token");
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| ClientError::Token(format!("{}: {e}", path.display())))?;
    if !meta.file_type().is_file() {
        return Err(ClientError::Token(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(ClientError::Token(format!(
            "{} is not owned by you",
            path.display()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(ClientError::Token(format!(
            "{} is readable by others (mode {:o})",
            path.display(),
            meta.mode() & 0o777
        )));
    }
    let mut text = String::new();
    std::fs::File::open(&path)?
        .take(256)
        .read_to_string(&mut text)?;
    let token = text.trim().to_string();
    if !tk_core::token::well_formed(&token) {
        return Err(ClientError::Token(format!(
            "{} does not hold a token",
            path.display()
        )));
    }
    Ok(token)
}

/// Reads `otlp.endpoint` beside the socket (contract section 4). Only a
/// loopback `http://` URL with a port is accepted; anything else is `None`,
/// so nothing is sent off the machine (`contracts/events.md` section 2).
pub fn read_otlp_endpoint(dir: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(dir.join("otlp.endpoint"))
        .ok()?
        .take(256)
        .read_to_string(&mut text)
        .ok()?;
    let url = text.trim();
    loopback_http(url).then(|| url.to_string())
}

/// `http://127.0.0.1:<port>`, `http://[::1]:<port>` or `http://localhost:<port>`,
/// with nothing after the port but an optional `/`.
pub fn loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = if let Some(r) = rest.strip_prefix("[::1]:") {
        ("::1", r)
    } else {
        match rest.rsplit_once(':') {
            Some((h, p)) => (h, p),
            None => return false,
        }
    };
    matches!(host, "127.0.0.1" | "::1" | "localhost")
        && !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|p| (1..=65535).contains(&p))
}

/// The scope a `hello` got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub scope: String,
    pub chat: Option<String>,
    pub daemon: Value,
}

pub struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    /// Connects and says `hello`.
    pub fn connect(
        socket: &Path,
        token: &str,
        client: &str,
    ) -> Result<(Client, Hello), ClientError> {
        Client::connect_timeout(socket, token, client, TIMEOUT)
    }

    /// `connect` with a read and write timeout of the caller's choosing.
    pub fn connect_timeout(
        socket: &Path,
        token: &str,
        client: &str,
        timeout: Duration,
    ) -> Result<(Client, Hello), ClientError> {
        let stream = UnixStream::connect(socket)
            .map_err(|e| ClientError::NotRunning(format!("{}: {e}", socket.display())))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut c = Client {
            writer: stream,
            reader,
        };
        let reply = c.request(&json!({"op": "hello", "token": token, "client": client}))?;
        let hello = Hello {
            scope: reply
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            chat: reply
                .get("chat")
                .and_then(Value::as_str)
                .map(str::to_string),
            daemon: reply.get("daemon").cloned().unwrap_or(Value::Null),
        };
        if hello.scope != "daemon" && hello.scope != "chat" {
            return Err(ClientError::Protocol(format!(
                "hello reply has no scope: {reply}"
            )));
        }
        Ok((c, hello))
    }

    /// Sends one request line and reads its one reply. `ok: false` is an
    /// error carrying the daemon's message.
    pub fn request(&mut self, line: &Value) -> Result<Value, ClientError> {
        let mut text =
            serde_json::to_string(line).map_err(|e| ClientError::Protocol(e.to_string()))?;
        if text.len() >= MAX_LINE {
            return Err(ClientError::Protocol("request line too long".into()));
        }
        text.push('\n');
        self.writer.write_all(text.as_bytes())?;
        let reply = self.read_line()?;
        match reply.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(reply),
            Some(false) => Err(ClientError::Refused(
                reply
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason")
                    .to_string(),
            )),
            None => Err(ClientError::Protocol(format!("reply without ok: {reply}"))),
        }
    }

    /// One line from the daemon, bounded.
    pub fn read_line(&mut self) -> Result<Value, ClientError> {
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the daemon closed the connection",
            )));
        }
        if buf.len() > MAX_LINE {
            return Err(ClientError::Protocol("line too long".into()));
        }
        serde_json::from_slice(&buf).map_err(|e| ClientError::Protocol(format!("{e}")))
    }

    /// `token.mint` (daemon scope): a fresh chat token.
    pub fn mint(&mut self, chat: &str, harness: &str) -> Result<String, ClientError> {
        let reply = self.request(&json!({"op": "token.mint", "chat": chat, "harness": harness}))?;
        let token = reply
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| ClientError::Protocol("token.mint reply has no token".into()))?;
        if !tk_core::token::well_formed(token) {
            return Err(ClientError::Protocol(
                "token.mint returned a malformed token".into(),
            ));
        }
        Ok(token.to_string())
    }

    /// `event`: stores one event, returns its id.
    pub fn post(&mut self, event: &Event) -> Result<String, ClientError> {
        let value =
            serde_json::to_value(event).map_err(|e| ClientError::Protocol(e.to_string()))?;
        let reply = self.request(&json!({"op": "event", "event": value}))?;
        Ok(reply
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&event.id)
            .to_string())
    }

    /// `tail`: after the reply, the connection streams `{"event": ...}`
    /// lines. The read timeout is lifted (a quiet daemon is not an error on
    /// a tail); the caller reads with `next_event`.
    pub fn tail(mut self, since: Option<i64>) -> Result<Tail, ClientError> {
        let mut req = json!({"op": "tail"});
        if let Some(s) = since {
            req["since"] = json!(s);
        }
        self.request(&req)?;
        self.writer.set_read_timeout(None)?;
        Ok(Tail { client: self })
    }
}

pub struct Tail {
    client: Client,
}

impl Tail {
    /// The next streamed event. Lines that are not `{"event": ...}` are
    /// skipped; a line whose event does not parse is an error.
    pub fn next_event(&mut self) -> Result<Event, ClientError> {
        loop {
            let line = self.client.read_line()?;
            if let Some(ev) = line.get("event") {
                return serde_json::from_value(ev.clone())
                    .map_err(|e| ClientError::Protocol(e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_endpoints() {
        for ok in [
            "http://127.0.0.1:4318",
            "http://[::1]:4318/",
            "http://localhost:1",
        ] {
            assert!(loopback_http(ok), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:4318",
            "http://127.0.0.1",
            "http://127.0.0.1:4318/v1/logs",
            "http://10.0.0.1:4318",
            "http://127.0.0.1.evil.com:80",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://user@127.0.0.1:80",
        ] {
            assert!(!loopback_http(bad), "{bad}");
        }
    }
}

/// A fake daemon for tests (this crate's and `tk run`'s): it answers the
/// contract's ops from a script and records every line it got. Not part of
/// the feed; `#[doc(hidden)]` so nothing else builds on it.
#[doc(hidden)]
pub mod fake {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    pub const DAEMON_TOKEN: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";
    pub const CHAT_TOKEN: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// What the fake does with each op.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mode {
        /// Answers as the contract says.
        Normal,
        /// Reads lines and never answers.
        Silent,
        /// Refuses `token.mint`.
        RefuseMint,
    }

    pub struct Fake {
        pub dir: PathBuf,
        pub socket: PathBuf,
        pub got: Arc<Mutex<Vec<Value>>>,
    }

    /// Starts a fake in a fresh 0700 folder under the OS temp folder, with
    /// `daemon.token` (0600) and `otlp.endpoint` beside the socket.
    pub fn start(name: &str, mode: Mode) -> Fake {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tk-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let tok = dir.join("daemon.token");
        std::fs::write(&tok, format!("{DAEMON_TOKEN}\n")).unwrap();
        std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(dir.join("otlp.endpoint"), "http://127.0.0.1:4318\n").unwrap();
        let socket = dir.join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g = got.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let g = g.clone();
                std::thread::spawn(move || serve(stream, mode, g));
            }
        });
        Fake { dir, socket, got }
    }

    fn serve(stream: std::os::unix::net::UnixStream, mode: Mode, got: Arc<Mutex<Vec<Value>>>) {
        let mut w = stream.try_clone().unwrap();
        let mut scope = None;
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                return;
            };
            got.lock().unwrap().push(v.clone());
            if mode == Mode::Silent {
                continue;
            }
            let op = v["op"].as_str().unwrap_or("");
            let reply = match (op, scope) {
                ("hello", None) => match v["token"].as_str() {
                    Some(DAEMON_TOKEN) => {
                        scope = Some("daemon");
                        json!({"ok": true, "scope": "daemon", "daemon": {"version": "0.0.0", "pid": 1, "protocol": 1}})
                    }
                    Some(CHAT_TOKEN) => {
                        scope = Some("chat");
                        json!({"ok": true, "scope": "chat", "chat": "chat-b"})
                    }
                    _ => {
                        let _ = writeln!(w, "{}", json!({"ok": false, "error": "bad token"}));
                        return;
                    }
                },
                (_, None) => {
                    let _ = writeln!(w, "{}", json!({"ok": false, "error": "hello first"}));
                    return;
                }
                ("token.mint", Some("daemon")) if mode == Mode::RefuseMint => {
                    json!({"ok": false, "error": "no"})
                }
                ("token.mint", Some("daemon")) => json!({"ok": true, "token": CHAT_TOKEN}),
                ("event", _) => json!({"ok": true, "id": v["event"]["id"]}),
                ("tail", Some("daemon")) => json!({"ok": true}),
                _ => json!({"ok": false, "error": format!("unexpected {op}")}),
            };
            if writeln!(w, "{reply}").is_err() {
                return;
            }
        }
    }

    impl Fake {
        pub fn lines(&self) -> Vec<Value> {
            self.got.lock().unwrap().clone()
        }

        pub fn path(&self) -> &Path {
            &self.socket
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

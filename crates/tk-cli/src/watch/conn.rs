//! One connection to the daemon's socket (`contracts/daemon-protocol.md`,
//! section 2). `tk run`'s client (`tk_audit_linux::client`) answers one
//! request at a time; `tail` and `approver` need a connection that stays
//! open with no read timeout and that tells replies from pushes, so `watch`
//! keeps its own, and shares the socket path and the token checks with
//! `tk run`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tk_audit_linux::client as shared;

/// The longest line either side sends (contract section 2).
pub const MAX_LINE: usize = shared::MAX_LINE;
/// The read and write timeout for a request and its one reply.
pub const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Conn {
    w: UnixStream,
    r: BufReader<UnixStream>,
    pub hello: Value,
}

/// Where the socket is: `--socket PATH` when given, else the contract's
/// path. No environment variable moves it: a `TK_DAEMON_SOCKET` an agent
/// sets would point `watch` at a daemon of its own.
pub fn socket(flag: Option<&str>) -> Result<PathBuf, String> {
    match flag {
        Some(p) if Path::new(p).is_absolute() => Ok(PathBuf::from(p)),
        Some(p) => Err(format!("--socket must be an absolute path, got {p:?}")),
        None => shared::socket_path().ok_or_else(|| "no HOME or XDG_RUNTIME_DIR".to_string()),
    }
}

impl Conn {
    /// Connects to `socket`, reads `daemon.token` beside it with `tk run`'s
    /// checks (a regular file you own, mode 0600), and says `hello`. The
    /// reply must give daemon scope: `watch` never acts with a chat token.
    pub fn open(socket: &Path, client: &str) -> Result<Conn, String> {
        let dir = socket
            .parent()
            .ok_or_else(|| format!("{} has no folder", socket.display()))?;
        let token = shared::read_daemon_token(dir).map_err(|e| e.to_string())?;
        Conn::open_with(socket, &token, client)
    }

    pub fn open_with(socket: &Path, token: &str, client: &str) -> Result<Conn, String> {
        let stream = UnixStream::connect(socket).map_err(|e| {
            format!(
                "the daemon is not running ({}: {e}); start it with `tk-daemon` or its user unit",
                socket.display()
            )
        })?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(TIMEOUT)))
            .map_err(|e| e.to_string())?;
        let r = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut c = Conn {
            w: stream,
            r,
            hello: Value::Null,
        };
        let reply = c.request(&json!({"op": "hello", "token": token, "client": client}))?;
        if reply.get("scope").and_then(Value::as_str) != Some("daemon") {
            return Err(format!(
                "the daemon did not give this connection daemon scope: {}",
                scope_text(&reply)
            ));
        }
        c.hello = reply;
        Ok(c)
    }

    /// Sends one line.
    pub fn send(&mut self, line: &Value) -> Result<(), String> {
        let mut text = serde_json::to_string(line).map_err(|e| e.to_string())?;
        if text.len() >= MAX_LINE {
            return Err("request line too long".into());
        }
        text.push('\n');
        self.w
            .write_all(text.as_bytes())
            .map_err(|e| format!("daemon socket: {e}"))
    }

    /// Sends one request and reads its reply. Only for requests with no
    /// push before the reply (every op but a streaming one already started).
    pub fn request(&mut self, line: &Value) -> Result<Value, String> {
        self.send(line)?;
        let reply = self.line()?;
        check_ok(reply)
    }

    /// One line, at most `MAX_LINE` bytes. A closed socket is an error.
    pub fn line(&mut self) -> Result<Value, String> {
        read_line(&mut self.r)
    }

    /// Lifts the read timeout: a quiet daemon is not an error on a stream.
    pub fn stream_mode(&mut self) -> Result<(), String> {
        self.w.set_read_timeout(None).map_err(|e| e.to_string())
    }

    /// A read timeout for draining a stream until the daemon is quiet.
    pub fn quiet_timeout(&mut self, d: Duration) -> Result<(), String> {
        self.w.set_read_timeout(Some(d)).map_err(|e| e.to_string())
    }

    /// Who serves the socket (`peer::of`).
    pub fn peer(&self) -> Option<super::peer::Peer> {
        super::peer::of(&self.w)
    }

    /// A second handle for a writer thread.
    pub fn writer(&self) -> Result<UnixStream, String> {
        self.w.try_clone().map_err(|e| e.to_string())
    }

    /// Splits off the reading half, for a reader thread.
    pub fn into_reader(self) -> BufReader<UnixStream> {
        self.r
    }
}

pub fn read_line(r: &mut impl BufRead) -> Result<Value, String> {
    let mut buf = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buf)
        .map_err(|e| format!("daemon socket: {e}"))?;
    if n == 0 {
        return Err("the daemon closed the connection".into());
    }
    if buf.len() > MAX_LINE {
        return Err("the daemon sent a line over 1 MiB".into());
    }
    serde_json::from_slice(&buf).map_err(|e| format!("daemon protocol: {e}"))
}

/// A reply line: `ok: true` passes, `ok: false` is the daemon's reason.
pub fn check_ok(reply: Value) -> Result<Value, String> {
    match reply.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(reply),
        Some(false) => Err(format!(
            "the daemon refused: {}",
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no reason given")
        )),
        None => Err(format!("daemon protocol: a reply without ok: {}", reply)),
    }
}

fn scope_text(reply: &Value) -> String {
    match reply.get("scope").and_then(Value::as_str) {
        Some(s) => format!("scope {s}"),
        None => "no scope".into(),
    }
}

/// What a line from a streaming connection is.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Reply(Value),
    Event(Value),
    Approve(Value),
    Other(Value),
}

pub fn classify(line: Value) -> Incoming {
    if line.get("ok").is_some() {
        Incoming::Reply(line)
    } else if let Some(e) = line.get("event") {
        Incoming::Event(e.clone())
    } else if let Some(a) = line.get("approve") {
        Incoming::Approve(a.clone())
    } else {
        Incoming::Other(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_socket_flag_is_refused() {
        assert!(socket(Some("run/daemon.sock")).is_err());
        assert_eq!(
            socket(Some("/tmp/x/daemon.sock")).unwrap(),
            PathBuf::from("/tmp/x/daemon.sock")
        );
    }

    #[test]
    fn lines_are_classified_by_their_key() {
        assert!(matches!(classify(json!({"ok": true})), Incoming::Reply(_)));
        assert!(matches!(
            classify(json!({"event": {"id": "a"}})),
            Incoming::Event(_)
        ));
        assert!(matches!(
            classify(json!({"approve": {"id": "a"}})),
            Incoming::Approve(_)
        ));
        assert!(matches!(classify(json!({"x": 1})), Incoming::Other(_)));
    }

    #[test]
    fn refused_replies_carry_the_reason() {
        let e = check_ok(json!({"ok": false, "error": "report needs daemon scope"})).unwrap_err();
        assert!(e.contains("report needs daemon scope"), "{e}");
        assert!(check_ok(json!({"id": 1})).is_err());
    }

    #[test]
    fn a_line_over_the_limit_is_an_error() {
        let mut big = vec![b'"'; MAX_LINE + 10];
        big.push(b'\n');
        let mut r = std::io::Cursor::new(big);
        assert!(read_line(&mut r).unwrap_err().contains("1 MiB"));
    }
}

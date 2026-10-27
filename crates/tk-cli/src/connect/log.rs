//! Every write is recorded: to the daemon's log when the daemon is running
//! (`contracts/daemon-protocol.md`), else as one JSON line in
//! `~/.tk/connect.log` (mode 0600). The record names the file, the server,
//! the hashes before and after, the backup and where the entry came from.
//! It never holds a value from the config, so no secret can reach it.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

const DAEMON_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub harness: String,
    pub path: String,
    pub server: String,
    /// `add`, `replace` or `remove`.
    pub action: String,
    pub sha256_before: String,
    pub sha256_after: String,
    pub backup: Option<String>,
    /// `entry <file> sha256 <hash>` or `gateway docker <profile>`.
    pub origin: String,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn random_hex() -> String {
    let mut b = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        b = (now_ms() as u64 ^ u64::from(std::process::id())).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The socket, as `contracts/daemon-protocol.md` section 1 places it. Kept
/// here rather than taken from `tk-daemon`, so the CLI does not link the
/// daemon (`tk_daemon::socket_path` is the same rule).
pub fn socket_path(home: &Path, xdg_runtime: Option<&Path>, macos: bool) -> PathBuf {
    if macos {
        home.join("Library/Application Support/tk/daemon.sock")
    } else {
        xdg_runtime
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".tk/run"))
            .join("tk/daemon.sock")
    }
}

fn detail(r: &Record) -> serde_json::Value {
    json!({
        "path": r.path,
        "server": r.server,
        "action": r.action,
        "sha256Before": r.sha256_before,
        "sha256After": r.sha256_after,
        "backup": r.backup,
        "origin": r.origin,
    })
}

fn summary(r: &Record) -> String {
    let s = format!("tk connect {} {} in {}", r.action, r.server, r.path);
    s.chars().take(200).collect()
}

/// Sends the record as one `event`. Any failure returns `Err`; the caller
/// falls back to the file.
fn to_daemon(socket: &Path, r: &Record) -> Result<(), String> {
    let token_path = socket.with_file_name("daemon.token");
    let token = std::fs::read_to_string(&token_path).map_err(|e| e.to_string())?;
    let token = token.trim();
    if !tk_core::token::well_formed(token) {
        return Err("daemon token malformed".into());
    }
    let mut s = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(DAEMON_TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(DAEMON_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let hello = json!({"op": "hello", "token": token, "client": "tk connect"});
    let event = json!({"op": "event", "event": {
        "id": format!("connect-{}-{}", now_ms(), random_hex()),
        "at": now_ms(),
        "chatId": "",
        "harness": r.harness,
        "source": "adapter",
        "kind": "config.write",
        "summary": summary(r),
        "detail": detail(r),
    }});
    let mut reader = BufReader::new(s.try_clone().map_err(|e| e.to_string())?);
    for line in [hello, event] {
        writeln!(s, "{line}").map_err(|e| e.to_string())?;
        let mut reply = String::new();
        reader.read_line(&mut reply).map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
        if v.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return Err(format!("daemon refused: {}", reply.trim()));
        }
    }
    Ok(())
}

fn to_file(log: &Path, r: &Record) -> Result<(), String> {
    if let Some(dir) = log.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let line = json!({
        "at": now_ms(),
        "harness": r.harness,
        "kind": "config.write",
        "detail": detail(r),
    });
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("{}: {e}", log.display()))
}

/// Records `r`. Returns where it went, for the user.
pub fn record(socket: &Path, log_file: &Path, r: &Record) -> Result<String, String> {
    match to_daemon(socket, r) {
        Ok(()) => Ok("the tk daemon's log".to_string()),
        Err(_) => to_file(log_file, r).map(|_| log_file.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect::write::tests::scratch;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixListener;

    fn rec() -> Record {
        Record {
            harness: "pi".into(),
            path: "/h/.pi/agent/mcp.json".into(),
            server: "exa".into(),
            action: "add".into(),
            sha256_before: "a".repeat(64),
            sha256_after: "b".repeat(64),
            backup: None,
            origin: "entry e.json sha256 c".into(),
        }
    }

    #[test]
    fn socket_paths_follow_the_contract() {
        let h = Path::new("/home/u");
        assert_eq!(
            socket_path(h, None, true),
            h.join("Library/Application Support/tk/daemon.sock")
        );
        assert_eq!(
            socket_path(h, Some(Path::new("/run/user/1")), false),
            Path::new("/run/user/1/tk/daemon.sock")
        );
        assert_eq!(
            socket_path(h, None, false),
            h.join(".tk/run/tk/daemon.sock")
        );
    }

    #[test]
    fn no_daemon_falls_back_to_a_private_log_file() {
        let d = scratch("log");
        let log = d.join(".tk/connect.log");
        let where_ = record(&d.join("none.sock"), &log, &rec()).unwrap();
        assert_eq!(where_, log.display().to_string());
        let text = std::fs::read_to_string(&log).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["detail"]["server"], "exa");
        assert_eq!(std::fs::metadata(&log).unwrap().mode() & 0o777, 0o600);
        record(&d.join("none.sock"), &log, &rec()).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
    }

    #[test]
    fn a_running_daemon_gets_hello_then_one_event() {
        let d = scratch("daemon");
        let sock = d.join("daemon.sock");
        std::fs::write(d.join("daemon.token"), "1".repeat(64)).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut w = s;
            let mut lines = Vec::new();
            for _ in 0..2 {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                lines.push(l);
                writeln!(w, "{{\"ok\": true}}").unwrap();
            }
            lines
        });
        let where_ = record(&sock, &d.join("connect.log"), &rec()).unwrap();
        assert_eq!(where_, "the tk daemon's log");
        let lines = server.join().unwrap();
        let hello: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(hello["op"], "hello");
        let ev: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(ev["event"]["kind"], "config.write");
        assert_eq!(ev["event"]["source"], "adapter");
        // The event parses as the contract's Event type.
        let _: tk_core::event::Event = serde_json::from_value(ev["event"].clone()).unwrap();
        assert!(!d.join("connect.log").exists());
    }

    #[test]
    fn a_daemon_that_refuses_falls_back_to_the_file() {
        let d = scratch("refuse");
        let sock = d.join("daemon.sock");
        std::fs::write(d.join("daemon.token"), "1".repeat(64)).unwrap();
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut w = s;
            writeln!(w, "{{\"ok\": false, \"error\": \"bad token\"}}").unwrap();
        });
        let log = d.join("connect.log");
        assert_eq!(
            record(&sock, &log, &rec()).unwrap(),
            log.display().to_string()
        );
    }
}

//! B2: the process registry. MonoCode knows the PID of every harness it
//! starts (`harness_spawn`) and every Cua runtime the helper starts, so it
//! tells the collector, which then follows those processes and their
//! descendants. The agent never touches this path.
//!
//! The messages are the registration socket's (`macos/activity-collector/
//! PROTOCOL.md`). Sends happen on one background thread with short timeouts,
//! so a missing or stuck collector never slows a spawn (case 9). The live set
//! is kept, and sent again whenever the collector comes back, so a harness
//! started before the collector was installed is still followed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde_json::json;

use super::types::ProcessKind;

pub const REGISTER_SOCKET: &str = "/var/run/monocode-activity/register.sock";
pub const READ_SOCKET: &str = "/var/run/monocode-activity/read.sock";
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LIVE: usize = 4_096;
const MAX_GRANTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveProcess {
    pub pid: u32,
    pub chat_id: String,
    pub kind: ProcessKind,
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveGrant {
    pub chat_id: String,
    pub bundle_id: String,
    pub app_path: Option<String>,
    pub at: i64,
}

/// A chat id the collector accepts: 1 to 128 of `[A-Za-z0-9_.:-]`.
pub fn valid_chat_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// A bundle id: 1 to 255 of `[A-Za-z0-9.-]`.
pub fn valid_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// An app path: absolute, a `.app`, no `..`, no NUL, at most 1,024 bytes.
pub fn valid_app_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 1_024
        && !path.contains('\0')
        && path.trim_end_matches('/').ends_with(".app")
        && !path.split('/').any(|part| part == "..")
}

pub fn register_message(process: &LiveProcess) -> String {
    let kind = match process.kind {
        ProcessKind::Harness => "harness",
        ProcessKind::CuaRuntime => "cuaRuntime",
    };
    json!({
        "v": 1, "op": "register", "pid": process.pid,
        "chatId": process.chat_id, "kind": kind, "at": process.at
    })
    .to_string()
}

pub fn grant_message(grant: &LiveGrant) -> String {
    let mut message = json!({
        "v": 1, "op": "grant", "chatId": grant.chat_id,
        "bundleId": grant.bundle_id, "at": grant.at
    });
    if let Some(path) = &grant.app_path {
        message["appPath"] = json!(path);
    }
    message.to_string()
}

pub fn grant_ended_message(chat_id: &str, bundle_id: &str, at: i64) -> String {
    json!({ "v": 1, "op": "grantEnded", "chatId": chat_id, "bundleId": bundle_id, "at": at })
        .to_string()
}

/// What the registry keeps and what it sends.
#[derive(Default)]
pub struct Registry {
    live: HashMap<u32, LiveProcess>,
    grants: HashMap<(String, String), LiveGrant>,
}

impl Registry {
    /// Adds a process. Returns the line to send, or `None` when refused.
    pub fn register(&mut self, process: LiveProcess) -> Option<String> {
        if process.pid == 0 || !valid_chat_id(&process.chat_id) {
            return None;
        }
        if !self.live.contains_key(&process.pid) && self.live.len() >= MAX_LIVE {
            return None;
        }
        let line = register_message(&process);
        self.live.insert(process.pid, process);
        Some(line)
    }

    /// Forgets an exited process. Nothing is sent: the collector learns exits
    /// from `eslogger`, and has no unregister for an agent to call.
    pub fn exited(&mut self, pid: u32) {
        self.live.remove(&pid);
    }

    pub fn grant(&mut self, grant: LiveGrant) -> Option<String> {
        if !valid_chat_id(&grant.chat_id) || !valid_bundle_id(&grant.bundle_id) {
            return None;
        }
        if grant
            .app_path
            .as_deref()
            .is_some_and(|p| !valid_app_path(p))
        {
            return None;
        }
        let key = (grant.chat_id.clone(), grant.bundle_id.clone());
        if !self.grants.contains_key(&key) && self.grants.len() >= MAX_GRANTS {
            return None;
        }
        let line = grant_message(&grant);
        self.grants.insert(key, grant);
        Some(line)
    }

    pub fn grant_ended(&mut self, chat_id: &str, bundle_id: &str, at: i64) -> Option<String> {
        self.grants
            .remove(&(chat_id.to_string(), bundle_id.to_string()))
            .map(|_| grant_ended_message(chat_id, bundle_id, at))
    }

    /// Every line that brings a fresh collector up to date.
    pub fn replay(&self) -> Vec<String> {
        let mut processes: Vec<&LiveProcess> = self.live.values().collect();
        processes.sort_by_key(|p| (p.at, p.pid));
        let mut lines: Vec<String> = processes.into_iter().map(register_message).collect();
        let mut grants: Vec<&LiveGrant> = self.grants.values().collect();
        grants.sort_by_key(|g| g.at);
        lines.extend(grants.into_iter().map(grant_message));
        lines
    }

    #[cfg(test)]
    pub fn chat_of(&self, pid: u32) -> Option<&str> {
        self.live.get(&pid).map(|p| p.chat_id.as_str())
    }

    pub fn live_count(&self) -> usize {
        self.live.len()
    }
}

enum Job {
    Send(String),
}

struct Shared {
    registry: Mutex<Registry>,
    tx: SyncSender<Job>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(1_024);
        let _ = thread::Builder::new()
            .name("activity-registry".into())
            .spawn(move || sender_loop(rx));
        Shared {
            registry: Mutex::new(Registry::default()),
            tx,
        }
    })
}

fn send(line: Option<String>) {
    if let Some(line) = line {
        let _ = shared().tx.try_send(Job::Send(line));
    }
}

pub fn register_process(pid: u32, chat_id: &str, kind: ProcessKind, at: i64) {
    let line = shared().registry.lock().ok().and_then(|mut r| {
        r.register(LiveProcess {
            pid,
            chat_id: chat_id.to_string(),
            kind,
            at,
        })
    });
    send(line);
}

pub fn process_exited(pid: u32) {
    if let Ok(mut registry) = shared().registry.lock() {
        registry.exited(pid);
    }
}

pub fn register_grant(chat_id: &str, bundle_id: &str, app_path: Option<&str>, at: i64) {
    let line = shared().registry.lock().ok().and_then(|mut r| {
        r.grant(LiveGrant {
            chat_id: chat_id.to_string(),
            bundle_id: bundle_id.to_string(),
            app_path: app_path.map(str::to_string),
            at,
        })
    });
    send(line);
}

pub fn end_grant(chat_id: &str, bundle_id: &str, at: i64) {
    let line = shared()
        .registry
        .lock()
        .ok()
        .and_then(|mut r| r.grant_ended(chat_id, bundle_id, at));
    send(line);
}

pub fn live_count() -> usize {
    shared()
        .registry
        .lock()
        .map(|r| r.live_count())
        .unwrap_or(0)
}

fn sender_loop(rx: Receiver<Job>) {
    // Whether the last send reached the collector. After a miss, the next
    // successful connection first replays the whole live set.
    let mut connected = false;
    for job in rx {
        let Job::Send(line) = job;
        let mut lines = Vec::new();
        if !connected {
            if let Some(shared) = SHARED.get() {
                if let Ok(registry) = shared.registry.lock() {
                    lines = registry.replay();
                }
            }
        }
        if !lines.contains(&line) {
            lines.push(line);
        }
        connected = deliver(&lines).is_ok();
    }
}

#[cfg(unix)]
fn deliver(lines: &[String]) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::net::UnixStream;
    // The protocol allows 64 lines a connection.
    for batch in lines.chunks(64) {
        let stream = UnixStream::connect(REGISTER_SOCKET)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream).take(64 * 1024);
        for line in batch {
            writer.write_all(line.as_bytes())?;
            writer.write_all(b"\n")?;
            // A refused line is not retried; the rest still go.
            let mut reply = String::new();
            reader.read_line(&mut reply)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn deliver(_lines: &[String]) -> std::io::Result<()> {
    Err(std::io::Error::other("no collector on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, chat: &str, at: i64) -> LiveProcess {
        LiveProcess {
            pid,
            chat_id: chat.into(),
            kind: ProcessKind::Harness,
            at,
        }
    }

    #[test]
    fn register_sends_the_protocol_line() {
        let mut registry = Registry::default();
        let line = registry.register(process(42, "chat-1", 7)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["op"], "register");
        assert_eq!(value["pid"], 42);
        assert_eq!(value["chatId"], "chat-1");
        assert_eq!(value["kind"], "harness");
        assert_eq!(value["v"], 1);
    }

    #[test]
    fn bad_ids_and_pid_zero_are_refused() {
        let mut registry = Registry::default();
        assert!(registry.register(process(0, "c", 1)).is_none());
        assert!(registry.register(process(1, "", 1)).is_none());
        assert!(registry.register(process(1, "a b", 1)).is_none());
        assert!(registry
            .register(process(1, "x\n{\"op\":\"grant\"}", 1))
            .is_none());
        assert!(registry.register(process(1, &"c".repeat(129), 1)).is_none());
    }

    #[test]
    fn an_exit_leaves_the_replay_and_sends_nothing() {
        let mut registry = Registry::default();
        registry.register(process(1, "a", 1));
        registry.register(process(2, "b", 2));
        registry.exited(1);
        let replay = registry.replay();
        assert_eq!(replay.len(), 1);
        assert!(replay[0].contains("\"pid\":2"));
        assert_eq!(registry.chat_of(2), Some("b"));
        assert_eq!(registry.chat_of(1), None);
    }

    #[test]
    fn a_reused_pid_moves_to_the_new_chat() {
        let mut registry = Registry::default();
        registry.register(process(5, "old", 1));
        registry.register(process(5, "new", 2));
        assert_eq!(registry.chat_of(5), Some("new"));
        assert_eq!(registry.replay().len(), 1);
    }

    #[test]
    fn grants_validate_and_end() {
        let mut registry = Registry::default();
        let grant = |path: Option<&str>| LiveGrant {
            chat_id: "c1".into(),
            bundle_id: "com.apple.calculator".into(),
            app_path: path.map(str::to_string),
            at: 3,
        };
        assert!(registry.grant(grant(Some("relative/Calc.app"))).is_none());
        assert!(registry
            .grant(grant(Some("/Applications/../etc/x.app")))
            .is_none());
        assert!(registry.grant(grant(Some("/Applications/Calc"))).is_none());
        let line = registry
            .grant(grant(Some("/System/Applications/Calculator.app")))
            .unwrap();
        assert!(line.contains("\"appPath\":\"/System/Applications/Calculator.app\""));
        assert_eq!(registry.replay().len(), 1);
        let ended = registry
            .grant_ended("c1", "com.apple.calculator", 9)
            .unwrap();
        assert!(ended.contains("grantEnded"));
        assert!(registry
            .grant_ended("c1", "com.apple.calculator", 9)
            .is_none());
        assert!(registry.replay().is_empty());
        let bad_bundle = LiveGrant {
            bundle_id: "com.apple/calc".into(),
            ..grant(None)
        };
        assert!(registry.grant(bad_bundle).is_none());
    }

    #[test]
    fn the_live_set_is_bounded() {
        let mut registry = Registry::default();
        for pid in 1..=(MAX_LIVE as u32) {
            assert!(registry.register(process(pid, "c", 0)).is_some());
        }
        assert!(registry
            .register(process(MAX_LIVE as u32 + 1, "c", 0))
            .is_none());
        // A known pid may still be updated.
        assert!(registry.register(process(1, "d", 0)).is_some());
    }

    #[test]
    fn delivery_without_a_collector_fails_fast() {
        let started = std::time::Instant::now();
        // No collector runs in the test workspace.
        if std::path::Path::new(REGISTER_SOCKET).exists() {
            return;
        }
        assert!(deliver(&["{}".to_string()]).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

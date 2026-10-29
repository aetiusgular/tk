//! The daemon's state and the protocol's ops (`contracts/daemon-protocol.md`,
//! section 2). `socket.rs` reads lines and hands each request here; this file
//! decides the reply, stores events, streams `tail` and runs `approve`.
//!
//! - One process: the OTLP receiver (`otel_receiver`), the activity log
//!   (`activity`) and the socket. Every event, from the receiver or a socket
//!   client, goes through `ingest`: bounded, stamped, written, then streamed.
//! - Nothing here blocks on a slow client. A `tail` that falls more than
//!   `MAX_PENDING_LINES` behind is dropped; an approver that has gone is
//!   skipped (rules, section 4, case 9).
//! - No outbound connection of any kind (rules, section 5, item 8).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver as Rx, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tk_core::event::{Event, Source};

use crate::activity::check::ChatContext;
use crate::activity::commands::{clean_context, flags_report};
use crate::activity::store::{self, ActivityLog, Entry};
use crate::activity::types::{ApprovalRecord, ApprovalScope, ApprovalSubject};
use crate::otel_receiver::{self, harness_env, Receiver, Settings};
use crate::protocol::{
    Answer, AnsweredBy, ApprovalRequest, CuAction, DaemonInfo, Push, Reply, Request, Scope,
    PROTOCOL_VERSION,
};

/// Events kept in memory for `tail`'s backlog, beside the log on disk. At
/// least the log's queue, so an event not yet written is still here.
pub const RING_CAPACITY: usize = 4_096;
/// A `tail` this many lines behind is dropped.
pub const MAX_PENDING_LINES: usize = 4_096;
/// At most this many events in one `tail` backlog.
pub const MAX_BACKLOG: usize = 5_000;
/// Events one `report` reads.
pub const REPORT_READ_LIMIT: usize = 20_000;
/// Approval waits are held to this range, in seconds.
pub const MIN_APPROVE_SECONDS: u32 = 1;
pub const MAX_APPROVE_SECONDS: u32 = 3_600;
/// Approvals waiting at once, over all connections.
pub const MAX_PENDING_APPROVALS: usize = 256;
/// Approvals one chat may have waiting at once, so one chat cannot use up
/// the rest (each refused one is `no`, never a hang).
pub const MAX_PENDING_PER_CHAT: usize = 16;

/// A running chat's harness: its pid and, where the OS says, when that
/// process started, so a reused pid is not taken for it after a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunPid {
    pub pid: u32,
    pub start_time: Option<u64>,
}

impl RunPid {
    fn of(pid: u32) -> Self {
        RunPid {
            pid,
            start_time: tk_core::peer::start_time(pid),
        }
    }

    /// Still the same process: it runs, and its start time matches when one
    /// was recorded.
    fn alive(&self) -> bool {
        match (self.start_time, tk_core::peer::start_time(self.pid)) {
            (Some(was), Some(now)) => was == now,
            (None, Some(_)) => true,
            (_, None) => false,
        }
    }
}

/// How long a minted token with no `run.start` yet survives a restart.
pub const UNSTARTED_TOKEN_MS: i64 = 3_600_000;
/// The state file is read only up to this size.
const STATE_FILE_MAX: u64 = 1024 * 1024;

/// Who a connection is, after `hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    Daemon,
    Chat { chat: String, harness: String },
}

/// One connection's way back to its client: whole lines, sent by the
/// connection's writer thread. `pending` counts lines queued and not yet
/// written, so a slow reader can be dropped instead of growing memory.
#[derive(Clone)]
pub struct Outbox {
    tx: Sender<String>,
    pending: Arc<AtomicUsize>,
    pub conn: u64,
    /// Who connected, for a socket connection (read at accept, S2). `None`
    /// for the daemon's own in-process clients (the collector, computer
    /// use), which are not a process an agent can start.
    pub peer: Option<Arc<ConnPeer>>,
}

/// The process on the other end of a socket connection, read when it was
/// accepted (integration w2, security finding S2).
#[derive(Debug, Default)]
pub struct ConnPeer {
    /// `SO_PEERCRED` / `LOCAL_PEERCRED`; `None` when the OS call failed.
    pub cred: Option<tk_core::peer::Peer>,
    /// The connecting process itself, on Linux 6.5 and later.
    pub pidfd: Option<std::os::fd::OwnedFd>,
    pub program: Option<String>,
    pub start_time: Option<u64>,
}

impl ConnPeer {
    /// Reads the peer of an accepted connection.
    pub fn of(stream: &std::os::unix::net::UnixStream) -> Self {
        let cred = tk_core::peer::of(stream);
        let pid = cred.as_ref().and_then(|c| c.pid);
        ConnPeer {
            pidfd: tk_core::peer::pidfd_of(stream),
            program: pid
                .and_then(tk_core::peer::program)
                .map(|p| p.to_string_lossy().into_owned()),
            start_time: pid.and_then(tk_core::peer::start_time),
            cred,
        }
    }

    /// What the log records about the peer.
    pub fn record(&self) -> Value {
        json!({
            "pid": self.cred.as_ref().and_then(|c| c.pid),
            "uid": self.cred.as_ref().map(|c| c.uid),
            "program": self.program.as_deref().map(|p| clip(p, 300)),
            "startTime": self.start_time,
            "pidfd": self.pidfd.is_some(),
        })
    }
}

impl Outbox {
    pub fn new(conn: u64) -> (Self, Rx<String>, Arc<AtomicUsize>) {
        Self::with_peer(conn, None)
    }

    /// An outbox for a socket connection, with who connected.
    pub fn with_peer(
        conn: u64,
        peer: Option<Arc<ConnPeer>>,
    ) -> (Self, Rx<String>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        (
            Outbox {
                tx,
                pending: pending.clone(),
                conn,
                peer,
            },
            rx,
            pending,
        )
    }

    /// Queues one line. False when the connection has gone.
    pub fn send_line(&self, line: String) -> bool {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(line).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    pub fn send<T: serde::Serialize>(&self, value: &T) -> bool {
        match serde_json::to_string(value) {
            Ok(line) => self.send_line(line),
            Err(_) => false,
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }
}

struct TailSub {
    chat: Option<String>,
    out: Outbox,
}

struct Pending {
    /// The answer and who gave it (the approver's peer record, S2).
    answer: SyncSender<(Answer, Value)>,
    chat: String,
}

/// Where the daemon keeps its files.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The socket; `daemon.pid`, `daemon.token` and `otlp.endpoint` sit
    /// beside it.
    pub socket: PathBuf,
    /// The data folder; the log is `<data>/activity`.
    pub data: PathBuf,
}

impl Paths {
    pub fn runtime_dir(&self) -> &Path {
        self.socket.parent().unwrap_or(Path::new("/"))
    }
    pub fn pid_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.pid")
    }
    pub fn token_file(&self) -> PathBuf {
        self.runtime_dir().join("daemon.token")
    }
    pub fn endpoint_file(&self) -> PathBuf {
        self.runtime_dir().join("otlp.endpoint")
    }
    /// Running chats kept across a restart (BX8; integration w2).
    pub fn state_file(&self) -> PathBuf {
        self.runtime_dir().join("chats.json")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.data.join("activity")
    }
}

/// The daemon's data folder per OS: macOS
/// `~/Library/Application Support/tk`, else `$XDG_DATA_HOME/tk`, else
/// `~/.local/share/tk`. The socket folder on Linux is `$XDG_RUNTIME_DIR`,
/// which is emptied at logout, so the log never lives there.
pub fn default_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/tk"));
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".local/share")))
        .map(|d| d.join("tk"))
}

pub struct Daemon {
    token_hash: String,
    log: &'static ActivityLog,
    log_dir: PathBuf,
    data_dir: String,
    receiver: Arc<Receiver>,
    /// Serializes ingest's ring and broadcast with a tail's backlog, so a
    /// tail neither misses nor repeats an event.
    order: Mutex<VecDeque<Event>>,
    tails: Mutex<Vec<TailSub>>,
    approvers: Mutex<Vec<Outbox>>,
    /// Waiting approvals by (chat, request id): two chats may wait on the
    /// same client id (S8).
    pending: Mutex<HashMap<(String, String), Pending>>,
    chats: Mutex<HashMap<String, ChatContext>>,
    /// Each running chat's harness pid, from its daemon-scope `run.start`
    /// until its `run.stop` (S2: an approver may not be one of them or run
    /// under one).
    runs: Mutex<HashMap<String, RunPid>>,
    /// What `tk status` shows for each running chat, from its `run.start`:
    /// harness, boundary, when, full-access flags (box test w2, BW3: the
    /// tail's backlog lost a chat behind a flood of newer events).
    run_info: Mutex<HashMap<String, Value>>,
    /// When each live chat token was minted, by its hash (hex), for the
    /// state file (BX8).
    minted_at: Mutex<HashMap<String, i64>>,
    /// `<socket folder>/chats.json` when the daemon keeps its chats across a
    /// restart (BX8); `None` in tests that build a daemon by hand.
    state_file: Option<PathBuf>,
    next_conn: AtomicU64,
    pub counters: Counters,
}

#[derive(Default)]
pub struct Counters {
    pub events: AtomicU64,
    pub refused_events: AtomicU64,
    pub tails_dropped: AtomicU64,
    pub approvals: AtomicU64,
}

fn now_ms() -> i64 {
    store::now_ms()
}

fn ok() -> Reply {
    Reply {
        ok: true,
        error: None,
        scope: None,
        chat: None,
        daemon: None,
        id: None,
        token: None,
        answer: None,
        by: None,
        report: None,
    }
}

pub fn error(message: impl Into<String>) -> Reply {
    Reply {
        ok: false,
        error: Some(message.into()),
        ..ok()
    }
}

/// Text for a log line or a summary: one line, bounded.
fn clip(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect()
}

/// What a connection does after its reply.
pub enum After {
    Continue,
    Close,
}

impl Daemon {
    /// Builds the daemon around a started log and its own receiver. The
    /// receiver's sink is this daemon's `ingest`; it binds 127.0.0.1 on a
    /// free port.
    pub fn new(
        token: &str,
        log: &'static ActivityLog,
        data_dir: &Path,
        log_prompts: bool,
    ) -> Result<Arc<Self>, String> {
        Self::new_with_state(token, log, data_dir, log_prompts, None)
    }

    /// As `new`, keeping running chats across a restart in `state_file`
    /// (integration w2, box finding BX8): the chat tokens' hashes (never a
    /// token), each running chat's harness pid and start time, and the OTLP
    /// port. At start it takes back the tokens of chats whose harness still
    /// runs, and of tokens minted in the last hour whose chat has not
    /// started, and binds the old port when it is free. The file is used only
    /// when it is a regular file of this user's, mode 0600 or tighter.
    pub fn new_with_state(
        token: &str,
        log: &'static ActivityLog,
        data_dir: &Path,
        log_prompts: bool,
        state_file: Option<PathBuf>,
    ) -> Result<Arc<Self>, String> {
        let saved = state_file.as_deref().and_then(read_state);
        let preferred = saved
            .as_ref()
            .and_then(|v| v.get("otlpPort"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());
        let settings = Arc::new(Mutex::new(Settings {
            enabled: true,
            log_prompts,
            chats: HashSet::new(),
        }));
        let cell: Arc<Mutex<Option<std::sync::Weak<Daemon>>>> = Arc::new(Mutex::new(None));
        let sink_cell = cell.clone();
        let sink: otel_receiver::Sink = Arc::new(move |events| {
            let daemon = sink_cell
                .lock()
                .ok()
                .and_then(|d| d.as_ref().and_then(std::sync::Weak::upgrade));
            if let Some(daemon) = daemon {
                for event in events {
                    daemon.ingest(event);
                }
            }
        });
        let receiver = Receiver::start_on(settings, sink, preferred)?;
        let daemon = Arc::new(Daemon {
            token_hash: tk_core::token::hash(token),
            log,
            log_dir: log.dir().to_path_buf(),
            data_dir: data_dir.to_string_lossy().into_owned(),
            receiver,
            order: Mutex::new(VecDeque::new()),
            tails: Mutex::new(Vec::new()),
            approvers: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            chats: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
            run_info: Mutex::new(HashMap::new()),
            minted_at: Mutex::new(HashMap::new()),
            state_file,
            next_conn: AtomicU64::new(1),
            counters: Counters::default(),
        });
        if let Ok(mut c) = cell.lock() {
            *c = Some(Arc::downgrade(&daemon));
        }
        if let Some(saved) = saved {
            daemon.restore_state(&saved);
        }
        daemon.save_state();
        Ok(daemon)
    }

    /// Takes back what `save_state` wrote, keeping only chats that still
    /// run (BX8). Returns how many tokens came back.
    fn restore_state(&self, saved: &Value) -> usize {
        let now = now_ms();
        let mut runs: HashMap<String, RunPid> = HashMap::new();
        let mut infos: HashMap<String, Value> = HashMap::new();
        // Chats that had started, running or not: a started chat comes back
        // only while its harness runs.
        let mut started: HashSet<String> = HashSet::new();
        for r in saved
            .get("runs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(chat), Some(pid)) = (
                r.get("chat").and_then(Value::as_str),
                r.get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok()),
            ) else {
                continue;
            };
            let run = RunPid {
                pid,
                start_time: r.get("startTime").and_then(Value::as_u64),
            };
            if let Some(i) = r.get("info").map(run_info_restored) {
                infos.insert(chat.to_string(), i);
            }
            started.insert(chat.to_string());
            // A run comes back only with the start time it was recorded with
            // and never as pid 1 or 0 (review W2-S9: a planted run with no
            // start time would match any live process for good).
            if crate::activity::registry::valid_chat_id(chat)
                && pid > 1
                && run.start_time.is_some()
                && run.alive()
            {
                runs.insert(chat.to_string(), run);
            }
        }
        let mut restored = 0;
        let mut minted_at = HashMap::new();
        for t in saved
            .get("tokens")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(sha), Some(chat), Some(harness), Some(at)) = (
                t.get("sha256").and_then(Value::as_str),
                t.get("chat").and_then(Value::as_str),
                t.get("harness")
                    .and_then(Value::as_str)
                    .and_then(harness_env::harness_id),
                t.get("mintedAt").and_then(Value::as_i64),
            ) else {
                continue;
            };
            let Some(hash) = parse_sha256(sha) else {
                continue;
            };
            let keep = runs.contains_key(chat)
                || (!started.contains(chat) && now.saturating_sub(at) < UNSTARTED_TOKEN_MS);
            if keep
                && crate::activity::registry::valid_chat_id(chat)
                && self.receiver.restore(hash, chat, harness)
            {
                minted_at.insert(sha.to_string(), at);
                restored += 1;
            }
        }
        if let Ok(mut m) = self.minted_at.lock() {
            *m = minted_at;
        }
        infos.retain(|chat, _| runs.contains_key(chat));
        if let Ok(mut i) = self.run_info.lock() {
            *i = infos;
        }
        if let Ok(mut r) = self.runs.lock() {
            *r = runs;
        }
        restored
    }

    /// The chats `tk run` started that still run: chat, pid, whether that
    /// process is still the one that started (its start time), and what its
    /// `run.start` said. Daemon scope only (in `report`).
    pub fn running(&self) -> Vec<Value> {
        let runs = self.runs.lock().map(|r| r.clone()).unwrap_or_default();
        let info = self.run_info.lock().map(|i| i.clone()).unwrap_or_default();
        let mut out: Vec<Value> = runs
            .iter()
            .map(|(chat, run)| {
                let mut row = info.get(chat).cloned().unwrap_or_else(|| json!({}));
                if let Value::Object(m) = &mut row {
                    m.insert("chat".into(), Value::String(chat.clone()));
                    m.insert("pid".into(), Value::from(run.pid));
                    m.insert("alive".into(), Value::Bool(run.alive()));
                }
                row
            })
            .collect();
        out.sort_by(|a, b| {
            a.get("since")
                .and_then(Value::as_i64)
                .cmp(&b.get("since").and_then(Value::as_i64))
                .then_with(|| a["chat"].as_str().cmp(&b["chat"].as_str()))
        });
        out
    }

    /// Writes the state file (BX8): token hashes, running chats, the port.
    /// Best effort; a failure leaves the old file, and the next change
    /// writes again.
    fn save_state(&self) {
        let Some(path) = self.state_file.as_deref() else {
            return;
        };
        let minted = self.minted_at.lock().map(|m| m.clone()).unwrap_or_default();
        let tokens: Vec<Value> = self
            .receiver
            .holders_snapshot()
            .into_iter()
            .filter_map(|(hash, chat, harness)| {
                let sha = hex(&hash);
                minted.get(&sha).map(
                    |at| json!({"sha256": sha, "chat": chat, "harness": harness, "mintedAt": at}),
                )
            })
            .collect();
        let info = self.run_info.lock().map(|i| i.clone()).unwrap_or_default();
        let runs: Vec<Value> = self
            .runs
            .lock()
            .map(|r| {
                r.iter()
                    .map(|(chat, run)| {
                        let mut row =
                            json!({"chat": chat, "pid": run.pid, "startTime": run.start_time});
                        if let (Some(i), Value::Object(m)) = (info.get(chat), &mut row) {
                            m.insert("info".into(), i.clone());
                        }
                        row
                    })
                    .collect()
            })
            .unwrap_or_default();
        let port = self
            .receiver
            .endpoint()
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok());
        let text = json!({
            "version": 1,
            "otlpPort": port,
            "tokens": tokens,
            "runs": runs,
        })
        .to_string();
        let _ = crate::service::write_private(path, &text);
    }

    pub fn endpoint(&self) -> String {
        self.receiver.endpoint()
    }

    pub fn next_conn(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// Stores one event and streams it. The daemon stamps `at` (case 35);
    /// a client's own time moves to `detail.harnessAt`. Returns the id kept.
    pub fn ingest(&self, mut event: Event) -> String {
        if event.at > 0 {
            if let Value::Object(map) = &mut event.detail {
                map.entry("harnessAt").or_insert(Value::from(event.at));
            }
        }
        event.at = now_ms();
        let event = store::bound_event(event);
        self.log.submit(Entry::Event(event.clone()));
        self.counters.events.fetch_add(1, Ordering::Relaxed);
        let line = serde_json::to_string(&Push::Event(event.clone())).ok();
        let Ok(mut ring) = self.order.lock() else {
            return event.id;
        };
        ring.push_back(event.clone());
        while ring.len() > RING_CAPACITY {
            ring.pop_front();
        }
        if let (Some(line), Ok(mut tails)) = (line, self.tails.lock()) {
            tails.retain(|sub| {
                if sub.chat.as_deref().is_some_and(|c| c != event.chat_id) {
                    return true;
                }
                if sub.out.pending() >= MAX_PENDING_LINES || !sub.out.send_line(line.clone()) {
                    self.counters.tails_dropped.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                true
            });
        }
        drop(ring);
        event.id
    }

    fn record_approval(&self, record: ApprovalRecord) {
        self.log.submit(Entry::Approval(record));
    }

    /// `hello`: the daemon's token gives the daemon scope, a minted chat
    /// token the chat scope. Anything else closes the connection.
    pub fn hello(&self, token: &str) -> Option<Who> {
        if !tk_core::token::well_formed(token) {
            return None;
        }
        if tk_core::token::hash(token) == self.token_hash {
            return Some(Who::Daemon);
        }
        self.receiver
            .holder_of(token)
            .map(|(chat, harness)| Who::Chat {
                chat,
                harness: harness.to_string(),
            })
    }

    pub fn hello_reply(who: &Who) -> Reply {
        match who {
            Who::Daemon => Reply {
                scope: Some(Scope::Daemon),
                daemon: Some(DaemonInfo {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    pid: std::process::id(),
                    protocol: PROTOCOL_VERSION,
                }),
                ..ok()
            },
            Who::Chat { chat, .. } => Reply {
                scope: Some(Scope::Chat),
                chat: Some(chat.clone()),
                ..ok()
            },
        }
    }

    /// One request after `hello`. The reply goes out first; `tail` and
    /// `approver` then push on `out`.
    pub fn handle(self: &Arc<Self>, who: &Who, request: Request, out: &Outbox) -> After {
        match request {
            Request::Hello { .. } => {
                out.send(&error("hello once"));
            }
            Request::Event { event } => {
                out.send(&self.op_event(who, event));
            }
            Request::Tail { since, chat } => self.op_tail(who, since, chat, out),
            Request::Report { chat, since, until } => {
                out.send(&self.op_report(who, chat, since, until));
            }
            Request::Approve { request } => {
                out.send(&self.op_approve(who, request));
            }
            Request::Approver {} => {
                if *who != Who::Daemon {
                    out.send(&error("approver needs daemon scope"));
                } else if let Some(why) = out.peer.as_deref().and_then(|p| self.approver_refusal(p))
                {
                    self.approver_event(out, false, Some(&why));
                    out.send(&error(format!("approver refused: {why}")));
                } else if let Ok(mut approvers) = self.approvers.lock() {
                    approvers.retain(|a| a.conn != out.conn);
                    approvers.push(out.clone());
                    drop(approvers);
                    self.approver_event(out, true, None);
                    out.send(&ok());
                }
            }
            Request::Answer { id, answer, chat } => {
                out.send(&self.op_answer(out.conn, &id, chat.as_deref(), answer));
            }
            Request::TokenMint { chat, harness } => {
                out.send(&self.op_mint(who, &chat, &harness));
            }
            Request::Cu { action, chat, app } => {
                out.send(&op_cu(who, action, chat.as_deref(), app.as_deref()));
            }
        }
        After::Continue
    }

    fn op_event(&self, who: &Who, mut event: Event) -> Reply {
        match who {
            Who::Chat { chat, harness } => {
                // A chat token sits in a harness's environment: it may post
                // its own telemetry and adapter records, in its own chat,
                // and nothing a kernel feed, computer use or `tk run` writes
                // (the fork's review U2).
                if !matches!(event.source, Source::Adapter | Source::Otel) {
                    self.counters.refused_events.fetch_add(1, Ordering::Relaxed);
                    return error("a chat token posts only adapter and otel events");
                }
                event.chat_id = chat.clone();
                event.harness = harness.clone();
            }
            Who::Daemon => {
                // Approval records are the daemon's own (`approve`).
                if event.source == Source::Approval {
                    self.counters.refused_events.fetch_add(1, Ordering::Relaxed);
                    return error("approval events are written by the daemon");
                }
                if event.source == Source::Run && event.kind == "run.start" {
                    self.set_chat_from_run(&event);
                    if let (Some(pid), Ok(mut runs)) = (event.pid, self.runs.lock()) {
                        if runs.len() < 2_048 || runs.contains_key(&event.chat_id) {
                            runs.insert(event.chat_id.clone(), RunPid::of(pid));
                            if let Ok(mut info) = self.run_info.lock() {
                                info.insert(event.chat_id.clone(), run_info_of(&event, now_ms()));
                            }
                        }
                    }
                    self.save_state();
                }
                // The run is over: its chat tokens open nothing from now on
                // (integration w1, lane D's request 2). A connection already
                // greeted keeps its scope until it closes.
                if event.source == Source::Run && event.kind == "run.stop" {
                    self.receiver.revoke_chat(&event.chat_id);
                    if let Ok(mut runs) = self.runs.lock() {
                        runs.remove(&event.chat_id);
                    }
                    if let Ok(mut info) = self.run_info.lock() {
                        info.remove(&event.chat_id);
                    }
                    self.save_state();
                }
            }
        }
        if store::bound_event(event.clone()).id.is_empty() {
            return error("event id is empty");
        }
        if matches!(who, Who::Chat { .. }) && self.holds_gate_decision(&event) {
            // The hook and the agent share the chat token (case 66), and the
            // agent can post first (integration w2 review, W2-S3), so a second
            // copy is never dropped: it is kept under its own id, and the
            // daemon records the conflict, which only it can write.
            return self.keep_conflict(event);
        }
        let id = self.ingest(event);
        Reply {
            id: Some(id),
            ..ok()
        }
    }

    /// A chat-scope post under the id of a gate decision already held:
    /// stored as `<id>-conflict-<uuid>` with `detail.conflictsWith`, and a
    /// daemon-written `gate.conflict` (source `approval`) naming both.
    fn keep_conflict(&self, mut event: Event) -> Reply {
        let original = store::bound_event(event.clone()).id;
        // The copy's id must survive `bound_event`'s cut to `MAX_ID`, or a long
        // original would come back as itself (re-review W2-R2): the original
        // part is cut so the suffix always fits.
        let suffix = format!("-conflict-{}", uuid::Uuid::new_v4().simple());
        let keep = store::MAX_ID.saturating_sub(suffix.len());
        let head: String = original.chars().take(keep).collect();
        let copy = format!("{head}{suffix}");
        event.id = copy.clone();
        match &mut event.detail {
            Value::Object(map) => {
                map.insert("conflictsWith".into(), Value::String(original.clone()));
            }
            other => *other = json!({"value": other.clone(), "conflictsWith": original}),
        }
        let (chat, harness, kind, source) = (
            event.chat_id.clone(),
            event.harness.clone(),
            event.kind.clone(),
            event.source,
        );
        let kept = self.ingest(event);
        self.ingest(Event {
            id: format!("gate-conflict-{}", uuid::Uuid::new_v4().simple()),
            at: 0,
            chat_id: chat,
            harness,
            source: Source::Approval,
            kind: "gate.conflict".into(),
            summary: clip(
                &format!("two records under the gate decision {original}"),
                200,
            ),
            detail: json!({"id": original, "copy": kept, "kind": kind, "source": source}),
            pid: None,
            approval_id: None,
        });
        Reply {
            id: Some(kept),
            ..ok()
        }
    }

    /// True when the ring holds a gate decision under this event's chat and
    /// id (S9). Readers keep the first copy whatever is written later
    /// (`store::may_replace`); this refusal only tells the poster, for a
    /// decision still in the last `RING_CAPACITY` events.
    fn holds_gate_decision(&self, event: &Event) -> bool {
        let id = store::bound_event(event.clone()).id;
        self.order.lock().is_ok_and(|ring| {
            ring.iter().rev().any(|old| {
                old.id == id && old.chat_id == event.chat_id && store::is_gate_decision(old)
            })
        })
    }

    /// A daemon-scope `run.start` may name the chat's folders
    /// (`detail.folders`, or `detail.cwd`), which the check needs for
    /// `writeOutsideFolders`. Only the daemon scope may: a chat could
    /// otherwise widen its own folders.
    fn set_chat_from_run(&self, event: &Event) {
        let mut folders: Vec<String> = event
            .detail
            .get("folders")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(cwd) = event.detail.get("cwd").and_then(Value::as_str) {
            folders.insert(0, cwd.to_string());
        }
        let Some(context) = clean_context(ChatContext {
            chat_id: event.chat_id.clone(),
            harness: event.harness.clone(),
            folders,
        }) else {
            return;
        };
        if let Ok(mut chats) = self.chats.lock() {
            if chats.len() >= 2_048 && !chats.contains_key(&context.chat_id) {
                return;
            }
            chats.insert(context.chat_id.clone(), context);
        }
    }

    fn op_tail(
        self: &Arc<Self>,
        who: &Who,
        since: Option<i64>,
        chat: Option<String>,
        out: &Outbox,
    ) {
        let chat = match (who, chat) {
            (Who::Daemon, chat) => chat,
            (Who::Chat { chat: own, .. }, Some(c)) if *own == c => Some(c),
            (Who::Chat { .. }, Some(_)) => {
                out.send(&error("a chat token tails only its own chat"));
                return;
            }
            (Who::Chat { .. }, None) => {
                out.send(&error("tail needs daemon scope or a chat filter"));
                return;
            }
        };
        // The log on disk first, outside the lock; then, under it, the ring
        // (which holds anything not yet on disk), the subscription, and only
        // then the `ok`. `ingest` broadcasts under the same lock, so nothing
        // a client does after it reads `ok` can be ingested before the
        // subscription exists (lane K's request 5: the old order sent `ok`
        // first, and an event ingested in between was never pushed).
        let mut backlog: Vec<Event> = match since {
            Some(since) => {
                store::read(&self.log_dir, since, i64::MAX, chat.as_deref(), MAX_BACKLOG).events
            }
            None => Vec::new(),
        };
        let Ok(ring) = self.order.lock() else {
            out.send(&error("busy"));
            return;
        };
        if let Some(since) = since {
            backlog = merge_backlog(backlog, ring.iter(), since, chat.as_deref());
        }
        let Ok(mut tails) = self.tails.lock() else {
            out.send(&error("busy"));
            return;
        };
        out.send(&ok());
        for event in backlog {
            out.send(&Push::Event(event));
        }
        tails.push(TailSub {
            chat,
            out: out.clone(),
        });
        drop(tails);
        drop(ring);
        #[cfg(test)]
        tests::after_tail_ok();
    }

    /// The `report` object (contract, section 5: lane B defines it, lane J
    /// renders it): counts over the window, and the check's flags.
    fn op_report(&self, who: &Who, chat: Option<String>, since: i64, until: i64) -> Reply {
        if *who != Who::Daemon {
            return error("report needs daemon scope");
        }
        if until < since {
            return error("until is before since");
        }
        let mut report = self.report(chat.as_deref(), since, until);
        if let Value::Object(m) = &mut report {
            let running: Vec<Value> = self
                .running()
                .into_iter()
                .filter(|r| chat.as_deref().is_none_or(|c| r["chat"] == c))
                .collect();
            m.insert("running".into(), Value::Array(running));
        }
        Reply {
            report: Some(report),
            ..ok()
        }
    }

    pub fn report(&self, chat: Option<&str>, since: i64, until: i64) -> Value {
        let _guard = self.log.read_lock.lock();
        let back = store::read(&self.log_dir, since, until, chat, REPORT_READ_LIMIT);
        let mut by_source: Map<String, Value> = Map::new();
        let mut by_kind: Map<String, Value> = Map::new();
        let mut approvals: Map<String, Value> = Map::new();
        for e in &back.events {
            bump(
                &mut by_source,
                &serde_json::to_value(e.source).unwrap_or_default(),
            );
            bump(&mut by_kind, &Value::String(e.kind.clone()));
            if e.source == Source::Approval {
                if let Some(by) = e.detail.get("by").and_then(Value::as_str) {
                    let answer = e.detail.get("answer").and_then(Value::as_str).unwrap_or("");
                    bump(&mut approvals, &Value::String(format!("{answer}:{by}")));
                }
            }
        }
        let chats = self.chats.lock().map(|c| c.clone()).unwrap_or_default();
        let collector = collector_events(since, until, chat);
        let mut check = flags_report(
            &self.log_dir,
            Some(&self.data_dir),
            chat,
            since,
            until,
            &chats,
            collector,
            now_ms(),
        );
        check.dropped = self.log.dropped();
        json!({
            "since": since,
            "until": until,
            "chat": chat,
            "events": back.events.len(),
            "flags": check.flags.len(),
            "bySource": by_source,
            "byKind": by_kind,
            "approvals": approvals,
            "chainOk": back.chain_ok && check.chain_ok,
            "truncated": back.truncated || check.truncated,
            "dropped": check.dropped,
            "check": check,
        })
    }

    fn op_approve(&self, who: &Who, mut request: ApprovalRequest) -> Reply {
        if let Who::Chat { chat, harness } = who {
            request.chat = chat.clone();
            request.harness = harness.clone();
        }
        request.id = clip(&request.id, 128);
        request.tool = clip(&request.tool, 200);
        request.summary = clip(&request.summary, 500);
        request.timeout_seconds = request
            .timeout_seconds
            .clamp(MIN_APPROVE_SECONDS, MAX_APPROVE_SECONDS);
        if request.id.is_empty() {
            return error("approval id is empty");
        }
        self.counters.approvals.fetch_add(1, Ordering::Relaxed);
        let record_id = format!("approval-{}", uuid::Uuid::new_v4().simple());
        self.approval_event(&record_id, &request, None, None, Value::Null);
        let (answer, by, approver) = self.wait_for_answer(&request);
        self.approval_event(&record_id, &request, Some(answer), Some(by), approver);
        self.record_approval(ApprovalRecord {
            id: record_id,
            chat_id: request.chat.clone(),
            harness: request.harness.clone(),
            subject: ApprovalSubject::Tool,
            target: request.tool.clone(),
            scope: ApprovalScope::Once,
            granted: answer == Answer::Yes,
            granted_at: now_ms(),
            ended_at: None,
        });
        Reply {
            id: Some(request.id),
            answer: Some(answer),
            by: Some(by),
            ..ok()
        }
    }

    /// Sends the request to every approver; the first answer wins. None
    /// connected: `no` at once (case 63). None in time: `no` at the timeout.
    fn wait_for_answer(&self, request: &ApprovalRequest) -> (Answer, AnsweredBy, Value) {
        let (tx, rx) = mpsc::sync_channel::<(Answer, Value)>(1);
        {
            let Ok(mut pending) = self.pending.lock() else {
                return (Answer::No, AnsweredBy::None, Value::Null);
            };
            let key = (request.chat.clone(), request.id.clone());
            let this_chat = pending.values().filter(|p| p.chat == request.chat).count();
            if pending.contains_key(&key)
                || pending.len() >= MAX_PENDING_APPROVALS
                || this_chat >= MAX_PENDING_PER_CHAT
            {
                return (Answer::No, AnsweredBy::None, Value::Null);
            }
            pending.insert(
                key,
                Pending {
                    answer: tx,
                    chat: request.chat.clone(),
                },
            );
        }
        let line = serde_json::to_string(&Push::Approve(request.clone())).ok();
        let mut reached = 0;
        if let (Some(line), Ok(mut approvers)) = (line, self.approvers.lock()) {
            approvers.retain(|a| a.send_line(line.clone()));
            reached = approvers.len();
        }
        let outcome = if reached == 0 {
            (Answer::No, AnsweredBy::None, Value::Null)
        } else {
            match rx.recv_timeout(Duration::from_secs(u64::from(request.timeout_seconds))) {
                Ok((answer, approver)) => (answer, AnsweredBy::Approver, approver),
                Err(_) => (Answer::No, AnsweredBy::Timeout, Value::Null),
            }
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&(request.chat.clone(), request.id.clone()));
        }
        outcome
    }

    fn op_answer(&self, conn: u64, id: &str, chat: Option<&str>, answer: Answer) -> Reply {
        let approver = self
            .approvers
            .lock()
            .ok()
            .and_then(|a| a.iter().find(|o| o.conn == conn).cloned());
        let Some(approver) = approver else {
            return error("not an approver");
        };
        let who = match approver.peer.as_deref() {
            Some(p) => p.record(),
            None => json!({ "inProcess": true }),
        };
        let Ok(pending) = self.pending.lock() else {
            return error("busy");
        };
        let mut waiting = pending
            .iter()
            .filter(|((c, i), _)| i == id && chat.is_none_or(|want| want == c));
        match (waiting.next(), waiting.next()) {
            // The channel holds one answer: the first wins, the rest fail
            // to send and are told nothing more.
            (Some((_, p)), None) => {
                let _ = p.answer.try_send((answer, who));
                ok()
            }
            (Some(_), Some(_)) => error("two chats wait on this id: name the chat"),
            (None, _) => error("no such request, or it was answered or timed out"),
        }
    }

    /// Writes the approval's event; the outcome replaces the request under
    /// the same id (case 35: last copy per id).
    fn approval_event(
        &self,
        record_id: &str,
        request: &ApprovalRequest,
        answer: Option<Answer>,
        by: Option<AnsweredBy>,
        approver: Value,
    ) {
        let word = |v: Option<Value>| v.and_then(|v| v.as_str().map(str::to_string));
        let answer_text = word(answer.and_then(|a| serde_json::to_value(a).ok()));
        let by_text = word(by.and_then(|b| serde_json::to_value(b).ok()));
        let summary = match (&answer_text, &by_text) {
            (Some(a), Some(b)) => format!("{}: {a} ({b})", request.tool),
            _ => format!("{}: asked", request.tool),
        };
        let mut detail = json!({
            "requestId": request.id,
            "tool": request.tool,
            "request": request.summary,
            "timeoutSeconds": request.timeout_seconds,
        });
        if let (Some(a), Some(b), Value::Object(map)) = (answer_text, by_text, &mut detail) {
            map.insert("answer".into(), Value::String(a));
            map.insert("by".into(), Value::String(b));
            if !approver.is_null() {
                map.insert("approver".into(), approver);
            }
        }
        self.ingest(Event {
            id: record_id.to_string(),
            at: 0,
            chat_id: request.chat.clone(),
            harness: request.harness.clone(),
            source: Source::Approval,
            kind: "approval".into(),
            summary,
            detail,
            pid: None,
            approval_id: Some(request.id.clone()),
        });
    }

    /// Why a socket peer may not become an approver, if it may not
    /// (integration w2, security finding S2; lane J's request 1a). A process
    /// that is a running chat's harness or descends from one is the agent;
    /// so is one under a `tk` program (a chat this daemon may not know, from
    /// before a restart). This does not stop an agent that leaves the tree
    /// (a double fork, `setsid`, tmux, `systemd-run --user`): S2 stays open
    /// until the approver proves a person (a PAM check) or agents run as
    /// another user.
    fn approver_refusal(&self, peer: &ConnPeer) -> Option<String> {
        let Some(cred) = peer.cred.as_ref() else {
            return Some("the daemon cannot read who is asking".into());
        };
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if cred.uid != me {
            return Some(format!("uid {} is not this daemon's user", cred.uid));
        }
        let Some(pid) = cred.pid else {
            return Some("the OS gave no pid for the peer".into());
        };
        // The pid must still be the process read at accept (review W2-S5): a
        // child that connects and exits leaves a pid with no ancestors, and
        // without a pidfd (macOS, Linux before 6.5) nothing else catches it.
        if let Some(why) = Self::peer_moved(pid, peer.start_time) {
            return Some(why);
        }
        let mut line = vec![pid];
        line.extend(tk_core::peer::ancestors(pid));
        let runs = self.runs.lock().map(|r| r.clone()).unwrap_or_default();
        for (chat, run) in &runs {
            let harness_pid = &run.pid;
            if line.contains(harness_pid) {
                return Some(format!(
                    "it runs inside chat {} (harness pid {harness_pid})",
                    clip(chat, 128)
                ));
            }
        }
        if let Some(tk) = tk_core::peer::tk_run_ancestor(pid) {
            return Some(format!(
                "it runs under a tk process (pid {tk}), a chat's shell"
            ));
        }
        // Read last, so a pid that was reused while the walk ran is caught.
        if let Some(fd) = peer.pidfd.as_ref() {
            if !tk_core::peer::pidfd_alive(fd) {
                return Some("the process that connected has exited".into());
            }
        }
        Self::peer_moved(pid, peer.start_time)
    }

    /// Why `pid` is no longer the process whose start time was read when the
    /// connection was accepted, if it is not.
    fn peer_moved(pid: u32, at_accept: Option<u64>) -> Option<String> {
        let Some(at_accept) = at_accept else {
            return Some("the daemon could not read the process at connect".into());
        };
        match tk_core::peer::start_time(pid) {
            None => Some("the process that connected is gone".into()),
            Some(now) if now != at_accept => {
                Some(format!("pid {pid} is not the process that connected"))
            }
            Some(_) => None,
        }
    }

    /// Records an approver's registration, or its refusal, with who it was.
    fn approver_event(&self, out: &Outbox, accepted: bool, why: Option<&str>) {
        let peer = out
            .peer
            .as_deref()
            .map(ConnPeer::record)
            .unwrap_or(Value::Null);
        let program = peer
            .get("program")
            .and_then(Value::as_str)
            .unwrap_or("unknown program")
            .to_string();
        let pid = peer.get("pid").and_then(Value::as_u64);
        let summary = match (accepted, why) {
            (true, _) => format!("approver connected: {program} (pid {})", pid.unwrap_or(0)),
            (false, why) => format!("approver refused: {}", why.unwrap_or("")),
        };
        self.ingest(Event {
            id: format!("approver-{}", uuid::Uuid::new_v4().simple()),
            at: 0,
            chat_id: String::new(),
            harness: String::new(),
            source: Source::Approval,
            kind: "approver".into(),
            summary: clip(&summary, 200),
            detail: json!({
                "accepted": accepted,
                "reason": why,
                "peer": peer,
                "conn": out.conn,
            }),
            pid: pid.and_then(|p| u32::try_from(p).ok()),
            approval_id: None,
        });
    }

    fn op_mint(&self, who: &Who, chat: &str, harness: &str) -> Reply {
        if *who != Who::Daemon {
            return error("token.mint needs daemon scope");
        }
        if !crate::activity::registry::valid_chat_id(chat) {
            return error("chat id: 1 to 128 of [A-Za-z0-9_.:-]");
        }
        let Some(harness) = harness_env::harness_id(harness) else {
            return error("unknown harness");
        };
        match self.receiver.mint(chat, harness) {
            Some(token) => {
                if let Ok(mut m) = self.minted_at.lock() {
                    let live: HashSet<String> = self
                        .receiver
                        .holders_snapshot()
                        .iter()
                        .map(|(h, _, _)| hex(h))
                        .collect();
                    m.retain(|sha, _| live.contains(sha));
                    m.insert(hex(&Receiver::token_hash(&token)), now_ms());
                }
                self.save_state();
                Reply {
                    token: Some(token),
                    ..ok()
                }
            }
            None => error("could not mint a token"),
        }
    }

    /// Drops a connection's approver and tail registrations when it closes.
    pub fn forget(&self, conn: u64) {
        if let Ok(mut a) = self.approvers.lock() {
            a.retain(|o| o.conn != conn);
        }
        if let Ok(mut t) = self.tails.lock() {
            t.retain(|s| s.out.conn != conn);
        }
    }

    /// Runs retention now: logs past 7 days, screenshots past 3, the 1 GiB
    /// cap (`activity/retention.rs`). The writer also runs it on its first
    /// write each day; this covers a day with no writes.
    pub fn retention_pass(&self, now: i64) -> usize {
        let _guard = self.log.read_lock.lock();
        retention_pass(&self.log_dir, now)
    }
}

/// `cu` (lane K's request 3): computer use's controls, daemon scope only.
/// A chat token may not stop, read or revoke computer use: it is the
/// agent's (case 66). Where computer use does not run (not macOS, or the
/// feature off) the answer says so.
fn op_cu(who: &Who, action: CuAction, chat: Option<&str>, app: Option<&str>) -> Reply {
    if *who != Who::Daemon {
        return error("cu needs daemon scope");
    }
    let need = |v: Option<&str>, what: &str| -> Result<String, String> {
        v.filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("cu {what} is missing"))
    };
    #[cfg(feature = "computer-use")]
    {
        let Some(service) = crate::computer_use::service::running() else {
            return error("computer use is not running here (macOS only, started by the daemon)");
        };
        let report = match action {
            CuAction::Status => serde_json::to_value(service.status()).ok(),
            CuAction::Stop => match need(chat, "chat") {
                Ok(c) => serde_json::to_value(service.stop(&c)).ok(),
                Err(e) => return error(e),
            },
            CuAction::StopAll => {
                service.stop_all();
                Some(json!({"stopped": "all"}))
            }
            CuAction::Revoke => match (need(chat, "chat"), need(app, "app")) {
                (Ok(c), Ok(a)) => Some(json!({"revoked": service.revoke(&c, &a)})),
                (Err(e), _) | (_, Err(e)) => return error(e),
            },
        };
        Reply { report, ..ok() }
    }
    #[cfg(not(feature = "computer-use"))]
    {
        let _ = (action, need(chat, "chat"), app);
        error("computer use is not built into this daemon (feature computer-use off)")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_sha256(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The state file when it is safe to read: a regular file (no link) owned
/// by this user, no group or other bits, at most 1 MiB, JSON with
/// `version: 1`. Anything else is ignored, and the daemon starts empty.
fn read_state(path: &Path) -> Option<Value> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    // Opened without following a link and checked on the handle (review
    // W2-S9: no gap between the check and the read).
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    if !meta.file_type().is_file()
        || meta.uid() != me
        || meta.mode() & 0o077 != 0
        || meta.len() > STATE_FILE_MAX
    {
        return None;
    }
    let mut text = String::new();
    file.by_ref()
        .take(STATE_FILE_MAX + 1)
        .read_to_string(&mut text)
        .ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    (value.get("version").and_then(Value::as_u64) == Some(1)).then_some(value)
}

/// The disk's records and the ring's, as one backlog: one record per chat
/// and id, the last copy where it may replace the first (case 35, the
/// store's `may_replace`), oldest first, at most `MAX_BACKLOG`.
/// What a `run.start` says that `tk status` shows, each string cut: the
/// record is the daemon's own (daemon scope), but its words are `tk run`'s.
fn run_info_of(event: &Event, received: i64) -> Value {
    let cut = |v: Option<&Value>, max: usize| -> Value {
        v.and_then(Value::as_str)
            .map(|s| Value::String(s.chars().take(max).collect()))
            .unwrap_or(Value::Null)
    };
    let flags: Vec<Value> = event
        .detail
        .get("dangerousFlags")
        .and_then(Value::as_array)
        .map(|a| a.iter().take(16).map(|f| cut(Some(f), 64)).collect())
        .unwrap_or_default();
    json!({
        "harness": Value::String(event.harness.chars().take(64).collect()),
        "boundary": cut(event.detail.get("boundary"), 16),
        "since": received,
        "fullAccessFlags": flags,
    })
}

/// A run's info as `chats.json` held it: the same fields, re-cut, nothing
/// else kept (review W2-S9: the file is read, not trusted).
fn run_info_restored(v: &Value) -> Value {
    let cut = |k: &str, max: usize| -> Value {
        v.get(k)
            .and_then(Value::as_str)
            .map(|s| Value::String(s.chars().take(max).collect()))
            .unwrap_or(Value::Null)
    };
    let flags: Vec<Value> = v
        .get("fullAccessFlags")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .take(16)
                .filter_map(Value::as_str)
                .map(|s| Value::String(s.chars().take(64).collect()))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "harness": cut("harness", 64),
        "boundary": cut("boundary", 16),
        "since": v.get("since").and_then(Value::as_i64),
        "fullAccessFlags": flags,
    })
}

pub fn merge_backlog<'a>(
    disk: Vec<Event>,
    ring: impl Iterator<Item = &'a Event>,
    since: i64,
    chat: Option<&str>,
) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    let mut slot: HashMap<(String, String), usize> = HashMap::new();
    let ring = ring.filter(|e| e.at >= since && chat.is_none_or(|c| c == e.chat_id));
    for e in disk.into_iter().chain(ring.cloned()) {
        let key = (e.chat_id.clone(), e.id.clone());
        match slot.get(&key) {
            Some(&i) => {
                if out[i].at <= e.at && store::may_replace(&out[i], &e) {
                    out[i] = e;
                }
            }
            None => {
                slot.insert(key, out.len());
                out.push(e);
            }
        }
    }
    out.sort_by_key(|e| e.at);
    if out.len() > MAX_BACKLOG {
        out.drain(..out.len() - MAX_BACKLOG);
    }
    out
}

fn bump(map: &mut Map<String, Value>, key: &Value) {
    let Some(key) = key.as_str() else { return };
    let n = map.get(key).and_then(Value::as_u64).unwrap_or(0);
    map.insert(key.to_string(), json!(n + 1));
}

/// One retention pass over a log folder. Returns the items deleted.
pub fn retention_pass(dir: &Path, now: i64) -> usize {
    use crate::activity::retention;
    let items = retention::scan(dir);
    retention::apply(&retention::plan(
        &items,
        retention::day_of_ms(now),
        retention::CAP_BYTES,
    ))
}

/// The Mac collector's events for a report, as the fork's panel read them;
/// `None` off macOS. Lane K registers the collector with the daemon in
/// wave 2.
fn collector_events(
    since: i64,
    until: i64,
    chat: Option<&str>,
) -> Option<crate::activity::collector::EventsReply> {
    use crate::activity::collector;
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut reply = collector::fetch_events(since, until, chat, collector::MAX_EVENTS);
    if reply.complete || !reply.events.is_empty() {
        if let Some(problem) = collector::fetch_status_detail()
            .as_ref()
            .and_then(collector::eslogger_problem)
        {
            reply.problem.get_or_insert(problem);
        }
    }
    Some(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::types::ActivitySource;
    use std::sync::mpsc::RecvTimeoutError;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    type Hook = Box<dyn Fn() + Send>;
    static AFTER_OK: Mutex<Option<Hook>> = Mutex::new(None);

    /// Runs where a tail's client first holds its `ok` and can act on it.
    pub(super) fn after_tail_ok() {
        if let Ok(hook) = AFTER_OK.lock() {
            if let Some(hook) = hook.as_ref() {
                hook();
            }
        }
    }

    fn daemon() -> (Arc<Daemon>, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "tkd-unit-{}-{}",
            std::process::id(),
            &tk_core::token::mint().unwrap()[..12]
        ));
        let data = root.join("data");
        let log: &'static ActivityLog =
            Box::leak(Box::new(ActivityLog::start(data.join("activity"))));
        (Daemon::new(TOKEN, log, &data, false).unwrap(), root)
    }

    fn event(id: &str) -> Event {
        Event {
            id: id.into(),
            at: 0,
            chat_id: "chat-a".into(),
            harness: "pi".into(),
            source: ActivitySource::Adapter,
            kind: "tool.call".into(),
            summary: id.into(),
            detail: json!({}),
            pid: None,
            approval_id: None,
        }
    }

    /// BX8's state file is taken only when it is this user's own private
    /// regular file: one others can read or write, or a link, is ignored.
    #[test]
    fn a_state_file_that_is_not_private_is_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "tkd-state-{}-{}",
            std::process::id(),
            &tk_core::token::mint().unwrap()[..12]
        ));
        std::fs::create_dir_all(&root).unwrap();
        let token = tk_core::token::mint().unwrap();
        let state = root.join("chats.json");
        let text = json!({
            "version": 1,
            "tokens": [{
                "sha256": hex(&Receiver::token_hash(&token)),
                "chat": "chat-x", "harness": "pi", "mintedAt": now_ms()
            }],
            "runs": []
        })
        .to_string();
        let start = |path: PathBuf| {
            let log: &'static ActivityLog = Box::leak(Box::new(ActivityLog::start(
                root.join(format!("data-{}", uuid::Uuid::new_v4().simple())),
            )));
            Daemon::new_with_state(TOKEN, log, &root, false, Some(path)).unwrap()
        };
        std::fs::write(&state, &text).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(start(state.clone()).hello(&token), None, "0644");
        let link = root.join("link.json");
        std::fs::write(&state, &text).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&state, &link).unwrap();
        assert_eq!(start(link).hello(&token), None, "a link");
        std::fs::write(&state, &text).unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
        let chat = Some(Who::Chat {
            chat: "chat-x".into(),
            harness: "pi".into(),
        });
        assert_eq!(start(state.clone()).hello(&token), chat, "0600, its own");
        // Review W2-S9: a planted run that names no start time, or pid 1,
        // keeps a started chat's token alive for good; neither counts.
        for run in [
            json!({"chat": "chat-x", "pid": std::process::id(), "startTime": null}),
            json!({"chat": "chat-x", "pid": 1, "startTime": tk_core::peer::start_time(1)}),
        ] {
            let planted = json!({
                "version": 1,
                "tokens": [{
                    "sha256": hex(&Receiver::token_hash(&token)),
                    "chat": "chat-x", "harness": "pi", "mintedAt": 0
                }],
                "runs": [run]
            })
            .to_string();
            std::fs::write(&state, &planted).unwrap();
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(start(state.clone()).hello(&token), None, "{run}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Lane K's request 5 (the CI flake `ids_the_daemon_makes_never_repeat`):
    /// a client that acts on `tail`'s `ok` at once must see what it caused.
    /// On the old order (`ok` before the subscription) an event ingested at
    /// that moment was never pushed.
    #[test]
    fn an_event_right_after_the_tail_ok_reaches_the_tail() {
        let (d, root) = daemon();
        let hooked = d.clone();
        *AFTER_OK.lock().unwrap() = Some(Box::new(move || {
            hooked.ingest(event("right-after-ok"));
        }));
        let (out, rx, _) = Outbox::new(d.next_conn());
        d.op_tail(&Who::Daemon, None, None, &out);
        *AFTER_OK.lock().unwrap() = None;
        let first: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(first["ok"], true, "{first}");
        let pushed = match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("the event after ok was never pushed"),
            Err(e) => panic!("{e}"),
        };
        assert!(pushed.contains("right-after-ok"), "{pushed}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Integration w2 review W2-S5: a process inside a chat forks a child
    /// that connects and exits; without a pidfd the dead pid has no
    /// ancestors to walk, so the daemon must refuse a peer whose pid no
    /// longer resolves to the process read at accept.
    #[test]
    fn an_approver_whose_process_is_gone_or_reused_is_refused() {
        let (d, root) = daemon();
        let me = tk_core::peer::Peer {
            pid: None,
            uid: unsafe { libc::geteuid() },
        };
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let started = tk_core::peer::start_time(pid);
        assert!(started.is_some());
        child.kill().unwrap();
        child.wait().unwrap();
        let gone = ConnPeer {
            cred: Some(tk_core::peer::Peer {
                pid: Some(pid),
                ..me.clone()
            }),
            pidfd: None,
            program: None,
            start_time: started,
        };
        let why = d.approver_refusal(&gone).expect("a reaped peer is refused");
        assert!(why.contains("gone"), "{why}");
        let own = std::process::id();
        let reused = ConnPeer {
            cred: Some(tk_core::peer::Peer {
                pid: Some(own),
                ..me.clone()
            }),
            pidfd: None,
            program: None,
            start_time: tk_core::peer::start_time(own).map(|t| t + 1),
        };
        let why = d
            .approver_refusal(&reused)
            .expect("another process under the pid is refused");
        assert!(why.contains("not the process"), "{why}");
        let unread = ConnPeer {
            cred: Some(tk_core::peer::Peer {
                pid: Some(own),
                ..me
            }),
            pidfd: None,
            program: None,
            start_time: None,
        };
        assert!(d.approver_refusal(&unread).is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}

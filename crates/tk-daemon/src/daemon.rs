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
    Answer, AnsweredBy, ApprovalRequest, DaemonInfo, Push, Reply, Request, Scope, PROTOCOL_VERSION,
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
}

impl Outbox {
    pub fn new(conn: u64) -> (Self, Rx<String>, Arc<AtomicUsize>) {
        let (tx, rx) = mpsc::channel();
        let pending = Arc::new(AtomicUsize::new(0));
        (
            Outbox {
                tx,
                pending: pending.clone(),
                conn,
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
    answer: SyncSender<Answer>,
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
    pending: Mutex<HashMap<String, Pending>>,
    chats: Mutex<HashMap<String, ChatContext>>,
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
        let receiver = Receiver::start(settings, sink)?;
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
            next_conn: AtomicU64::new(1),
            counters: Counters::default(),
        });
        if let Ok(mut c) = cell.lock() {
            *c = Some(Arc::downgrade(&daemon));
        }
        Ok(daemon)
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
                } else if let Ok(mut approvers) = self.approvers.lock() {
                    approvers.retain(|a| a.conn != out.conn);
                    approvers.push(out.clone());
                    out.send(&ok());
                }
            }
            Request::Answer { id, answer } => {
                out.send(&self.op_answer(out.conn, &id, answer));
            }
            Request::TokenMint { chat, harness } => {
                out.send(&self.op_mint(who, &chat, &harness));
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
                }
            }
        }
        if store::bound_event(event.clone()).id.is_empty() {
            return error("event id is empty");
        }
        let id = self.ingest(event);
        Reply {
            id: Some(id),
            ..ok()
        }
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
        out.send(&ok());
        // The log on disk first, outside the lock; then, under it, the ring
        // (which holds anything not yet on disk) and the subscription.
        let mut backlog: Vec<Event> = match since {
            Some(since) => {
                store::read(&self.log_dir, since, i64::MAX, chat.as_deref(), MAX_BACKLOG).events
            }
            None => Vec::new(),
        };
        let Ok(ring) = self.order.lock() else { return };
        if let Some(since) = since {
            backlog = merge_backlog(backlog, ring.iter(), since, chat.as_deref());
        }
        for event in backlog {
            out.send(&Push::Event(event));
        }
        if let Ok(mut tails) = self.tails.lock() {
            tails.push(TailSub {
                chat,
                out: out.clone(),
            });
        }
        drop(ring);
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
        Reply {
            report: Some(self.report(chat.as_deref(), since, until)),
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
        self.approval_event(&record_id, &request, None, None);
        let (answer, by) = self.wait_for_answer(&request);
        self.approval_event(&record_id, &request, Some(answer), Some(by));
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
    fn wait_for_answer(&self, request: &ApprovalRequest) -> (Answer, AnsweredBy) {
        let (tx, rx) = mpsc::sync_channel::<Answer>(1);
        {
            let Ok(mut pending) = self.pending.lock() else {
                return (Answer::No, AnsweredBy::None);
            };
            let this_chat = pending.values().filter(|p| p.chat == request.chat).count();
            if pending.contains_key(&request.id)
                || pending.len() >= MAX_PENDING_APPROVALS
                || this_chat >= MAX_PENDING_PER_CHAT
            {
                return (Answer::No, AnsweredBy::None);
            }
            pending.insert(
                request.id.clone(),
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
            (Answer::No, AnsweredBy::None)
        } else {
            match rx.recv_timeout(Duration::from_secs(u64::from(request.timeout_seconds))) {
                Ok(answer) => (answer, AnsweredBy::Approver),
                Err(_) => (Answer::No, AnsweredBy::Timeout),
            }
        };
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&request.id);
        }
        outcome
    }

    fn op_answer(&self, conn: u64, id: &str, answer: Answer) -> Reply {
        let is_approver = self
            .approvers
            .lock()
            .map(|a| a.iter().any(|o| o.conn == conn))
            .unwrap_or(false);
        if !is_approver {
            return error("not an approver");
        }
        let Ok(pending) = self.pending.lock() else {
            return error("busy");
        };
        match pending.get(id) {
            // The channel holds one answer: the first wins, the rest fail
            // to send and are told nothing more.
            Some(p) => {
                let _ = p.answer.try_send(answer);
                ok()
            }
            None => error("no such request, or it was answered or timed out"),
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
            Some(token) => Reply {
                token: Some(token),
                ..ok()
            },
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

/// The disk's records and the ring's, as one backlog: one record per chat
/// and id, the last copy where it may replace the first (case 35, the
/// store's `may_replace`), oldest first, at most `MAX_BACKLOG`.
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

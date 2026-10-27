//! B5: the check. Each logged action is set against the approval that
//! covered it at that time; an action nothing covered is a flag.
//!
//! The pattern is Fenceline's (note X, section 3.1): what the agent was
//! allowed (here, MonoCode's own approvals record, which the agent cannot
//! write) against what the OS saw. Detection only: a flag is shown, nothing
//! is blocked.
//!
//! Flags:
//! - `appNotGranted`: computer use acted in an app no grant covered then.
//! - `writeOutsideFolders`: a file created, written, renamed or deleted
//!   outside the chat's folders, and outside any folder approval.
//! - `leftRunning`: a process a tool call started that was still running
//!   when the call ended (Fenceline: `cmd &` and `nohup` slip past monitors).
//! - `sensitivePath`: a sensitive file opened (the collector keeps only
//!   those opens).
//! - `newHost`: a host never seen before and not approved.
//! - `recordingOff`: computer use's recording did not start.
//!
//! Collector events carry a tier (`root` or `child`); a child is a tool's
//! when it started inside a tool call or descends from one, else the
//! harness's own. Known harness behaviour of `root` and harness children
//! (keychain reads, a credential's owner reading it, shell init files) is
//! counted as expected, never a tool's. Repeats are one row with a count.
//! The collector's log stays complete: all of this happens here.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{ActivityEvent, ActivitySource, ApprovalRecord, ApprovalScope, ApprovalSubject};

/// A "once" approval covers its action, which follows within this window.
pub const ONCE_WINDOW_MS: i64 = 10 * 60 * 1000;
/// A process still running this long after its tool call ended is flagged.
pub const LEFT_RUNNING_GRACE_MS: i64 = 10_000;
/// Events shown around a flag.
pub const CONTEXT_WINDOW_MS: i64 = 30_000;
pub const MAX_CONTEXT: usize = 20;
/// A screenshot taken this close to the flagged action is shown with it.
pub const SCREENSHOT_WINDOW_MS: i64 = 10_000;

/// What the check knows about a chat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    pub chat_id: String,
    pub harness: String,
    /// Absolute folders the chat may write in: its working folder first.
    pub folders: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FlagReason {
    AppNotGranted,
    WriteOutsideFolders,
    LeftRunning,
    SensitivePath,
    NewHost,
    /// Computer use's recording did not start (`cua.recording`, `on` false).
    RecordingOff,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flag {
    /// The flagged event's id, prefixed with the reason.
    pub id: String,
    pub chat_id: String,
    pub at: i64,
    pub reason: FlagReason,
    /// One line, plain text.
    pub summary: String,
    pub event_id: String,
    /// Ids of the events around it, same chat, oldest first.
    pub context: Vec<String>,
    /// A screenshot path from computer use's record near the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// Events this row stands for (the same kind, process and file within
    /// 15 minutes).
    #[serde(default = "one")]
    pub count: u32,
    /// The last of them; `at` is the first.
    #[serde(default)]
    pub last_at: i64,
    /// Where in the chat's process tree it came from (collector events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
}

fn one() -> u32 {
    1
}

pub struct CheckInput<'a> {
    pub events: &'a [ActivityEvent],
    pub approvals: &'a [ApprovalRecord],
    pub chats: &'a HashMap<String, ChatContext>,
    /// Hosts seen before this window, lowercased.
    pub known_hosts: &'a HashSet<String>,
    /// The time of the check, for "still running".
    pub now_ms: i64,
    /// The app's data folder (`app_data_dir`), guarded but for a harness
    /// home's session state.
    pub data_dir: Option<&'a str>,
}

fn str_field<'a>(event: &'a ActivityEvent, key: &str) -> Option<&'a str> {
    event.detail.get(key).and_then(Value::as_str)
}

/// Whether an approval held at `at`.
pub fn holds_at(record: &ApprovalRecord, at: i64) -> bool {
    if !record.granted || at < record.granted_at {
        return false;
    }
    let end = match (record.ended_at, record.scope) {
        (Some(end), _) => end,
        (None, ApprovalScope::Once) => record.granted_at.saturating_add(ONCE_WINDOW_MS),
        (None, _) => i64::MAX,
    };
    at <= end
}

fn covering<'a>(
    approvals: &'a [ApprovalRecord],
    chat_id: &str,
    subject: ApprovalSubject,
    at: i64,
    matches: impl Fn(&str) -> bool,
) -> Option<&'a ApprovalRecord> {
    approvals.iter().find(|record| {
        // A project's site rule is kept per folder, not per chat, so it is
        // recorded with no chat and covers a host in every chat.
        let chat_matches = record.chat_id == chat_id
            || (subject == ApprovalSubject::Host && record.chat_id.is_empty());
        record.subject == subject && chat_matches && holds_at(record, at) && matches(&record.target)
    })
}

/// True when `path` is the folder or below it, read literally.
pub fn inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() || !path.starts_with('/') || path.split('/').any(|p| p == "..") {
        return false;
    }
    path == folder || path.starts_with(&format!("{folder}/"))
}

/// The user's home folder from a path, `/Users/<name>` or `/home/<name>`.
fn home_of(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    let (Some(""), Some(root), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    ((root == "Users" || root == "home") && !name.is_empty()).then(|| format!("/{root}/{name}"))
}

/// The app's data folder, compared without case: APFS is case-insensitive,
/// so `Activity/` is the same folder (security review T2). It comes from
/// the running app (`app_data_dir`), so a dev build guards its own
/// `com.monocode.desktop.dev` folder (decision 1).
fn data_sub(data_dir: Option<&str>, sub: &str) -> Option<String> {
    let base = data_dir?.trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/{sub}").to_ascii_lowercase())
}

/// Session and log state a harness keeps in the homes MonoCode gives it in
/// the data folder, the same kind `~/.claude` and `~/.codex` hold as noise.
/// Claude Code's (`CLAUDE_CONFIG_DIR`, harness.rs:1426): `projects`,
/// `shell-snapshots`, `session-env`, `file-history`, `todos`, `statsig`,
/// `debug` (Claude Code 2.1.304 strings). Codex's (`CODEX_HOME`,
/// harness.rs:1168, :1433): `codex_mono_store.rs` STATE_NAMES less
/// `memories` and `backups`, which are not caches. Settings, config,
/// credentials and `.claude.json` stay guarded.
const HARNESS_HOME_NOISE: &[&str] = &[
    "projects",
    "file-history",
    "todos",
    "statsig",
    "debug",
    "sessions",
    "archived_sessions",
    "session_index.jsonl",
    "history.jsonl",
    "log",
    "logs",
    "tmp",
];

/// Folders inside a harness home whose files the harness runs as code:
/// Claude Code runs a shell-snapshots file and the session-env scripts
/// before each Bash command (Claude Code 2.1.304 strings), and Codex sources
/// its shell_snapshots file (codex-rs/core/src/shell_snapshot.rs:1010). A
/// write there is never noise, in `~/.claude`, `~/.codex` or the data
/// folder's harness homes (review Y1).
const HARNESS_RUNS: &[&str] = &["shell-snapshots", "session-env", "shell_snapshots"];

/// Whether `path` sits in a folder of `HARNESS_RUNS` under the harness home
/// that starts at `home_dir`.
fn harness_runs(path: &str, home_dir: &str) -> bool {
    path.to_ascii_lowercase()
        .strip_prefix(&format!("{}/", home_dir.to_ascii_lowercase()))
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|first| HARNESS_RUNS.contains(&first))
}

/// The only writes in the data folder that are noise: a harness home's
/// session state, `provider-accounts/<provider>/<id>/<state>` or
/// `codex-monos/<id>/<state>` (review X6).
fn data_noise(path: &str, data_dir: Option<&str>) -> bool {
    let Some(base) = data_sub(data_dir, "") else {
        return false;
    };
    let lower = path.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix(base.as_str()) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return false;
    }
    let state = match parts.as_slice() {
        ["provider-accounts", _, _, state, ..] => state,
        ["codex-monos", _, state, ..] => state,
        _ => return false,
    };
    HARNESS_HOME_NOISE.contains(state)
}

/// Everything in MonoCode's data folder but a harness home's session state:
/// the log, computer use's recordings, the OTel helper script Claude Code
/// runs, the provider accounts' settings, the database (review X6).
fn guarded_path(path: &str, data_dir: Option<&str>) -> bool {
    let Some(data) = data_dir.map(|d| d.trim_end_matches('/').to_ascii_lowercase()) else {
        return false;
    };
    !data.is_empty() && inside(&path.to_ascii_lowercase(), &data) && !data_noise(path, data_dir)
}

/// Paths a write never flags: scratch space, devices, and the harnesses' own
/// state (Fenceline's filter for Claude Code's `~/.claude`).
pub fn noise_path(path: &str, granted_app: bool, data_dir: Option<&str>) -> bool {
    const SYSTEM: &[&str] = &[
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
        "/dev",
    ];
    if SYSTEM.iter().any(|prefix| inside(path, prefix)) {
        return true;
    }
    // MonoCode's data folder holds the log, computer use's recordings, a
    // script Claude Code runs and the provider accounts. A chat's process
    // writing there is the tampering the log exists to catch (security
    // review S5, X6); only a harness home's session state is noise. The
    // app's own writes are never attributed to a chat, so they raise nothing.
    if data_noise(path, data_dir) {
        return true;
    }
    if guarded_path(path, data_dir) {
        return false;
    }
    let Some(home) = home_of(path) else {
        return false;
    };
    const HARNESS: &[&str] = &[
        ".claude",
        ".codex",
        ".cache",
        ".npm",
        ".cargo/registry",
        "Library/Caches",
        "Library/Logs",
    ];
    if HARNESS.iter().any(|rel| {
        let dir = format!("{home}/{rel}");
        inside(path, &dir) && !harness_runs(path, &dir)
    }) {
        return true;
    }
    if path.starts_with(&format!("{home}/.claude.json")) {
        return true;
    }
    // An app working under a grant writes its own state; that is the app,
    // not something the agent chose to touch.
    const APP_STATE: &[&str] = &[
        "Library/Containers",
        "Library/Preferences",
        "Library/Saved Application State",
        "Library/Group Containers",
        "Library/HTTPStorages",
        "Library/WebKit",
    ];
    granted_app
        && APP_STATE
            .iter()
            .any(|rel| inside(path, &format!("{home}/{rel}")))
}

/// A project's own `.env` inside the chat's folders is expected reading.
fn expected_sensitive(path: &str, folders: &[String]) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    let env_file = name == ".env" || name.starts_with(".env.");
    env_file && folders.iter().any(|f| inside(path, f))
}

fn host_matches(target: &str, host: &str) -> bool {
    let target = target.trim().to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    if let Some(suffix) = target.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    target == host
}

/// The host an event reached: its own `host`, or the one its OTel detail
/// named (`detail.otel.host`, lane A's feed keeps it there).
pub fn event_host(event: &ActivityEvent) -> Option<&str> {
    str_field(event, "host").or_else(|| {
        event
            .detail
            .get("otel")
            .and_then(|otel| otel.get("host"))
            .and_then(Value::as_str)
    })
}

/// Whether a path is inside computer use's recording folder, with no `..`
/// (review U6).
fn in_recordings(path: &str, data_dir: Option<&str>) -> bool {
    let Some(folder) = data_sub(data_dir, "computer-use/recordings") else {
        return false;
    };
    !path.split('/').any(|part| part == "..") && inside(&path.to_ascii_lowercase(), &folder)
}

/// One computer-use runtime: its pid and the time span it ran.
struct RuntimeSpan {
    pid: u32,
    started: i64,
    ended: Option<i64>,
}

/// The computer-use runtimes each chat ran. A runtime ends at the first of:
/// its own stop record, the chat's next runtime start, or the first exit of
/// its pid after its start. A write counts as the runtime's only inside
/// that span, so a reused pid is not exempted (reviews U5, V2, V3).
fn runtime_spans(events: &[ActivityEvent]) -> HashMap<String, Vec<RuntimeSpan>> {
    let mut starts: HashMap<String, Vec<(i64, u32)>> = HashMap::new();
    let mut stops: HashMap<(String, u32), Vec<i64>> = HashMap::new();
    let mut exits: HashMap<u32, Vec<i64>> = HashMap::new();
    for e in events {
        let Some(pid) = e.pid else { continue };
        match (&e.source, e.kind.as_str()) {
            (ActivitySource::Cua, "cua.runtime") => starts
                .entry(e.chat_id.clone())
                .or_default()
                .push((e.at, pid)),
            (ActivitySource::Cua, "cua.runtime.stop") => stops
                .entry((e.chat_id.clone(), pid))
                .or_default()
                .push(e.at),
            (ActivitySource::Eslogger, "exit") => exits.entry(pid).or_default().push(e.at),
            _ => {}
        }
    }
    let first_from = |list: Option<&Vec<i64>>, from: i64| {
        list.and_then(|times| times.iter().copied().filter(|&t| t >= from).min())
    };
    let mut spans = HashMap::new();
    for (chat, mut list) in starts {
        list.sort();
        list.dedup();
        let mut out = Vec::new();
        for (i, &(started, pid)) in list.iter().enumerate() {
            let next = list.get(i + 1).map(|&(at, _)| at);
            let stop = first_from(stops.get(&(chat.clone(), pid)), started);
            let exit = first_from(exits.get(&pid), started);
            let ended = [next, stop, exit].into_iter().flatten().min();
            out.push(RuntimeSpan {
                pid,
                started,
                ended,
            });
        }
        spans.insert(chat, out);
    }
    spans
}

/// One tool call: from its record's `at` to `detail.endedAt`. A call with
/// no end is still running and open-ended (Mac test 3.8: the old windows
/// had zero width).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolWindow {
    pub start: i64,
    pub end: Option<i64>,
    /// `detail.background`: work the agent left running on purpose.
    pub background: bool,
}

impl ToolWindow {
    fn holds(&self, at: i64) -> bool {
        at >= self.start && self.end.is_none_or(|end| at <= end)
    }
}

fn int_field(event: &ActivityEvent, key: &str) -> Option<i64> {
    event.detail.get(key).and_then(Value::as_i64)
}

fn pid_field(event: &ActivityEvent, key: &str) -> Option<u32> {
    event
        .detail
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

/// Tool call windows per chat, from the adapter's and OTel's `tool.*`
/// records joined by call id.
pub fn tool_windows(events: &[ActivityEvent]) -> HashMap<String, Vec<ToolWindow>> {
    let mut calls: HashMap<(String, String), ToolWindow> = HashMap::new();
    for event in events {
        if !matches!(event.source, ActivitySource::Adapter | ActivitySource::Otel)
            || !event.kind.starts_with("tool.")
        {
            continue;
        }
        let Some(call) = str_field(event, "callId") else {
            continue;
        };
        let ended = int_field(event, "endedAt");
        let background = event.detail.get("background").and_then(Value::as_bool) == Some(true);
        let window = calls
            .entry((event.chat_id.clone(), call.to_string()))
            .or_insert(ToolWindow {
                start: event.at,
                end: None,
                background: false,
            });
        window.start = window.start.min(event.at);
        if let Some(end) = ended {
            window.end = Some(window.end.map_or(end, |e| e.max(end)));
        }
        window.background |= background;
    }
    let mut out: HashMap<String, Vec<ToolWindow>> = HashMap::new();
    for ((chat, _), window) in calls {
        out.entry(chat).or_default().push(window);
    }
    out
}

/// One turn of a chat, from the adapter feed's `turn.start` to its
/// `turn.end`; `None` is open to now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TurnSpan {
    start: i64,
    end: Option<i64>,
}

/// Turn spans per chat. A start with no end is open to now; an end whose
/// start fell before the window opens at the window's start; a start while
/// a turn is open keeps it open. A record marked `reattached` (a turn taken
/// back after a reload) opens a span too. Only the adapter writes these
/// (`src/features/harness-telemetry/model/telemetryFeed.ts`, `turn.started`
/// and `turn.completed`).
fn turn_spans(events: &[ActivityEvent]) -> HashMap<String, Vec<TurnSpan>> {
    let mut out: HashMap<String, Vec<TurnSpan>> = HashMap::new();
    let mut list: Vec<&ActivityEvent> = events
        .iter()
        .filter(|e| e.source == ActivitySource::Adapter && e.kind.starts_with("turn."))
        .collect();
    list.sort_by_key(|e| e.at);
    for e in list {
        let spans = out.entry(e.chat_id.clone()).or_default();
        let open = spans.last().is_some_and(|s| s.end.is_none());
        let reattached = e.detail.get("reattached").and_then(Value::as_bool) == Some(true);
        match e.kind.as_str() {
            "turn.end" if open => {
                if let Some(last) = spans.last_mut() {
                    last.end = Some(e.at);
                }
            }
            "turn.end" => spans.push(TurnSpan {
                start: i64::MIN,
                end: Some(e.at),
            }),
            _ if open => {}
            "turn.start" => spans.push(TurnSpan {
                start: e.at,
                end: None,
            }),
            _ if reattached => spans.push(TurnSpan {
                start: e.at,
                end: None,
            }),
            _ => {}
        }
    }
    out
}

/// Whether `at` falls in a turn of the chat. A chat with no turn records in
/// the window counts as in a turn throughout: nothing at the harness-child
/// tier is hidden on a guess (review X4).
fn in_turn(spans: &HashMap<String, Vec<TurnSpan>>, chat: &str, at: i64) -> bool {
    match spans.get(chat) {
        None => true,
        Some(list) => list
            .iter()
            .any(|s| at >= s.start && s.end.is_none_or(|end| at <= end)),
    }
}

/// Where in a chat's process tree an event came from. The collector says
/// `root` or `child`; the check splits `child` by tool windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    /// The registered harness process itself.
    Root,
    /// A descendant started outside every tool call: the harness's own
    /// helpers, MCP servers, its shell snapshot.
    HarnessChild,
    /// Started inside a tool call, or a descendant of such a process. Never
    /// suppressed.
    Tool,
}

/// The fork that made each child, per chat: (time, parent pid, parent's
/// collector tier).
type Forks = HashMap<(String, u32), Vec<(i64, u32, bool)>>;

struct Tree<'a> {
    forks: Forks,
    first_seen: HashMap<(String, u32), i64>,
    /// Lineage fields seen per pid: ppid, originalPpid, groupId.
    lineage: HashMap<(String, u32), Vec<u32>>,
    windows: &'a HashMap<String, Vec<ToolWindow>>,
}

impl<'a> Tree<'a> {
    fn new(events: &[ActivityEvent], windows: &'a HashMap<String, Vec<ToolWindow>>) -> Tree<'a> {
        let mut forks: Forks = HashMap::new();
        let mut first_seen: HashMap<(String, u32), i64> = HashMap::new();
        let mut lineage: HashMap<(String, u32), Vec<u32>> = HashMap::new();
        for e in events
            .iter()
            .filter(|e| e.source == ActivitySource::Eslogger)
        {
            let Some(pid) = e.pid else { continue };
            let key = (e.chat_id.clone(), pid);
            first_seen
                .entry(key.clone())
                .and_modify(|t| *t = (*t).min(e.at))
                .or_insert(e.at);
            for field in ["ppid", "originalPpid", "groupId"] {
                if let Some(other) = pid_field(e, field) {
                    let list = lineage.entry(key.clone()).or_default();
                    if other > 1 && other != pid && !list.contains(&other) && list.len() < 8 {
                        list.push(other);
                    }
                }
            }
            if e.kind == "fork" {
                if let Some(child) = pid_field(e, "child") {
                    let root = str_field(e, "tier") == Some("root");
                    forks
                        .entry((e.chat_id.clone(), child))
                        .or_default()
                        .push((e.at, pid, root));
                }
            }
        }
        Tree {
            forks,
            first_seen,
            lineage,
            windows,
        }
    }

    /// Whether the registered harness itself forked `pid` (its last fork
    /// before `at`). No fork seen means no.
    fn forked_by_root(&self, chat: &str, pid: u32, at: i64) -> bool {
        self.forks
            .get(&(chat.to_string(), pid))
            .and_then(|list| {
                list.iter()
                    .filter(|(t, _, _)| *t <= at)
                    .max_by_key(|(t, _, _)| *t)
            })
            .is_some_and(|(_, _, root)| *root)
    }

    fn in_window(&self, chat: &str, at: i64) -> bool {
        self.windows
            .get(chat)
            .is_some_and(|list| list.iter().any(|w| w.holds(at)))
    }

    /// The fork that made `pid`, the latest at or before `at`.
    fn fork_of(&self, chat: &str, pid: u32, at: i64) -> Option<(i64, u32, bool)> {
        self.forks
            .get(&(chat.to_string(), pid))?
            .iter()
            .copied()
            .filter(|(t, _, _)| *t <= at)
            .max_by_key(|(t, _, _)| *t)
    }

    /// Whether a non-root process belongs to a tool call: started inside a
    /// window, or a descendant of one. A process whose fork was not seen is
    /// placed by its lineage (`ppid`, `originalPpid`, `groupId`), then by
    /// when it was first seen.
    fn is_tool(&self, chat: &str, pid: u32, at: i64, depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        if let Some((started, parent, parent_root)) = self.fork_of(chat, pid, at) {
            if self.in_window(chat, started) {
                return true;
            }
            return !parent_root && self.is_tool(chat, parent, started, depth + 1);
        }
        let key = (chat.to_string(), pid);
        if let Some(parents) = self.lineage.get(&key) {
            if parents.iter().any(|&p| {
                self.first_seen.contains_key(&(chat.to_string(), p))
                    && self.is_tool(chat, p, at, depth + 1)
            }) {
                return true;
            }
        }
        self.first_seen
            .get(&key)
            .is_some_and(|&first| self.in_window(chat, first))
    }

    fn tier(&self, event: &ActivityEvent) -> Tier {
        match (str_field(event, "tier"), event.pid) {
            (Some("root"), _) => Tier::Root,
            (Some("child"), Some(pid)) => {
                if self.is_tool(&event.chat_id, pid, event.at, 0) {
                    Tier::Tool
                } else {
                    Tier::HarnessChild
                }
            }
            // An event from an older collector carries no tier: treated as
            // a tool's, so nothing about it is hidden.
            _ => Tier::Tool,
        }
    }
}

/// Why an event that would flag is expected instead. Counted per rule.
pub const RULE_HARNESS_READ: &str = "harnessRead";
pub const RULE_CREDENTIAL_OWNER: &str = "credentialOwner";
pub const RULE_SHELL_INIT: &str = "shellInit";

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Docker's own binaries, by where Docker Desktop and Homebrew install
/// them, never by name alone: a `docker` from any other folder is not the
/// credential's owner (review X5).
fn docker_owner(exe: &str) -> bool {
    // Absolute, with no empty, `.` or `..` part after the leading `/`.
    let mut parts = exe.split('/');
    if parts.next() != Some("") || parts.any(|p| p.is_empty() || p == "." || p == "..") {
        return false;
    }
    for dir in ["/usr/local/bin/", "/opt/homebrew/bin/"] {
        if let Some(name) = exe.strip_prefix(dir) {
            return name == "docker"
                || name
                    .strip_prefix("docker-credential-")
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
        }
    }
    exe.strip_prefix("/Applications/Docker.app/Contents/")
        .is_some_and(|rest| !rest.is_empty())
}

/// Files a shell reads at every start; a write to one is never hidden.
const SHELL_RC: &[&str] = &[
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".zlogout",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
];

/// A write no rule may hide: a shell rc file, or a LaunchAgents or
/// LaunchDaemons folder (persistence).
fn never_hidden_write(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower
        .split('/')
        .any(|part| part == "launchagents" || part == "launchdaemons")
    {
        return true;
    }
    home_of(path).is_some_and(|home| SHELL_RC.iter().any(|rc| path == format!("{home}/{rc}")))
}

/// A write a rule may hide only inside the home's own config (a dot entry
/// at the top of home) or the chat's folders.
fn hideable_write_place(path: &str, folders: &[String]) -> bool {
    if folders.iter().any(|f| inside(path, f)) {
        return true;
    }
    let Some(home) = home_of(path) else {
        return false;
    };
    path.strip_prefix(&format!("{home}/"))
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|first| first.starts_with('.') && first.len() > 1)
        && !path.split('/').any(|p| p == "..")
}

/// The rule that makes a flag expected, or `None`. Only for `root` and
/// `harness-child` events; never for a tool's (Mac test 3.7, 3.9, 3.11).
/// A harness child counts only outside every turn: the tier rests on tool
/// call records from the webview, and a missing or late one (a reload, a
/// re-attached turn) leaves a tool's processes looking like the harness's
/// (review X4).
fn expected_rule(
    event: &ActivityEvent,
    tier: Tier,
    reason: FlagReason,
    harness: &str,
    folders: &[String],
    forked_by_root: bool,
    during_turn: bool,
) -> Option<&'static str> {
    if tier == Tier::Tool || (tier == Tier::HarnessChild && during_turn) {
        return None;
    }
    let exe = str_field(event, "exe").unwrap_or("");
    let path = str_field(event, "path")?;
    let home = home_of(path);
    match reason {
        FlagReason::SensitivePath => {
            // Claude Code keeps its login in the Keychain
            // (https://code.claude.com/docs/en/iam) and reads both files at
            // start and every 32 s; itself or through /usr/bin/security.
            let claude_keychain = harness == "claude"
                && (path == "/Library/Keychains/System.keychain"
                    || home.as_deref().is_some_and(|h| {
                        path == format!("{h}/Library/Keychains/login.keychain-db")
                    }))
                // `security` only when the harness ran it itself: a tool
                // whose call record is missing lands in the harness-child
                // tier, and its `security` must still flag.
                && (tier == Tier::Root || (exe == "/usr/bin/security" && forked_by_root));
            let codex_auth = harness == "codex"
                && tier == Tier::Root
                && home
                    .as_deref()
                    .is_some_and(|h| path == format!("{h}/.codex/auth.json"));
            if claude_keychain || codex_auth {
                return Some(RULE_HARNESS_READ);
            }
            // The program that owns a credential reading it (secure-agent's
            // defaults.yaml: `programs: ["docker", "docker-credential-*",
            // "com.docker.*"]` for ~/.docker).
            if docker_owner(exe)
                && home
                    .as_deref()
                    .is_some_and(|h| path == format!("{h}/.docker/config.json"))
            {
                return Some(RULE_CREDENTIAL_OWNER);
            }
            None
        }
        FlagReason::WriteOutsideFolders => {
            // compinit rewrites ~/.zcompdump* and the shell appends its
            // history (https://zsh.sourceforge.io/Doc/Release/Completion-System.html).
            if exe != "/bin/zsh" && exe != "/bin/bash" {
                return None;
            }
            let home = home?;
            let shell_file = |p: &str| {
                let Some(rest) = p.strip_prefix(&format!("{home}/")) else {
                    return false;
                };
                !rest.contains('/') && (rest.starts_with(".zcompdump") || rest == ".zsh_history")
            };
            let paths: Vec<&str> = [str_field(event, "path"), str_field(event, "dest")]
                .into_iter()
                .flatten()
                .collect();
            let all = paths.iter().all(|p| {
                shell_file(p) && !never_hidden_write(p) && hideable_write_place(p, folders)
            });
            all.then_some(RULE_SHELL_INIT)
        }
        _ => None,
    }
}

/// Flags of one kind, for one process, on one file, within this long, are
/// one row with a count.
pub const DEDUPE_WINDOW_MS: i64 = 15 * 60 * 1000;
/// At most this many expected rows come back for the toggle.
pub const MAX_EXPECTED_ROWS: usize = 200;

/// Dot folders at the top of home that hold credentials, keys or code
/// that runs: never grouped, each write its own row (review X3).
const NOT_APP_STATE: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".config/gh",
    ".config/fish",
    ".config/git",
    ".docker",
    ".kube",
    ".netrc",
    ".local",
    ".oh-my-zsh",
    ".cargo",
];

/// The folder a harness helper's writes are grouped by, when the path sits
/// inside a folder that is plainly an app's own state: a dot folder at the
/// top of home (an MCP server's `~/.claude-server-commander/`), or one
/// app's folder in `~/.config`. `None` for anything else: a file at the top
/// of home, anything under `Library/`, a credential folder, a `bin` folder,
/// a write no rule may hide, a sensitive file. Those always get their own
/// row (review X3: a LaunchAgent folded into an earlier Preferences row).
fn group_folder(path: &str) -> Option<String> {
    let home = home_of(path)?;
    let rest = path.strip_prefix(&format!("{home}/"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        || parts.iter().any(|p| p.eq_ignore_ascii_case("bin"))
        || never_hidden_write(path)
        || sensitive_file(path)
    {
        return None;
    }
    let first = parts[0];
    if !first.starts_with('.') || first.len() < 2 {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    if NOT_APP_STATE
        .iter()
        .any(|rel| lower == *rel || lower.starts_with(&format!("{rel}/")))
        || SHELL_RC.iter().any(|rc| first.eq_ignore_ascii_case(rc))
    {
        return None;
    }
    if first == ".config" {
        // One row per app, and never a file directly in ~/.config.
        return (parts.len() >= 3).then(|| format!("{home}/.config/{}", parts[1]));
    }
    Some(format!("{home}/{first}"))
}

/// The files the collector logs opens of (`macos/activity-collector/src/
/// filter.rs:34-48`) that sit outside `Library/` and the folders above,
/// plus any `.env` file.
fn sensitive_file(path: &str) -> bool {
    let name = basename(path);
    name == ".env" || name.starts_with(".env.") || name.eq_ignore_ascii_case(".netrc")
}

/// The written path a write flag is about (a copy's or link's destination).
fn written_paths(event: &ActivityEvent) -> Vec<&str> {
    let path = str_field(event, "path");
    let dest = str_field(event, "dest");
    match event.kind.as_str() {
        // The source of a link, copy or clone is read, not written.
        "link" | "copyfile" | "clone" => dest.into_iter().collect(),
        _ => [path, dest].into_iter().flatten().collect(),
    }
}

fn one_line(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect()
}

/// What the check returns: the flags, the expected events it hid, and how
/// many each rule hid.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckOutput {
    pub flags: Vec<Flag>,
    /// Hidden rows, deduped like flags, for the panel's toggle.
    pub expected: Vec<Flag>,
    /// Events hidden per rule.
    pub suppressed: BTreeMap<String, u64>,
}

struct Candidate<'e> {
    event: &'e ActivityEvent,
    reason: FlagReason,
    summary: String,
    tier: Option<Tier>,
    /// The path or folder the row is about, for grouping.
    subject: String,
    grouped_path: Option<String>,
    /// Never merged with another: a write to the log or the recordings
    /// (each tampering attempt is its own row).
    alone: bool,
}

/// Collapses candidates on (chat, reason, tier, exe, subject) within 15
/// minutes of a row's first event.
fn dedupe(events: &[ActivityEvent], mut found: Vec<Candidate>) -> Vec<Flag> {
    found.sort_by_key(|c| c.event.at);
    type Key = (String, FlagReason, Option<Tier>, String, String);
    let mut open: HashMap<Key, usize> = HashMap::new();
    let mut rows: Vec<Flag> = Vec::new();
    let mut paths: Vec<HashSet<String>> = Vec::new();
    for c in found {
        let exe = str_field(c.event, "exe").unwrap_or("").to_string();
        let key: Key = (
            c.event.chat_id.clone(),
            c.reason,
            c.tier,
            exe,
            c.subject.clone(),
        );
        if let Some(&i) = open.get(&key).filter(|_| !c.alone) {
            let row = &mut rows[i];
            if c.event.at - row.at <= DEDUPE_WINDOW_MS {
                row.count = row.count.saturating_add(1);
                row.last_at = c.event.at;
                if let Some(p) = c.grouped_path {
                    if paths[i].len() < 1000 {
                        paths[i].insert(p);
                    }
                    if paths[i].len() > 1 {
                        row.summary = format!(
                            "Changed {} files in {}, outside this chat's folders",
                            paths[i].len(),
                            c.subject
                        );
                    }
                }
                continue;
            }
        }
        if !c.alone {
            open.insert(key, rows.len());
        }
        paths.push(c.grouped_path.into_iter().collect());
        rows.push(Flag {
            id: format!("{}:{}", reason_key(c.reason), c.event.id),
            chat_id: c.event.chat_id.clone(),
            at: c.event.at,
            reason: c.reason,
            summary: c.summary,
            event_id: c.event.id.clone(),
            context: context_ids(events, c.event),
            screenshot: screenshot_near(events, c.event),
            count: 1,
            last_at: c.event.at,
            tier: c.tier,
        });
    }
    rows
}

/// Runs the check. Flags come back oldest first, with what it hid.
pub fn run_full(input: &CheckInput) -> CheckOutput {
    let empty_folders: Vec<String> = Vec::new();
    let mut found: Vec<Candidate> = Vec::new();
    let mut hidden: Vec<Candidate> = Vec::new();
    let mut suppressed: BTreeMap<String, u64> = BTreeMap::new();
    let mut flagged_hosts: HashSet<(String, String)> = HashSet::new();
    let data_dir = input.data_dir;

    let windows = tool_windows(input.events);
    let runtimes = runtime_spans(input.events);
    let tree = Tree::new(input.events, &windows);
    let turns = turn_spans(input.events);

    for event in input.events {
        if event.chat_id.is_empty() {
            continue;
        }
        let chat = input.chats.get(&event.chat_id);
        let folders = chat.map(|chat| &chat.folders).unwrap_or(&empty_folders);
        let harness = chat
            .map(|c| c.harness.as_str())
            .filter(|h| !h.is_empty())
            .unwrap_or(event.harness.as_str());
        let mut flag: Option<(FlagReason, String, String, Option<String>)> = None;
        let mut alone = false;

        // Computer use's recording did not start (its runtime writes this).
        if event.source == ActivitySource::Cua
            && event.kind == "cua.recording"
            && event.detail.get("on").and_then(Value::as_bool) == Some(false)
        {
            let error = str_field(event, "error").unwrap_or("no reason given");
            flag = Some((
                FlagReason::RecordingOff,
                format!(
                    "Computer use recording did not start: {}",
                    one_line(error, 300)
                ),
                String::new(),
                None,
            ));
        }

        // An app no grant covered.
        let bundle = str_field(event, "bundleId").or_else(|| str_field(event, "grant"));
        if flag.is_none() && matches!(event.source, ActivitySource::Cua | ActivitySource::Eslogger)
        {
            if let Some(bundle) = bundle {
                let covered = covering(
                    input.approvals,
                    &event.chat_id,
                    ApprovalSubject::App,
                    event.at,
                    |t| t == bundle,
                );
                if covered.is_none() {
                    flag = Some((
                        FlagReason::AppNotGranted,
                        format!("Acted in {bundle} with no grant for this chat"),
                        bundle.to_string(),
                        None,
                    ));
                }
            }
        }

        // Writes outside the chat's folders.
        if flag.is_none()
            && event.source == ActivitySource::Eslogger
            && matches!(
                event.kind.as_str(),
                "create"
                    | "write"
                    | "rename"
                    | "unlink"
                    | "truncate"
                    | "link"
                    | "copyfile"
                    | "clone"
                    | "exchangedata"
            )
        {
            let granted_app = str_field(event, "grant").is_some();
            let own_runtime = event.pid.is_some_and(|pid| {
                runtimes.get(&event.chat_id).is_some_and(|spans| {
                    spans.iter().any(|span| {
                        span.pid == pid
                            && event.at >= span.started
                            && span.ended.is_none_or(|ended| event.at <= ended)
                    })
                })
            });
            for path in written_paths(event) {
                // The chat's own runtime writing its recording (T1).
                if own_runtime && in_recordings(path, data_dir) {
                    continue;
                }
                // The log's and the recordings' folders are flagged even when
                // a chat's folder holds them, such as a chat on ~ (review U9).
                let guarded = guarded_path(path, data_dir);
                if !guarded
                    && (noise_path(path, granted_app, data_dir)
                        || folders.iter().any(|f| inside(path, f)))
                {
                    continue;
                }
                // A folder approval does not clear a guarded path either, as
                // with a chat's folder (review V6).
                let approved = !guarded
                    && covering(
                        input.approvals,
                        &event.chat_id,
                        ApprovalSubject::Folder,
                        event.at,
                        |t| inside(path, t),
                    )
                    .is_some();
                if !approved {
                    alone = guarded;
                    let verb = match event.kind.as_str() {
                        "create" => "Created",
                        "write" => "Wrote",
                        "rename" => "Moved",
                        "truncate" => "Truncated",
                        "link" => "Linked",
                        "copyfile" | "clone" => "Copied to",
                        "exchangedata" => "Swapped",
                        _ => "Deleted",
                    };
                    flag = Some((
                        FlagReason::WriteOutsideFolders,
                        format!("{verb} {path}, outside this chat's folders"),
                        path.to_string(),
                        Some(path.to_string()),
                    ));
                    break;
                }
            }
        }

        // Sensitive opens.
        if flag.is_none() && event.source == ActivitySource::Eslogger && event.kind == "open" {
            if let Some(path) = str_field(event, "path") {
                if !expected_sensitive(path, folders) {
                    flag = Some((
                        FlagReason::SensitivePath,
                        format!("Opened {path}"),
                        path.to_string(),
                        None,
                    ));
                }
            }
        }

        // New hosts.
        if flag.is_none() {
            if let Some(host) = event_host(event) {
                let lower = host.to_ascii_lowercase();
                let approved = covering(
                    input.approvals,
                    &event.chat_id,
                    ApprovalSubject::Host,
                    event.at,
                    |t| host_matches(t, &lower),
                );
                let key = (event.chat_id.clone(), lower.clone());
                if approved.is_none()
                    && !input.known_hosts.contains(&lower)
                    && !flagged_hosts.contains(&key)
                {
                    flagged_hosts.insert(key);
                    flag = Some((
                        FlagReason::NewHost,
                        format!("Reached {lower}, a host not seen before"),
                        lower,
                        None,
                    ));
                }
            }
        }

        let Some((reason, summary, mut subject, mut grouped_path)) = flag else {
            continue;
        };
        let tier = (event.source == ActivitySource::Eslogger).then(|| tree.tier(event));
        if let Some(tier) = tier {
            let forked_by_root = event
                .pid
                .is_some_and(|pid| tree.forked_by_root(&event.chat_id, pid, event.at));
            let during_turn = in_turn(&turns, &event.chat_id, event.at);
            if let Some(rule) = expected_rule(
                event,
                tier,
                reason,
                harness,
                folders,
                forked_by_root,
                during_turn,
            ) {
                *suppressed.entry(rule.to_string()).or_default() += 1;
                hidden.push(Candidate {
                    event,
                    reason,
                    summary,
                    tier: Some(tier),
                    subject,
                    grouped_path: None,
                    alone: false,
                });
                continue;
            }
            // A harness helper's writes to its own state folder are one row
            // per folder; any other write keeps its own row, and a repeat of
            // the same path still collapses into it.
            let group = (tier == Tier::HarnessChild && reason == FlagReason::WriteOutsideFolders)
                .then(|| group_folder(&subject))
                .flatten();
            match group {
                Some(folder) => subject = folder,
                None => grouped_path = None,
            }
        } else {
            grouped_path = None;
        }
        found.push(Candidate {
            event,
            reason,
            summary,
            tier,
            subject,
            grouped_path,
            alone,
        });
    }

    found.extend(left_running(input, &windows, &tree));
    let flags = dedupe(input.events, found);
    let mut expected = dedupe(input.events, hidden);
    expected.truncate(MAX_EXPECTED_ROWS);
    CheckOutput {
        flags,
        expected,
        suppressed,
    }
}

/// Processes a tool call started that were still running 10 s after it
/// ended (Fenceline: `cmd &` and `nohup` slip past monitors). Keyed on the
/// forked child (`detail.child`); a process whose fork was dropped is
/// placed by its lineage and first sighting. A call with no end is not
/// judged; a call marked `background` is exempt.
fn left_running<'e>(
    input: &CheckInput<'e>,
    windows: &HashMap<String, Vec<ToolWindow>>,
    tree: &Tree,
) -> Vec<Candidate<'e>> {
    let mut exits: HashMap<u32, Vec<i64>> = HashMap::new();
    let mut by_pid: HashMap<(String, u32), Vec<&'e ActivityEvent>> = HashMap::new();
    for e in input
        .events
        .iter()
        .filter(|e| e.source == ActivitySource::Eslogger)
    {
        let Some(pid) = e.pid else { continue };
        if e.kind == "exit" {
            exits.entry(pid).or_default().push(e.at);
        }
        by_pid.entry((e.chat_id.clone(), pid)).or_default().push(e);
    }
    // (chat, child pid) -> (start time, the event to flag).
    let mut started: HashMap<(String, u32), (i64, &'e ActivityEvent)> = HashMap::new();
    for e in input
        .events
        .iter()
        .filter(|e| e.source == ActivitySource::Eslogger)
    {
        if e.chat_id.is_empty() {
            continue;
        }
        if e.kind == "fork" {
            if let Some(child) = pid_field(e, "child") {
                started
                    .entry((e.chat_id.clone(), child))
                    .or_insert((e.at, e));
            }
        }
    }
    // A child whose fork was dropped: first seen by its exec, tied to this
    // chat's tree by `ppid`, `originalPpid` or `groupId`.
    for ((chat, pid), list) in &by_pid {
        let key = (chat.clone(), *pid);
        if chat.is_empty() || started.contains_key(&key) || tree.forks.contains_key(&key) {
            continue;
        }
        let Some(first) = list.iter().min_by_key(|e| e.at) else {
            continue;
        };
        if str_field(first, "tier") != Some("child") || first.kind != "exec" {
            continue;
        }
        let linked = tree.lineage.get(&key).is_some_and(|parents| {
            parents
                .iter()
                .any(|p| tree.first_seen.contains_key(&(chat.clone(), *p)))
        });
        if linked {
            started.insert(key, (first.at, *first));
        }
    }
    let mut out = Vec::new();
    for ((chat, child), (at, event)) in started {
        let Some(list) = windows.get(&chat) else {
            continue;
        };
        let holding: Vec<&ToolWindow> = list.iter().filter(|w| w.holds(at)).collect();
        // Not in a call, a call still running, or work left on purpose.
        if holding.is_empty() || holding.iter().any(|w| w.end.is_none() || w.background) {
            continue;
        }
        let end = holding.iter().filter_map(|w| w.end).max().unwrap_or(at);
        let deadline = end.saturating_add(LEFT_RUNNING_GRACE_MS);
        if input.now_ms <= deadline {
            continue;
        }
        let exited = exits
            .get(&child)
            .is_some_and(|times| times.iter().any(|&t| t >= at && t <= deadline));
        if exited {
            continue;
        }
        let own = by_pid.get(&(chat.clone(), child));
        let what = own
            .and_then(|list| {
                list.iter()
                    .filter(|e| e.kind == "exec" && e.at <= deadline)
                    .max_by_key(|e| e.at)
                    .and_then(|e| str_field(e, "exe"))
            })
            .unwrap_or("A process");
        let reparented =
            own.is_some_and(|list| list.iter().any(|e| pid_field(e, "ppid") == Some(1)));
        let summary = format!(
            "{what} (pid {child}) kept running after its tool call ended{}",
            if reparented {
                ", reparented to launchd"
            } else {
                ""
            }
        );
        out.push(Candidate {
            event,
            reason: FlagReason::LeftRunning,
            summary,
            tier: Some(Tier::Tool),
            subject: child.to_string(),
            grouped_path: None,
            alone: false,
        });
    }
    out
}

pub fn reason_key(reason: FlagReason) -> &'static str {
    match reason {
        FlagReason::AppNotGranted => "appNotGranted",
        FlagReason::WriteOutsideFolders => "writeOutsideFolders",
        FlagReason::LeftRunning => "leftRunning",
        FlagReason::SensitivePath => "sensitivePath",
        FlagReason::NewHost => "newHost",
        FlagReason::RecordingOff => "recordingOff",
    }
}

fn context_ids(events: &[ActivityEvent], center: &ActivityEvent) -> Vec<String> {
    let mut near: Vec<&ActivityEvent> = events
        .iter()
        .filter(|e| e.chat_id == center.chat_id && (e.at - center.at).abs() <= CONTEXT_WINDOW_MS)
        .collect();
    near.sort_by_key(|e| ((e.at - center.at).abs(), e.at));
    near.truncate(MAX_CONTEXT);
    near.sort_by_key(|e| e.at);
    near.into_iter().map(|e| e.id.clone()).collect()
}

fn screenshot_near(events: &[ActivityEvent], center: &ActivityEvent) -> Option<String> {
    events
        .iter()
        .filter(|e| e.chat_id == center.chat_id && (e.at - center.at).abs() <= SCREENSHOT_WINDOW_MS)
        .filter_map(|e| str_field(e, "screenshot").map(|s| ((e.at - center.at).abs(), s)))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, path)| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(
        id: &str,
        at: i64,
        source: ActivitySource,
        kind: &str,
        detail: Value,
        pid: Option<u32>,
    ) -> ActivityEvent {
        ActivityEvent {
            id: id.into(),
            at,
            chat_id: "c1".into(),
            harness: "claude".into(),
            source,
            kind: kind.into(),
            summary: String::new(),
            detail,
            pid,
            approval_id: None,
        }
    }

    fn approval(
        subject: ApprovalSubject,
        target: &str,
        scope: ApprovalScope,
        at: i64,
        end: Option<i64>,
    ) -> ApprovalRecord {
        ApprovalRecord {
            id: format!("a-{target}"),
            chat_id: "c1".into(),
            harness: "claude".into(),
            subject,
            target: target.into(),
            scope,
            granted: true,
            granted_at: at,
            ended_at: end,
        }
    }

    fn chats() -> HashMap<String, ChatContext> {
        HashMap::from([(
            "c1".to_string(),
            ChatContext {
                chat_id: "c1".into(),
                harness: "claude".into(),
                folders: vec!["/Users/t/code/app".into(), "/Users/t/code/my lib".into()],
            },
        )])
    }

    const DATA: &str = "/Users/t/Library/Application Support/com.monocode.desktop";

    fn check_full(
        events: &[ActivityEvent],
        approvals: &[ApprovalRecord],
        data_dir: &str,
    ) -> CheckOutput {
        let chats = chats();
        let hosts = HashSet::from(["github.com".to_string()]);
        run_full(&CheckInput {
            events,
            approvals,
            chats: &chats,
            known_hosts: &hosts,
            now_ms: 1_000_000,
            data_dir: Some(data_dir),
        })
    }

    fn check(events: &[ActivityEvent], approvals: &[ApprovalRecord]) -> Vec<Flag> {
        check_full(events, approvals, DATA).flags
    }

    fn es(id: &str, at: i64, kind: &str, path: &str) -> ActivityEvent {
        ev(
            id,
            at,
            ActivitySource::Eslogger,
            kind,
            json!({ "path": path }),
            Some(10),
        )
    }

    #[test]
    fn writes_inside_the_folders_pass_and_outside_flag() {
        let flags = check(
            &[
                es("1", 1, "write", "/Users/t/code/app/src/main.rs"),
                es("2", 2, "create", "/Users/t/code/my lib/new file.txt"),
                es("3", 3, "unlink", "/Users/t/Documents/taxes.pdf"),
                es("4", 4, "write", "/Users/t/code/application/x"),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["3", "4"],
            "a sibling folder with a shared prefix is outside"
        );
        assert_eq!(flags[0].reason, FlagReason::WriteOutsideFolders);
        assert!(flags[0]
            .summary
            .starts_with("Deleted /Users/t/Documents/taxes.pdf"));
    }

    #[test]
    fn a_rename_out_of_the_folder_flags_on_its_destination() {
        let mut event = es("1", 1, "rename", "/Users/t/code/app/secret.txt");
        event.detail["dest"] = json!("/Users/t/Desktop/secret.txt");
        let flags = check(&[event], &[]);
        assert_eq!(flags.len(), 1);
        assert!(flags[0].summary.contains("/Users/t/Desktop/secret.txt"));
    }

    #[test]
    fn dot_dot_never_counts_as_inside() {
        let flags = check(
            &[es("1", 1, "write", "/Users/t/code/app/../../.zshrc")],
            &[],
        );
        assert_eq!(flags.len(), 1);
    }

    #[test]
    fn a_write_to_the_log_or_the_recordings_is_flagged() {
        // Security review S5.
        let base = "/Users/t/Library/Application Support/com.monocode.desktop";
        let flags = check(
            &[
                es(
                    "1",
                    1,
                    "write",
                    &format!("{base}/activity/app-2026-10-16.jsonl"),
                ),
                es(
                    "2",
                    2,
                    "create",
                    &format!("{base}/activity/install/c.plist"),
                ),
                es(
                    "3",
                    3,
                    "unlink",
                    &format!("{base}/computer-use/recordings/r/turn-00001/after.png"),
                ),
                es("4", 4, "write", &format!("{base}/monocode.db")),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        // The database is guarded too since review X6.
        assert_eq!(ids, vec!["1", "2", "3", "4"]);
    }

    #[test]
    fn the_chats_runtime_may_write_its_recordings_and_nothing_else_may() {
        // Security review T1 and T2.
        let base = "/Users/t/Library/Application Support/com.monocode.desktop";
        let rec = format!("{base}/computer-use/recordings/rt1/turn-00001/after.png");
        let mut runtime = ev(
            "r",
            0,
            ActivitySource::Cua,
            "cua.runtime",
            json!({}),
            Some(77),
        );
        runtime.chat_id = "c1".into();
        let mut by_runtime = es("1", 1, "create", &rec);
        by_runtime.pid = Some(77);
        let by_other = es("2", 2, "create", &rec); // pid 10, the harness
        let mut other_chat_runtime = es("3", 3, "create", &rec);
        other_chat_runtime.pid = Some(77);
        other_chat_runtime.chat_id = "c2".into();
        let mut runtime_elsewhere = es("4", 4, "write", &format!("{base}/activity/app-x.jsonl"));
        runtime_elsewhere.pid = Some(77);
        let case = es("5", 5, "write", &format!("{base}/Activity/app-x.jsonl"));
        let flags = check(
            &[
                runtime,
                by_runtime,
                by_other,
                other_chat_runtime,
                runtime_elsewhere,
                case,
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["2", "3", "4", "5"]);
    }

    #[test]
    fn the_runtime_exemption_is_bounded_in_time_and_place() {
        // Reviews U5, U6 and U9.
        let base = "/Users/t/Library/Application Support/com.monocode.desktop";
        let rec = format!("{base}/computer-use/recordings/rt1/turn-00001/after.png");
        let mut runtime = ev(
            "r",
            100,
            ActivitySource::Cua,
            "cua.runtime",
            json!({}),
            Some(77),
        );
        runtime.chat_id = "c1".into();
        let mut before = es("1", 50, "create", &rec);
        before.pid = Some(77);
        let mut during = es("2", 150, "create", &rec);
        during.pid = Some(77);
        let mut exit = ev(
            "x",
            200,
            ActivitySource::Eslogger,
            "exit",
            json!({}),
            Some(77),
        );
        exit.chat_id = "c1".into();
        let mut reused = es("3", 300, "create", &rec);
        reused.pid = Some(77);
        let mut dotdot = es(
            "4",
            160,
            "write",
            &format!("{base}/computer-use/recordings/../../activity/app-x.jsonl"),
        );
        dotdot.pid = Some(77);
        let flags = check(&[runtime, before, during, exit, reused, dotdot], &[]);
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["1", "4", "3"]);
    }

    #[test]
    fn a_runtime_ends_at_its_first_exit_its_stop_or_the_next_start() {
        // Review V2 and V3.
        let base = "/Users/t/Library/Application Support/com.monocode.desktop";
        let rec = format!("{base}/computer-use/recordings/rt1/turn-00001/after.png");
        let cua = |id: &str, at: i64, kind: &str, pid: u32| {
            let mut e = ev(id, at, ActivitySource::Cua, kind, json!({}), Some(pid));
            e.chat_id = "c1".into();
            e
        };
        let exit = |id: &str, at: i64, pid: u32| {
            let mut e = ev(
                id,
                at,
                ActivitySource::Eslogger,
                "exit",
                json!({}),
                Some(pid),
            );
            e.chat_id = "c1".into();
            e
        };
        let write = |id: &str, at: i64, pid: u32| {
            let mut e = es(id, at, "create", &rec);
            e.pid = Some(pid);
            e
        };
        let flags = check(
            &[
                // pid 77: exits at 200; a later process reusing 77 exits at 500.
                cua("r1", 100, "cua.runtime", 77),
                write("1", 150, 77),
                exit("x1", 200, 77),
                write("2", 300, 77),
                exit("x2", 500, 77),
                // pid 88: its stop record ends it, with no exit in view.
                cua("r2", 600, "cua.runtime", 88),
                write("3", 650, 88),
                cua("s2", 700, "cua.runtime.stop", 88),
                write("4", 750, 88),
                // pid 99: the chat's next runtime start ends it.
                cua("r3", 800, "cua.runtime", 99),
                write("5", 850, 99),
                cua("r4", 900, "cua.runtime", 66),
                write("6", 950, 99),
                write("7", 960, 66),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["2", "4", "6"]);
    }

    #[test]
    fn a_folder_approval_does_not_clear_the_log_or_the_recordings() {
        // Review V6.
        let approvals = [approval(
            ApprovalSubject::Folder,
            "/Users/t",
            ApprovalScope::Chat,
            0,
            None,
        )];
        let base = "/Users/t/Library/Application Support/com.monocode.desktop";
        let flags = check(
            &[
                es("1", 5, "write", &format!("{base}/activity/app-x.jsonl")),
                es("2", 6, "write", "/Users/t/Documents/a.txt"),
            ],
            &approvals,
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["1"]);
    }

    #[test]
    fn noise_paths_never_flag() {
        let flags = check(
            &[
                es("1", 1, "write", "/private/var/folders/xy/T/tmp.1"),
                es("2", 2, "write", "/Users/t/.claude/projects/x.jsonl"),
                es("3", 3, "write", "/Users/t/.claude.json.backup"),
                es("4", 4, "create", "/dev/null"),
                es("5", 5, "write", "/Users/t/.codex/sessions/a"),
            ],
            &[],
        );
        assert!(flags.is_empty(), "{flags:?}");
    }

    #[test]
    fn a_folder_approval_covers_a_write_while_it_holds() {
        let approvals = [approval(
            ApprovalSubject::Folder,
            "/Users/t/Documents",
            ApprovalScope::Chat,
            0,
            Some(100),
        )];
        let flags = check(
            &[
                es("1", 50, "write", "/Users/t/Documents/a.txt"),
                es("2", 150, "write", "/Users/t/Documents/b.txt"),
            ],
            &approvals,
        );
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].event_id, "2");
    }

    #[test]
    fn a_denied_or_other_chat_approval_covers_nothing() {
        let mut denied = approval(
            ApprovalSubject::Folder,
            "/Users/t/Documents",
            ApprovalScope::Chat,
            0,
            None,
        );
        denied.granted = false;
        let mut other = approval(
            ApprovalSubject::Folder,
            "/Users/t/Documents",
            ApprovalScope::Chat,
            0,
            None,
        );
        other.chat_id = "c2".into();
        let flags = check(
            &[es("1", 5, "write", "/Users/t/Documents/a.txt")],
            &[denied, other],
        );
        assert_eq!(flags.len(), 1);
    }

    #[test]
    fn computer_use_in_an_app_with_no_grant_flags() {
        let approvals = [approval(
            ApprovalSubject::App,
            "com.apple.calculator",
            ApprovalScope::Chat,
            0,
            Some(100),
        )];
        let flags = check(
            &[
                ev(
                    "1",
                    10,
                    ActivitySource::Cua,
                    "click",
                    json!({"bundleId": "com.apple.calculator"}),
                    None,
                ),
                ev(
                    "2",
                    20,
                    ActivitySource::Cua,
                    "click",
                    json!({"bundleId": "com.apple.finder"}),
                    None,
                ),
                ev(
                    "3",
                    200,
                    ActivitySource::Cua,
                    "click",
                    json!({"bundleId": "com.apple.calculator"}),
                    None,
                ),
            ],
            &approvals,
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["2", "3"],
            "a grant that ended covers nothing after"
        );
        assert!(flags.iter().all(|f| f.reason == FlagReason::AppNotGranted));
    }

    #[test]
    fn a_granted_app_writing_its_own_state_is_noise_but_a_document_is_not() {
        let approvals = [approval(
            ApprovalSubject::App,
            "com.apple.finder",
            ApprovalScope::Chat,
            0,
            None,
        )];
        let mut prefs = es(
            "1",
            5,
            "write",
            "/Users/t/Library/Preferences/com.apple.finder.plist",
        );
        prefs.detail["grant"] = json!("com.apple.finder");
        let mut doc = es("2", 6, "unlink", "/Users/t/Documents/report.docx");
        doc.detail["grant"] = json!("com.apple.finder");
        let flags = check(&[prefs, doc], &approvals);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].event_id, "2");
        assert_eq!(flags[0].reason, FlagReason::WriteOutsideFolders);
    }

    #[test]
    fn sensitive_opens_flag_except_the_projects_own_env() {
        let flags = check(
            &[
                es("1", 1, "open", "/Users/t/.ssh/id_ed25519"),
                es("2", 2, "open", "/Users/t/code/app/.env.local"),
                es("3", 3, "open", "/Users/t/other/.env"),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["1", "3"]);
        assert!(flags.iter().all(|f| f.reason == FlagReason::SensitivePath));
    }

    #[test]
    fn a_new_host_flags_once_per_chat_unless_approved_or_known() {
        let approvals = [approval(
            ApprovalSubject::Host,
            "*.example.com",
            ApprovalScope::Always,
            0,
            None,
        )];
        let flags = check(
            &[
                ev(
                    "1",
                    1,
                    ActivitySource::Otel,
                    "net",
                    json!({"host": "api.example.com"}),
                    None,
                ),
                ev(
                    "2",
                    2,
                    ActivitySource::Otel,
                    "net",
                    json!({"host": "GitHub.com"}),
                    None,
                ),
                ev(
                    "3",
                    3,
                    ActivitySource::Adapter,
                    "tool.started",
                    json!({"host": "evil.test"}),
                    None,
                ),
                ev(
                    "4",
                    4,
                    ActivitySource::Adapter,
                    "tool.started",
                    json!({"host": "evil.test"}),
                    None,
                ),
                ev(
                    "5",
                    5,
                    ActivitySource::Otel,
                    "net",
                    json!({"host": "example.com.evil.test"}),
                    None,
                ),
            ],
            &approvals,
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["3", "5"]);
    }

    #[test]
    fn a_host_under_the_otel_detail_flags_too() {
        // Integration: lane A keeps an OTel-derived host at detail.otel.host.
        let flags = check(
            &[ev(
                "1",
                1,
                ActivitySource::Adapter,
                "tool.call",
                json!({"callId": "w1", "otel": {"host": "news.example.org"}}),
                None,
            )],
            &[],
        );
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].reason, FlagReason::NewHost);
    }

    #[test]
    fn a_project_host_rule_covers_every_chat_but_a_folder_rule_does_not() {
        let mut host = approval(
            ApprovalSubject::Host,
            "docs.rs",
            ApprovalScope::Always,
            0,
            None,
        );
        host.chat_id = String::new();
        let mut folder = approval(
            ApprovalSubject::Folder,
            "/Users/t/Documents",
            ApprovalScope::Always,
            0,
            None,
        );
        folder.chat_id = String::new();
        let flags = check(
            &[
                ev(
                    "1",
                    1,
                    ActivitySource::Otel,
                    "net",
                    json!({"host": "docs.rs"}),
                    None,
                ),
                es("2", 2, "write", "/Users/t/Documents/a"),
            ],
            &[host, folder],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["2"]);
    }

    #[test]
    fn a_once_approval_lapses_after_its_window() {
        let record = approval(ApprovalSubject::Host, "a.dev", ApprovalScope::Once, 0, None);
        assert!(holds_at(&record, ONCE_WINDOW_MS));
        assert!(!holds_at(&record, ONCE_WINDOW_MS + 1));
        assert!(!holds_at(&record, -1));
    }

    #[test]
    fn a_process_left_running_after_its_tool_call_flags() {
        // Changed for Mac test 3.8: a call is one `tool.call` record whose
        // end is `detail.endedAt`, and a fork names the new process in
        // `detail.child` (the old test used two records and fork pids).
        let call = ev(
            "t",
            100,
            ActivitySource::Adapter,
            "tool.call",
            json!({"callId": "t1", "endedAt": 200}),
            None,
        );
        let daemon = ev(
            "p1",
            110,
            ActivitySource::Eslogger,
            "fork",
            json!({"child": 77, "tier": "child", "exe": "/bin/sh"}),
            Some(10),
        );
        let done = ev(
            "p2",
            120,
            ActivitySource::Eslogger,
            "fork",
            json!({"child": 78, "tier": "child", "exe": "/bin/sh"}),
            Some(10),
        );
        let exit = ev(
            "p3",
            150,
            ActivitySource::Eslogger,
            "exit",
            json!({"tier": "child"}),
            Some(78),
        );
        let flags = check(&[call, daemon, done, exit], &[]);
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert_eq!(flags[0].reason, FlagReason::LeftRunning);
        assert_eq!(flags[0].event_id, "p1");
        assert!(flags[0].summary.contains("pid 77"), "{}", flags[0].summary);
    }

    #[test]
    fn a_running_call_is_not_judged_yet() {
        let chats = chats();
        let hosts = HashSet::new();
        let events = [
            ev(
                "t",
                100,
                ActivitySource::Adapter,
                "tool.started",
                json!({"callId": "t1"}),
                None,
            ),
            ev(
                "p",
                110,
                ActivitySource::Eslogger,
                "fork",
                json!({}),
                Some(5),
            ),
        ];
        let flags = run_full(&CheckInput {
            events: &events,
            approvals: &[],
            chats: &chats,
            known_hosts: &hosts,
            now_ms: 105 + LEFT_RUNNING_GRACE_MS,
            data_dir: Some(DATA),
        })
        .flags;
        assert!(flags.is_empty());
    }

    #[test]
    fn a_flag_carries_its_context_and_nearest_screenshot() {
        let flags = check(
            &[
                ev(
                    "s1",
                    0,
                    ActivitySource::Cua,
                    "screenshot",
                    json!({"screenshot": "/x/a.png", "bundleId": "com.apple.calculator"}),
                    None,
                ),
                es("w", 5_000, "unlink", "/Users/t/Documents/x"),
                ev(
                    "s2",
                    9_000,
                    ActivitySource::Cua,
                    "screenshot",
                    json!({"screenshot": "/x/b.png"}),
                    None,
                ),
                ev(
                    "far",
                    90_000,
                    ActivitySource::Adapter,
                    "turn",
                    json!({}),
                    None,
                ),
            ],
            &[approval(
                ApprovalSubject::App,
                "com.apple.calculator",
                ApprovalScope::Chat,
                0,
                None,
            )],
        );
        let flag = flags.iter().find(|f| f.event_id == "w").unwrap();
        assert_eq!(flag.screenshot.as_deref(), Some("/x/b.png"));
        assert_eq!(flag.context, vec!["s1", "w", "s2"]);
    }

    #[test]
    fn unattributed_events_are_never_flagged() {
        let mut event = es("1", 1, "unlink", "/etc/hosts");
        event.chat_id = String::new();
        assert!(check(&[event], &[]).is_empty());
    }

    // Wave 3 Mac test findings 3.7, 3.8, 3.9, 3.11 and decision 1.

    fn call(id: &str, at: i64, call: &str, ended: Option<i64>) -> ActivityEvent {
        let mut detail = json!({ "callId": call });
        if let Some(end) = ended {
            detail["endedAt"] = json!(end);
        }
        ev(id, at, ActivitySource::Adapter, "tool.call", detail, None)
    }

    fn proc_ev(id: &str, at: i64, kind: &str, pid: u32, detail: Value) -> ActivityEvent {
        ev(id, at, ActivitySource::Eslogger, kind, detail, Some(pid))
    }

    #[test]
    fn a_child_left_running_inside_a_call_window_flags() {
        let events = [
            call("t", 100, "t1", Some(200)),
            proc_ev(
                "f",
                150,
                "fork",
                10,
                json!({"tier": "root", "child": 77, "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "e",
                151,
                "exec",
                77,
                json!({"tier": "child", "ppid": 10, "exe": "/bin/sleep"}),
            ),
        ];
        let flags = check(&events, &[]);
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["f"]);
    }

    #[test]
    fn expected_harness_reads_are_not_flags() {
        let events = [
            // A turn that ended before: what follows is outside every turn
            // (review X4).
            turn("t0", 0, "turn.start"),
            turn("t9", 0, "turn.end"),
            proc_ev(
                "k1",
                1,
                "open",
                10,
                json!({"tier": "root", "path": "/Users/t/Library/Keychains/login.keychain-db", "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "k2",
                2,
                "open",
                10,
                json!({"tier": "root", "path": "/Library/Keychains/System.keychain", "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "z",
                3,
                "write",
                11,
                json!({"tier": "child", "path": "/Users/t/.zcompdump-mac-5.9", "exe": "/bin/zsh"}),
            ),
            proc_ev(
                "d",
                4,
                "open",
                12,
                json!({"tier": "child", "path": "/Users/t/.docker/config.json", "exe": "/usr/local/bin/docker-credential-desktop"}),
            ),
        ];
        assert_eq!(check(&events, &[]), vec![]);
    }

    #[test]
    fn repeated_flags_collapse_into_one_row() {
        let events = [
            proc_ev(
                "a",
                1,
                "open",
                11,
                json!({"tier": "child", "path": "/Users/t/.ssh/known_hosts", "exe": "/bin/cat"}),
            ),
            proc_ev(
                "b",
                2,
                "open",
                11,
                json!({"tier": "child", "path": "/Users/t/.ssh/known_hosts", "exe": "/bin/cat"}),
            ),
            proc_ev(
                "c",
                3,
                "open",
                11,
                json!({"tier": "child", "path": "/Users/t/.ssh/known_hosts", "exe": "/bin/cat"}),
            ),
        ];
        assert_eq!(check(&events, &[]).len(), 1);
    }

    #[test]
    fn an_mcp_servers_state_folder_is_one_row() {
        let events = [
            proc_ev(
                "a",
                1,
                "write",
                11,
                json!({"tier": "child", "path": "/Users/t/.claude-server-commander/config.json", "exe": "/usr/local/bin/node"}),
            ),
            proc_ev(
                "b",
                2,
                "create",
                11,
                json!({"tier": "child", "path": "/Users/t/.claude-server-commander/logs/x.log", "exe": "/usr/local/bin/node"}),
            ),
        ];
        assert_eq!(check(&events, &[]).len(), 1);
    }

    #[test]
    fn a_recording_that_did_not_start_flags() {
        let events = [ev(
            "r",
            1,
            ActivitySource::Cua,
            "cua.recording",
            json!({"on": false, "error": "bounded_resource_outside_manifest"}),
            None,
        )];
        assert_eq!(check(&events, &[]).len(), 1);
    }

    #[test]
    fn the_dev_data_folder_is_guarded() {
        let approvals = [approval(
            ApprovalSubject::Folder,
            "/Users/t",
            ApprovalScope::Chat,
            0,
            None,
        )];
        let dev = "/Users/t/Library/Application Support/com.monocode.desktop.dev";
        let events = [es("1", 5, "write", &format!("{dev}/activity/app-x.jsonl"))];
        assert_eq!(check_full(&events, &approvals, dev).flags.len(), 1);
    }

    #[test]
    fn a_tool_tier_event_is_never_hidden() {
        // A Bash call reading the keychain through `security`, or a shell
        // in a tool call rewriting compinit's dump, stays a flag.
        let events = [
            call("t", 100, "t1", Some(500)),
            proc_ev(
                "f",
                110,
                "fork",
                10,
                json!({"tier": "root", "child": 30, "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "x",
                111,
                "exec",
                30,
                json!({"tier": "child", "exe": "/usr/bin/security"}),
            ),
            proc_ev(
                "k",
                112,
                "open",
                30,
                json!({"tier": "child", "exe": "/usr/bin/security", "path": "/Users/t/Library/Keychains/login.keychain-db"}),
            ),
            proc_ev(
                "z",
                113,
                "write",
                30,
                json!({"tier": "child", "exe": "/bin/zsh", "path": "/Users/t/.zcompdump"}),
            ),
            proc_ev("e", 120, "exit", 30, json!({"tier": "child"})),
        ];
        let out = check_full(&events, &[], DATA);
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["k", "z"]);
        assert!(out.flags.iter().all(|f| f.tier == Some(Tier::Tool)));
        assert!(out.suppressed.is_empty());
    }

    #[test]
    fn a_keychain_read_by_security_is_hidden_only_when_the_harness_ran_it() {
        // A tool's call record can be missing (a reload lost it, Mac test
        // 2.8), which leaves its processes in the harness-child tier. A
        // `security` run by a shell the harness started is then not the
        // harness's own sign-in read, and must still flag.
        let events = [
            // A turn that ended before: what follows is outside every turn
            // (review X4).
            turn("t0", 0, "turn.start"),
            turn("t9", 0, "turn.end"),
            proc_ev(
                "f1",
                100,
                "fork",
                10,
                json!({"tier": "root", "child": 40, "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "own",
                101,
                "open",
                40,
                json!({"tier": "child", "exe": "/usr/bin/security", "path": "/Users/t/Library/Keychains/login.keychain-db"}),
            ),
            proc_ev(
                "f2",
                200,
                "fork",
                10,
                json!({"tier": "root", "child": 50, "exe": "/usr/local/bin/claude"}),
            ),
            proc_ev(
                "f3",
                201,
                "fork",
                50,
                json!({"tier": "child", "child": 51, "exe": "/bin/zsh"}),
            ),
            proc_ev(
                "shell",
                202,
                "open",
                51,
                json!({"tier": "child", "exe": "/usr/bin/security", "path": "/Users/t/Library/Keychains/login.keychain-db"}),
            ),
            proc_ev(
                "nofork",
                // Past the 15-minute window, so it is its own row.
                300 + DEDUPE_WINDOW_MS + 1,
                "open",
                60,
                json!({"tier": "child", "exe": "/usr/bin/security", "path": "/Users/t/Library/Keychains/login.keychain-db"}),
            ),
        ];
        let out = check_full(&events, &[], DATA);
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert!(!ids.contains(&"own"), "{ids:?}");
        assert!(ids.contains(&"shell"), "{ids:?}");
        assert!(ids.contains(&"nofork"), "{ids:?}");
    }

    #[test]
    fn shell_rc_and_launch_agent_writes_are_never_hidden() {
        let events = [
            // A turn that ended before: what follows is outside every turn
            // (review X4).
            turn("t0", 0, "turn.start"),
            turn("t9", 0, "turn.end"),
            proc_ev(
                "rc",
                1,
                "write",
                11,
                json!({"tier": "child", "exe": "/bin/zsh", "path": "/Users/t/.zshrc"}),
            ),
            proc_ev(
                "la",
                2,
                "create",
                11,
                json!({"tier": "child", "exe": "/bin/zsh", "path": "/Users/t/Library/LaunchAgents/x.plist"}),
            ),
            proc_ev(
                "dump",
                3,
                "write",
                11,
                json!({"tier": "child", "exe": "/bin/zsh", "path": "/Users/t/.zcompdump-host-5.9"}),
            ),
            proc_ev(
                "hist",
                4,
                "write",
                11,
                json!({"tier": "child", "exe": "/bin/bash", "path": "/Users/t/.zsh_history"}),
            ),
            proc_ev(
                "other",
                5,
                "write",
                11,
                json!({"tier": "child", "exe": "/usr/bin/python3", "path": "/Users/t/.zcompdump"}),
            ),
        ];
        let out = check_full(&events, &[], DATA);
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["rc", "la", "other"]);
        assert_eq!(out.suppressed.get(RULE_SHELL_INIT), Some(&2));
        assert_eq!(out.expected.len(), 2);
    }

    #[test]
    fn expected_reads_are_per_harness_and_per_reader() {
        // A Codex chat reading Claude's keychain files is not expected,
        // nor is a random helper reading Docker's credentials.
        let mut codex = chats();
        codex.get_mut("c1").unwrap().harness = "codex".into();
        let events = [
            proc_ev(
                "k",
                1,
                "open",
                10,
                json!({"tier": "root", "path": "/Library/Keychains/System.keychain", "exe": "/usr/local/bin/codex"}),
            ),
            proc_ev(
                "a",
                2,
                "open",
                10,
                json!({"tier": "root", "path": "/Users/t/.codex/auth.json", "exe": "/usr/local/bin/codex"}),
            ),
            proc_ev(
                "d",
                3,
                "open",
                12,
                json!({"tier": "child", "path": "/Users/t/.docker/config.json", "exe": "/usr/bin/curl"}),
            ),
            proc_ev(
                "s",
                4,
                "open",
                12,
                json!({"tier": "child", "path": "/Users/t/Library/Keychains/login.keychain-db", "exe": "/usr/bin/security"}),
            ),
        ];
        let hosts = HashSet::new();
        let out = run_full(&CheckInput {
            events: &events,
            approvals: &[],
            chats: &codex,
            known_hosts: &hosts,
            now_ms: 1_000_000,
            data_dir: Some(DATA),
        });
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["k", "d", "s"]);
        assert_eq!(out.suppressed.get(RULE_HARNESS_READ), Some(&1));
    }

    #[test]
    fn repeats_count_and_keep_their_first_and_last_times() {
        let open = |id: &str, at: i64| {
            proc_ev(
                id,
                at,
                "open",
                11,
                json!({"tier": "child", "path": "/Users/t/.ssh/known_hosts", "exe": "/bin/cat"}),
            )
        };
        let late = DEDUPE_WINDOW_MS + 10;
        let flags = check(
            &[open("a", 1), open("b", 5), open("c", 9), open("d", late)],
            &[],
        );
        assert_eq!(flags.len(), 2);
        assert_eq!((flags[0].count, flags[0].at, flags[0].last_at), (3, 1, 9));
        assert_eq!((flags[1].count, flags[1].at), (1, late));
    }

    #[test]
    fn a_harness_helpers_folder_row_counts_its_files() {
        let flags = check(
            &[
                proc_ev(
                    "a",
                    1,
                    "write",
                    11,
                    json!({"tier": "child", "path": "/Users/t/.claude-server-commander/config.json", "exe": "/usr/local/bin/node"}),
                ),
                proc_ev(
                    "b",
                    2,
                    "create",
                    11,
                    json!({"tier": "child", "path": "/Users/t/.claude-server-commander/logs/x.log", "exe": "/usr/local/bin/node"}),
                ),
                proc_ev(
                    "c",
                    3,
                    "write",
                    11,
                    json!({"tier": "child", "path": "/Users/t/.claude-server-commander/config.json", "exe": "/usr/local/bin/node"}),
                ),
            ],
            &[],
        );
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].count, 3);
        assert_eq!(
            flags[0].summary,
            "Changed 2 files in /Users/t/.claude-server-commander, outside this chat's folders"
        );
        assert_eq!(flags[0].tier, Some(Tier::HarnessChild));
    }

    #[test]
    fn a_running_or_background_call_never_yields_left_running() {
        let child = |id: &str, at: i64, pid: u32| {
            proc_ev(id, at, "fork", 10, json!({"tier": "root", "child": pid}))
        };
        let mut bg = call("b", 300, "t2", Some(400));
        bg.detail["background"] = json!(true);
        let flags = check(
            &[
                call("r", 100, "t1", None),
                child("f1", 150, 41),
                bg,
                child("f2", 350, 42),
            ],
            &[],
        );
        assert!(flags.is_empty(), "{flags:?}");
    }

    #[test]
    fn a_process_whose_fork_was_dropped_is_placed_by_its_lineage() {
        // The fork of 55 never reached the log; its exec names its parent
        // through originalPpid and groupId, and launchd adopted it.
        let events = [
            call("t", 100, "t1", Some(200)),
            proc_ev("f", 110, "fork", 10, json!({"tier": "root", "child": 30})),
            proc_ev(
                "x",
                111,
                "exec",
                30,
                json!({"tier": "child", "ppid": 10, "exe": "/bin/zsh"}),
            ),
            proc_ev(
                "e",
                150,
                "exec",
                55,
                json!({"tier": "child", "ppid": 1, "originalPpid": 30, "groupId": 30, "exe": "/usr/bin/nohup"}),
            ),
            proc_ev("z", 160, "exit", 30, json!({"tier": "child"})),
        ];
        let flags = check(&events, &[]);
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert_eq!(flags[0].event_id, "e");
        assert_eq!(
            flags[0].summary,
            "/usr/bin/nohup (pid 55) kept running after its tool call ended, reparented to launchd"
        );
    }

    fn turn(id: &str, at: i64, kind: &str) -> ActivityEvent {
        ev(id, at, ActivitySource::Adapter, kind, json!({}), None)
    }

    #[test]
    fn a_helpers_distinct_writes_outside_app_state_each_get_a_row() {
        // Review X3: a second write under the same top folder folded into
        // the first row, so a LaunchAgent after a Preferences write, or
        // authorized_keys after known_hosts, was never seen on its own.
        let w = |id: &str, at: i64, path: &str| {
            proc_ev(
                id,
                at,
                "write",
                11,
                json!({"tier": "child", "path": path, "exe": "/usr/local/bin/node"}),
            )
        };
        let flags = check(
            &[
                w("p", 1, "/Users/t/Library/Preferences/y.plist"),
                w("la", 2, "/Users/t/Library/LaunchAgents/x.plist"),
                w("kh", 3, "/Users/t/.ssh/known_hosts"),
                w("ak", 4, "/Users/t/.ssh/authorized_keys"),
                w("ak2", 5, "/Users/t/.ssh/authorized_keys"),
                w("gh1", 6, "/Users/t/.config/other/a.json"),
                w("gh2", 7, "/Users/t/.config/gh/hosts.yml"),
                w("rc1", 8, "/Users/t/.npmrc"),
                w("rc2", 9, "/Users/t/.zshrc"),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["p", "la", "kh", "ak", "gh1", "gh2", "rc1", "rc2"]);
        // The same path still collapses.
        let ak = flags.iter().find(|f| f.event_id == "ak").unwrap();
        assert_eq!(ak.count, 2);
    }

    #[test]
    fn a_harness_child_read_inside_a_turn_is_shown() {
        // Review X4: a tool whose call record is missing or late lands in
        // the harness-child tier. Inside a turn its `security` read must
        // show; outside every turn the harness's own poll stays hidden.
        let key = "/Users/t/Library/Keychains/login.keychain-db";
        let read = |id: &str, at: i64, pid: u32| {
            [
                proc_ev(
                    &format!("f{id}"),
                    at,
                    "fork",
                    10,
                    json!({"tier": "root", "child": pid, "exe": "/usr/local/bin/claude"}),
                ),
                proc_ev(
                    id,
                    at + 1,
                    "open",
                    pid,
                    json!({"tier": "child", "exe": "/usr/bin/security", "path": key}),
                ),
            ]
        };
        let mut events = vec![turn("t1", 100, "turn.start"), turn("t2", 300, "turn.end")];
        events.extend(read("in", 150, 40));
        events.extend(read("out", 400 + DEDUPE_WINDOW_MS, 41));
        events.push(turn("t3", 500 + 2 * DEDUPE_WINDOW_MS, "turn.start"));
        // An open turn runs to now.
        events.extend(read("open", 600 + 2 * DEDUPE_WINDOW_MS, 42));
        events.push(proc_ev(
            "root",
            700 + 2 * DEDUPE_WINDOW_MS,
            "open",
            10,
            json!({"tier": "root", "exe": "/usr/local/bin/claude", "path": key}),
        ));
        events.sort_by_key(|e| e.at);
        let hosts = HashSet::new();
        let out = run_full(&CheckInput {
            events: &events,
            approvals: &[],
            chats: &chats(),
            known_hosts: &hosts,
            now_ms: 10 * DEDUPE_WINDOW_MS,
            data_dir: Some(DATA),
        });
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["in", "open"], "{ids:?}");
        // The harness itself is unchanged: hidden during a turn too.
        assert_eq!(out.suppressed.get(RULE_HARNESS_READ), Some(&2));

        // A chat with no turn records: the whole window counts as a turn.
        let bare: Vec<ActivityEvent> = read("bare", 150, 43).into();
        let out = check_full(&bare, &[], DATA);
        assert_eq!(out.flags.len(), 1, "{:?}", out.flags);
    }

    #[test]
    fn the_docker_owner_rule_needs_an_installed_docker() {
        // Review X5: any binary named docker, from any folder, read the
        // credentials unseen.
        let read = |id: &str, at: i64, exe: &str| {
            proc_ev(
                id,
                at,
                "open",
                12,
                json!({"tier": "child", "path": "/Users/t/.docker/config.json", "exe": exe}),
            )
        };
        let events = [
            turn("t1", 1, "turn.start"),
            turn("t2", 2, "turn.end"),
            read("a", 10, "/usr/local/bin/docker"),
            read("b", 11, "/opt/homebrew/bin/docker-credential-osxkeychain"),
            read(
                "c",
                12,
                "/Applications/Docker.app/Contents/Resources/bin/docker-credential-desktop",
            ),
            read("d", 13 + DEDUPE_WINDOW_MS, "/Users/t/bin/docker"),
            read(
                "e",
                14 + 2 * DEDUPE_WINDOW_MS,
                "/tmp/x/docker-credential-desktop",
            ),
            read(
                "f",
                15 + 3 * DEDUPE_WINDOW_MS,
                "/usr/local/bin/docker-credential-",
            ),
            read(
                "g",
                16 + 4 * DEDUPE_WINDOW_MS,
                "/Applications/Docker.app/Contents/../../x/docker",
            ),
        ];
        let out = check_full(&events, &[], DATA);
        let ids: Vec<&str> = out.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["d", "e", "f", "g"]);
        assert_eq!(out.suppressed.get(RULE_CREDENTIAL_OWNER), Some(&3));
    }

    #[test]
    fn shell_snapshots_the_harness_runs_are_never_noise() {
        // Claude Code runs a file from shell-snapshots (and the scripts in
        // session-env) before every Bash command; Codex sources its
        // shell_snapshots file (codex-rs/core/src/shell_snapshot.rs:1010).
        // Code planted there runs on every later command (review Y1).
        let data = DATA;
        for path in [
            format!("{data}/provider-accounts/claude/a1/shell-snapshots/snapshot-zsh-1.sh"),
            format!("{data}/provider-accounts/claude/a1/session-env/x/hook.sh"),
            format!("{data}/codex-monos/m1/shell_snapshots/s.sh"),
            "/Users/t/.claude/shell-snapshots/snapshot-zsh-1.sh".to_string(),
            "/Users/t/.claude/session-env/x/hook.sh".to_string(),
            "/Users/t/.codex/shell_snapshots/s.sh".to_string(),
        ] {
            assert!(!noise_path(&path, false, Some(data)), "{path}");
        }
        // Their transcripts and logs stay noise.
        assert!(noise_path(
            "/Users/t/.claude/projects/p/s.jsonl",
            false,
            Some(data)
        ));
        assert!(noise_path(
            &format!("{data}/codex-monos/m1/logs/x.log"),
            false,
            Some(data)
        ));
    }

    #[test]
    fn only_harness_state_in_the_data_folder_is_noise() {
        // Review X6: otel-helper holds a script Claude Code runs, and
        // provider-accounts holds other accounts' settings; both were noise.
        let base = DATA;
        let flags = check(
            &[
                es("1", 1, "write", &format!("{base}/otel-helper/h.sh")),
                es(
                    "2",
                    2,
                    "write",
                    &format!("{base}/provider-accounts/claude/a/settings.json"),
                ),
                es("3", 3, "write", &format!("{base}/monocode.db")),
                es("4", 4, "write", &format!("{base}/Monocode.db-wal")),
                es(
                    "5",
                    5,
                    "write",
                    &format!("{base}/codex-monos/a/config.toml"),
                ),
                es(
                    "6",
                    6,
                    "write",
                    &format!("{base}/codex-monos/a/sessions/2026/r.jsonl"),
                ),
                es(
                    "7",
                    7,
                    "write",
                    &format!("{base}/provider-accounts/claude/a/projects/p/s.jsonl"),
                ),
                es(
                    "8",
                    8,
                    "write",
                    &format!("{base}/provider-accounts/claude/a/.claude.json"),
                ),
                es(
                    "9",
                    9,
                    "write",
                    &format!("{base}/provider-accounts/codex/a/log/codex-tui.log"),
                ),
                es(
                    "10",
                    10,
                    "write",
                    &format!("{base}/codex-monos/a/../a/auth.json"),
                ),
            ],
            &[],
        );
        let ids: Vec<&str> = flags.iter().map(|f| f.event_id.as_str()).collect();
        // A provider account's `.claude.json` holds its MCP servers: shown.
        assert_eq!(ids, vec!["1", "2", "3", "4", "5", "8", "10"]);
    }

    #[test]
    fn inside_reads_literally() {
        assert!(inside("/a/b", "/a"));
        assert!(inside("/a", "/a/"));
        assert!(!inside("/ab", "/a"));
        assert!(!inside("a/b", "a"));
        assert!(!inside("/a/../b", "/a"));
        assert!(!inside("/a/b", ""));
        assert!(inside("/a/-x y", "/a"));
    }
}

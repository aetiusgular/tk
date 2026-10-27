//! Harness telemetry (wave 3, lane A): a loopback OTLP receiver.
//!
//! Claude Code and Codex export their own OpenTelemetry log events (tool
//! calls, approvals, model requests) here. Each record becomes an activity
//! event and goes to the window, where the telemetry feed joins it with
//! MonoCode's own adapter events and writes it to the activity log.
//!
//! - Listens on 127.0.0.1 only, on a free port, started on first use. It
//!   makes no outbound call of any kind.
//! - Each chat gets its own random token, never in an argument
//!   (`harness_env`): Claude Code reads it from an `otelHeadersHelper`
//!   script in a private folder, Codex from its environment, which its
//!   shell commands do not inherit. A request without a known token is
//!   refused before any of its body is read. The token names the chat, so a
//!   process can only post into the chat whose token it holds.
//! - Requests whose `Host` is not this listener are refused (DNS rebinding),
//!   and a browser's preflight gets no CORS answer.
//! - Bounded: 16 connections, a 16 KiB head, a 4 MiB body, deadlines re-armed
//!   before every read, a request rate cap, 2,000 records per request.
//! - Off in Settings means no harness is pointed here and anything still
//!   arriving is dropped.

#[path = "decode.rs"]
pub mod decode;
#[path = "harness_env.rs"]
pub mod harness_env;
#[path = "normalize.rs"]
pub mod normalize;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::activity::types::ActivityEvent;

const MAX_HEAD_BYTES: usize = 16 * 1024;
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const HEAD_DEADLINE: Duration = Duration::from_secs(10);
const BODY_DEADLINE: Duration = Duration::from_secs(30);
/// Chats with a token. The oldest gives way; its harness's next start gets a
/// new one.
const MAX_TOKENS: usize = 512;
/// Chats the window may name in one configure call.
const MAX_CHATS: usize = 5000;
/// Requests per second over all chats, with this many in a burst.
const RATE_PER_SEC: f64 = 40.0;
const RATE_BURST: f64 = 200.0;

/// The window event that carries a batch of normalised events.
pub const EVENTS_CHANNEL: &str = "harness-telemetry://events";

/// What the window set: the switch, the prompt text switch, and which
/// session ids are chats (helpers and probes are not).
#[derive(Default)]
pub(crate) struct Settings {
    pub(crate) enabled: bool,
    pub(crate) log_prompts: bool,
    pub(crate) chats: HashSet<String>,
}

#[derive(Clone)]
struct Holder {
    chat_id: String,
    harness: &'static str,
}

type Sink = Arc<dyn Fn(Vec<ActivityEvent>) + Send + Sync>;

/// One receiver: its port, its tokens, its counters.
pub struct Receiver {
    port: u16,
    /// sha256(token) to its chat. Looked up by hash, so a lookup's timing
    /// says nothing about the token itself.
    holders: Mutex<HashMap<[u8; 32], Holder>>,
    /// (chat, harness) to token, oldest first.
    tokens: Mutex<VecDeque<((String, &'static str), String)>>,
    settings: Arc<Mutex<Settings>>,
    sink: Sink,
    open: AtomicUsize,
    rate: Mutex<(f64, Instant)>,
    pub counters: Counters,
}

#[derive(Default)]
pub struct Counters {
    pub requests: AtomicU64,
    pub events: AtomicU64,
    pub refused_token: AtomicU64,
    pub refused_other: AtomicU64,
    pub dropped_records: AtomicU64,
}

static SETTINGS: OnceLock<Arc<Mutex<Settings>>> = OnceLock::new();
static RECEIVER: OnceLock<Result<Arc<Receiver>, String>> = OnceLock::new();

fn settings() -> Arc<Mutex<Settings>> {
    SETTINGS
        .get_or_init(|| Arc::new(Mutex::new(Settings::default())))
        .clone()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl Receiver {
    /// Binds 127.0.0.1 on a free port and serves on its own thread.
    pub(crate) fn start(settings: Arc<Mutex<Settings>>, sink: Sink) -> Result<Arc<Self>, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("telemetry receiver could not listen: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("telemetry receiver has no port: {e}"))?
            .port();
        let receiver = Arc::new(Self {
            port,
            holders: Mutex::new(HashMap::new()),
            tokens: Mutex::new(VecDeque::new()),
            settings,
            sink,
            open: AtomicUsize::new(0),
            rate: Mutex::new((RATE_BURST, Instant::now())),
            counters: Counters::default(),
        });
        let serving = receiver.clone();
        std::thread::Builder::new()
            .name("otel-receiver".into())
            .spawn(move || serving.serve(listener))
            .map_err(|e| format!("telemetry receiver could not start: {e}"))?;
        Ok(receiver)
    }

    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The token for a chat's harness: the same one across restarts, while
    /// it is among the newest `MAX_TOKENS`.
    pub fn token_for(&self, chat_id: &str, harness: &'static str) -> Option<String> {
        let mut tokens = self.tokens.lock().ok()?;
        let mut holders = self.holders.lock().ok()?;
        let key = (chat_id.to_string(), harness);
        if let Some((_, token)) = tokens.iter().find(|(k, _)| *k == key) {
            return Some(token.clone());
        }
        while tokens.len() >= MAX_TOKENS {
            if let Some((_, old)) = tokens.pop_front() {
                holders.remove(&hash(&old));
            }
        }
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        holders.insert(
            hash(&token),
            Holder {
                chat_id: chat_id.to_string(),
                harness,
            },
        );
        tokens.push_back((key, token.clone()));
        Some(token)
    }

    fn holder(&self, token: &str) -> Option<Holder> {
        self.holders.lock().ok()?.get(&hash(token)).cloned()
    }

    fn take_rate(&self) -> bool {
        let Ok(mut rate) = self.rate.lock() else {
            return false;
        };
        let now = Instant::now();
        let refill = now.saturating_duration_since(rate.1).as_secs_f64() * RATE_PER_SEC;
        rate.0 = (rate.0 + refill).min(RATE_BURST);
        rate.1 = now;
        if rate.0 >= 1.0 {
            rate.0 -= 1.0;
            true
        } else {
            false
        }
    }

    fn serve(self: Arc<Self>, listener: TcpListener) {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if !stream.peer_addr().is_ok_and(|a| a.ip().is_loopback()) {
                continue;
            }
            if self.open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                self.open.fetch_sub(1, Ordering::SeqCst);
                let mut stream = stream;
                let _ = respond(&mut stream, 503, None);
                // On the accept thread: drain only what is already here.
                close_without_waiting(&stream);
                continue;
            }
            let slot = OpenSlot(self.clone());
            let receiver = self.clone();
            let _ = std::thread::Builder::new()
                .name("otel-receiver-conn".into())
                .spawn(move || {
                    let _slot = slot;
                    let mut stream = stream;
                    let status = receiver.handle(&stream);
                    let (code, kind) = match status {
                        Ok(kind) => (200, Some(kind)),
                        Err(code) => {
                            if code == 401 {
                                receiver
                                    .counters
                                    .refused_token
                                    .fetch_add(1, Ordering::Relaxed);
                            } else {
                                receiver
                                    .counters
                                    .refused_other
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            (code, None)
                        }
                    };
                    let _ = respond(&mut stream, code, kind);
                    if code != 200 {
                        close_gently(&stream);
                    }
                });
        }
    }

    /// One request. `Ok` holds the body kind to answer with.
    fn handle(&self, stream: &TcpStream) -> Result<BodyKind, u16> {
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let mut reader = BufReader::new(Armed::new(stream, Instant::now() + HEAD_DEADLINE));
        let head = read_head(&mut reader)?;
        let host_ok = head.header("host").is_some_and(|host| {
            host == format!("127.0.0.1:{}", self.port)
                || host.eq_ignore_ascii_case(&format!("localhost:{}", self.port))
        });
        if !host_ok {
            return Err(403);
        }
        if head.method != "POST" {
            return Err(405);
        }
        let path = head.target.split('?').next().unwrap_or("");
        let signal = match path {
            "/v1/logs" => Signal::Logs,
            "/v1/metrics" | "/v1/traces" => Signal::Other,
            _ => return Err(404),
        };
        // The token, before any of the body is read.
        let holder = token_from(&head)
            .and_then(|t| self.holder(&t))
            .ok_or(401u16)?;
        if !self.take_rate() {
            return Err(429);
        }
        if head
            .header("content-encoding")
            .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        {
            return Err(415);
        }
        let kind = match head.header("content-type").map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        }) {
            Some(t) if t == "application/json" => BodyKind::Json,
            Some(t) if t == "application/x-protobuf" || t == "application/protobuf" => {
                BodyKind::Protobuf
            }
            _ => return Err(415),
        };
        let body = read_body(&mut reader, &head, Instant::now() + BODY_DEADLINE)?;
        if signal == Signal::Other {
            return Ok(kind);
        }
        let (enabled, log_prompts) = match self.settings.lock() {
            Ok(s) => (s.enabled, s.log_prompts),
            Err(_) => (false, false),
        };
        let decoded = match kind {
            BodyKind::Json => decode::decode_json(&body),
            BodyKind::Protobuf => decode::decode_protobuf(&body),
        }
        .map_err(|_| 400u16)?;
        self.counters
            .dropped_records
            .fetch_add(decoded.dropped as u64, Ordering::Relaxed);
        if !enabled {
            return Ok(kind);
        }
        let received = now_ms();
        let events: Vec<ActivityEvent> = decoded
            .logs
            .iter()
            .filter_map(|log| {
                normalize::to_activity(log, &holder.chat_id, holder.harness, log_prompts, received)
            })
            .collect();
        if !events.is_empty() {
            self.counters
                .events
                .fetch_add(events.len() as u64, Ordering::Relaxed);
            (self.sink)(events);
        }
        Ok(kind)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signal {
    Logs,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BodyKind {
    Json,
    Protobuf,
}

struct OpenSlot(Arc<Receiver>);

impl Drop for OpenSlot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

fn token_from(head: &Head) -> Option<String> {
    if let Some(t) = head.header(harness_env::TOKEN_HEADER) {
        return Some(t.trim().to_string()).filter(|t| !t.is_empty());
    }
    let auth = head.header("authorization")?;
    let (scheme, token) = auth.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// The whole reply in one buffer and one `write_all`: written in pieces,
/// a close right after can leave the client holding only the first of them
/// (Mac test 0.5).
fn respond<W: Write>(stream: &mut W, status: u16, kind: Option<BodyKind>) -> std::io::Result<()> {
    let (content_type, body): (&str, &[u8]) = match kind {
        Some(BodyKind::Protobuf) => ("application/x-protobuf", b""),
        _ => ("application/json", b"{}"),
    };
    let mut reply = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    stream.write_all(&reply)?;
    stream.flush()
}

const LINGER_DEADLINE: Duration = Duration::from_secs(1);
const LINGER_BYTES: usize = 8 * 1024;

/// `close_gently` for the accept thread, which must not wait: whatever
/// input has already arrived is read, then the socket goes.
fn close_without_waiting(stream: &TcpStream) {
    if stream.shutdown(std::net::Shutdown::Write).is_err() || stream.set_nonblocking(true).is_err()
    {
        return;
    }
    let mut buf = [0u8; 1024];
    let mut drained = 0;
    let mut stream = stream;
    while drained < LINGER_BYTES {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

/// After an error reply, with the request maybe not all read: close the
/// write side, then read what the client still sends, for a short while,
/// until it closes. Closing at once with input unread resets the connection,
/// and the reset can cost the client the reply (RFC 9112 section 9.6).
/// `set_linger` is not stable, so this is done by hand.
fn close_gently(stream: &TcpStream) {
    if stream.shutdown(std::net::Shutdown::Write).is_err() {
        return;
    }
    let deadline = Instant::now() + LINGER_DEADLINE;
    let mut drained = 0;
    let mut buf = [0u8; 1024];
    let mut reader = Armed::new(stream, deadline);
    while drained < LINGER_BYTES {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

// ---------------------------------------------------------------- HTTP

struct Head {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The socket with a deadline re-armed before every single read, so a
/// client sending a byte at a time cannot hold a slot past the deadline.
struct Armed<'a> {
    stream: &'a TcpStream,
    deadline: std::cell::Cell<Instant>,
}

impl<'a> Armed<'a> {
    fn new(stream: &'a TcpStream, deadline: Instant) -> Self {
        Self {
            stream,
            deadline: std::cell::Cell::new(deadline),
        }
    }
}

impl Read for Armed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self
            .deadline
            .get()
            .saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

type Reader<'a> = BufReader<Armed<'a>>;

/// 408 when the read timed out, by the error's kind: a socket timeout is
/// `WouldBlock` or `TimedOut`, and `Armed` answers `TimedOut` itself when no
/// time is left. Re-reading the clock here could call a timeout that fired
/// a moment early a bad request. Anything else is a bad request.
fn read_failure(error: &std::io::Error) -> u16 {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => 408,
        _ => 400,
    }
}

fn read_head(reader: &mut Reader) -> Result<Head, u16> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let room = (MAX_HEAD_BYTES + 1).saturating_sub(head.len()) as u64;
        let n = reader
            .by_ref()
            .take(room)
            .read_until(b'\n', &mut line)
            .map_err(|e| read_failure(&e))?;
        if n == 0 {
            return Err(400);
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD_BYTES {
            return Err(413);
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let head = String::from_utf8(head).map_err(|_| 400u16)?;
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    Ok(Head {
        method,
        target,
        headers,
    })
}

fn read_body(reader: &mut Reader, head: &Head, deadline: Instant) -> Result<Vec<u8>, u16> {
    reader.get_ref().deadline.set(deadline);
    let chunked = head
        .header("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader
                .by_ref()
                .take(256)
                .read_line(&mut size_line)
                .map_err(|e| read_failure(&e))?;
            let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| 400u16)?;
            let total = body.len().checked_add(size).ok_or(413u16)?;
            if total > MAX_BODY_BYTES {
                return Err(413);
            }
            let mut chunk = vec![0; size.checked_add(2).ok_or(413u16)?];
            reader
                .read_exact(&mut chunk)
                .map_err(|e| read_failure(&e))?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..size]);
        }
    } else {
        let length: usize = head
            .header("content-length")
            .ok_or(411u16)?
            .parse()
            .map_err(|_| 400u16)?;
        if length > MAX_BODY_BYTES {
            return Err(413);
        }
        reader
            .by_ref()
            .take(length as u64)
            .read_to_end(&mut body)
            .map_err(|e| read_failure(&e))?;
        if body.len() < length {
            return Err(400);
        }
    }
    Ok(body)
}

// ---------------------------------------------------------------- the app

/// The app's receiver, started on first use with a sink that sends each
/// batch to the window.
fn receiver(app: &tauri::AppHandle) -> Result<Arc<Receiver>, String> {
    RECEIVER
        .get_or_init(|| {
            use tauri::Emitter;
            let app = app.clone();
            let sink: Sink = Arc::new(move |events| {
                let _ = app.emit(EVENTS_CHANNEL, events);
            });
            Receiver::start(settings(), sink)
        })
        .clone()
}

/// The effective `CODEX_HOME` for a spawn: what the command sets, else the
/// app's environment, else `~/.codex`.
fn codex_home(cmd: &Command) -> Option<std::path::PathBuf> {
    let set = cmd
        .get_envs()
        .find(|(k, _)| *k == "CODEX_HOME")
        .and_then(|(_, v)| v.map(std::path::PathBuf::from));
    set.or_else(|| std::env::var_os("CODEX_HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".codex")))
}

/// A variable as the child will see it: set on the command, else inherited.
fn child_var(cmd: &Command, name: &str) -> Option<String> {
    for (k, v) in cmd.get_envs() {
        if k == name {
            return v.map(|v| v.to_string_lossy().into_owned());
        }
    }
    std::env::var(name).ok()
}

/// Called in `harness_spawn` before the child starts. Points a chat's
/// Claude Code or Codex at the receiver, or does nothing. Never fails the
/// spawn: a receiver that cannot start leaves the chat with its adapter
/// events only (corner case 9).
pub fn apply_to_child(
    app: &tauri::AppHandle,
    session_id: &str,
    binary_provider: Option<&str>,
    args: &[String],
    cmd: &mut Command,
) {
    let Some(harness) = harness_env::telemetry_harness(binary_provider, args) else {
        return;
    };
    let (enabled, log_prompts, is_chat) = match settings().lock() {
        Ok(s) => (s.enabled, s.log_prompts, s.chats.contains(session_id)),
        Err(_) => return,
    };
    if !enabled || !is_chat {
        return;
    }
    if harness_env::has_own_env_telemetry(&|name| child_var(cmd, name)) {
        return;
    }
    let mut codex_config = None;
    if harness == "codex" {
        codex_config =
            codex_home(cmd).and_then(|home| std::fs::read_to_string(home.join("config.toml")).ok());
        if harness_env::has_own_codex_otel(codex_config.as_deref()) {
            return;
        }
    }
    let Ok(receiver) = receiver(app) else { return };
    let Some(token) = receiver.token_for(session_id, harness) else {
        return;
    };
    let endpoint = receiver.endpoint();
    if harness == "claude" {
        let (set, remove) = harness_env::claude_env(&endpoint, log_prompts);
        for name in remove {
            cmd.env_remove(name);
        }
        for (k, v) in set {
            cmd.env(k, v);
        }
        // The token through `otelHeadersHelper`, as Claude Code's docs ask;
        // the header variable only where the script cannot be made.
        let settings = helper_for(app, &token)
            .and_then(|helper| harness_env::settings_with_helper(args, &helper.to_string_lossy()));
        match settings {
            Some(settings) => {
                cmd.env_remove("OTEL_EXPORTER_OTLP_HEADERS");
                cmd.args(["--settings", &settings]);
            }
            None => {
                let (k, v) = harness_env::claude_header_var(&token);
                cmd.env(k, v);
            }
        }
    } else {
        cmd.args(harness_env::codex_args(&endpoint, log_prompts));
        cmd.args(harness_env::codex_shell_env_args(codex_config.as_deref()));
        for (k, v) in harness_env::codex_env(&token) {
            cmd.env(k, v);
        }
    }
}

/// This app run's helper scripts, by token hash. The folder is emptied on
/// first use, so a token from an earlier run is never left on disk past it.
/// The folder, once made, and each token's script.
#[derive(Default)]
struct Helpers {
    dir: Option<std::path::PathBuf>,
    scripts: HashMap<[u8; 32], std::path::PathBuf>,
}

static HELPERS: OnceLock<Mutex<Helpers>> = OnceLock::new();

/// The chat's `otelHeadersHelper` script, written once per app run.
#[cfg(unix)]
fn helper_for(app: &tauri::AppHandle, token: &str) -> Option<std::path::PathBuf> {
    use tauri::Manager;
    let helpers = HELPERS.get_or_init(Default::default);
    let mut helpers = helpers.lock().ok()?;
    if let Some(path) = helpers.scripts.get(&hash(token)) {
        return Some(path.clone());
    }
    let dir = match &helpers.dir {
        Some(dir) => dir.clone(),
        None => {
            let dir = app.path().app_data_dir().ok()?.join("otel-helper");
            fresh_private_dir(&dir).ok()?;
            helpers.dir = Some(dir.clone());
            dir
        }
    };
    let path = write_helper(&dir, token).ok()?;
    helpers.scripts.insert(hash(token), path.clone());
    Some(path)
}

#[cfg(not(unix))]
fn helper_for(_app: &tauri::AppHandle, _token: &str) -> Option<std::path::PathBuf> {
    None
}

/// Removes this run's helper folder. Called once at quit (security review
/// X12): the scripts print the chats' tokens, so none outlives the app run.
/// The wipe at first use stays for a run that ended without quitting.
pub fn remove_helpers() {
    if let Some(helpers) = HELPERS.get() {
        let mut helpers = match helpers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        clear_helpers(&mut helpers);
    }
}

/// Removes the folder (a link there is removed, never followed) and forgets
/// its scripts.
fn clear_helpers(helpers: &mut Helpers) {
    if let Some(dir) = helpers.dir.take() {
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {
                let _ = std::fs::remove_dir_all(&dir);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&dir);
            }
            Err(_) => {}
        }
    }
    helpers.scripts.clear();
}

/// Empties `dir` (a link there is removed, never followed) and makes it
/// again, 0700.
#[cfg(unix)]
fn fresh_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(dir)?,
        Ok(_) => std::fs::remove_file(dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

/// One chat's script in `dir`: a new file (never an existing one or a link),
/// 0700, named by a hash so the name says nothing of the token.
#[cfg(unix)]
fn write_helper(dir: &std::path::Path, token: &str) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let name: String = hash(token)[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let path = dir.join(format!("{name}.sh"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    file.write_all(harness_env::headers_helper_script(token).as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

/// The switch, the prompt text switch, and which sessions are chats. Takes
/// effect at a harness's next start; the window restarts it when the
/// switches change.
#[tauri::command]
pub fn harness_telemetry_configure(
    enabled: bool,
    log_prompts: bool,
    chats: Vec<String>,
) -> Result<(), String> {
    let chats: HashSet<String> = chats
        .into_iter()
        .filter(|c| !c.is_empty() && c.len() <= 200)
        .take(MAX_CHATS)
        .collect();
    let state = settings();
    let mut s = state.lock().map_err(|_| "telemetry settings lock")?;
    s.enabled = enabled;
    s.log_prompts = log_prompts;
    s.chats = chats;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatus {
    listening: bool,
    requests: u64,
    events: u64,
    refused_token: u64,
    refused_other: u64,
    dropped_records: u64,
}

/// For Settings: whether the receiver runs and what it has seen. No port and
/// no token.
#[tauri::command]
pub fn harness_telemetry_status() -> TelemetryStatus {
    match RECEIVER.get() {
        Some(Ok(r)) => TelemetryStatus {
            listening: true,
            requests: r.counters.requests.load(Ordering::Relaxed),
            events: r.counters.events.load(Ordering::Relaxed),
            refused_token: r.counters.refused_token.load(Ordering::Relaxed),
            refused_other: r.counters.refused_other.load(Ordering::Relaxed),
            dropped_records: r.counters.dropped_records.load(Ordering::Relaxed),
        },
        _ => TelemetryStatus {
            listening: false,
            requests: 0,
            events: 0,
            refused_token: 0,
            refused_other: 0,
            dropped_records: 0,
        },
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

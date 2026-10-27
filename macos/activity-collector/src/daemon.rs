//! The daemon's moving parts: the eslogger pipeline, the two socket
//! servers, eslogger supervision and log maintenance. Everything here is
//! plain code that builds and is tested on any Unix; the macOS-only caller
//! check is passed in as a closure (see `main.rs`).

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::event::{self, ActivityEvent, RawKind};
use crate::filter;
use crate::lines::{self, Line};
use crate::log::{self, LogWriter};
use crate::protocol::{self, EsloggerState, ReadRequest, RegisterRequest, Status};
use crate::retention;
use crate::timefmt;
use crate::tree::{TrackError, Tracker};

/// The fixed eslogger binary.
pub const ESLOGGER: &str = "/usr/bin/eslogger";
/// Default socket folder.
pub const SOCKET_DIR: &str = "/var/run/monocode-activity";
/// Default log folder.
pub const LOG_DIR: &str = "/Library/Application Support/MonoCode/activity";
/// An eslogger line longer than this is skipped.
pub const MAX_ESLOGGER_LINE: usize = 1 << 20;
/// Lines queued between the eslogger reader and the processor.
pub const CHANNEL_CAP: usize = 4096;
/// Concurrent socket connections, both sockets together.
pub const MAX_CONNECTIONS: usize = 8;
/// Socket read and write timeout.
pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
/// At most this many characters of eslogger's last stderr line.
pub const MAX_STDERR_CHARS: usize = 300;

/// How often retention runs.
pub const MAINTENANCE_EVERY: Duration = Duration::from_secs(3600);

/// Locks a mutex, recovering from poisoning instead of panicking.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One eslogger line through the tracker and the filter: the event to log,
/// or `None`. The tracker sees every parsed event, kept or not, so forks
/// and exits keep it current.
pub fn process_line(tracker: &mut Tracker, line: &str) -> Option<ActivityEvent> {
    let raw = event::parse_eslogger_line(line)?;
    let attribution = tracker.observe(&raw)?;
    filter::to_activity(&raw, &attribution)
}

/// Applies one registration request to the tracker.
pub fn apply_register(tracker: &mut Tracker, req: &RegisterRequest) -> Result<(), &'static str> {
    let result = match req {
        RegisterRequest::Register {
            pid,
            chat_id,
            kind,
            at,
        } => tracker.register(*pid, chat_id, *kind, *at),
        RegisterRequest::Grant {
            chat_id,
            bundle_id,
            app_path,
            at,
        } => tracker.grant(chat_id, bundle_id, app_path.as_deref(), *at),
        RegisterRequest::GrantEnded {
            chat_id,
            bundle_id,
            at,
        } => {
            tracker.grant_ended(chat_id, bundle_id, *at);
            Ok(())
        }
    };
    result.map_err(|TrackError::Full| "full")
}

/// The event names eslogger offers (`--list-events` output, one a line)
/// split into the protocol's events that are offered and those missing.
pub fn split_events(list_output: &str) -> (Vec<String>, Vec<String>) {
    let offered: BTreeSet<&str> = list_output.lines().map(str::trim).collect();
    let mut kept = Vec::new();
    let mut missing = Vec::new();
    for kind in RawKind::ALL {
        let name = kind.es_name().to_owned();
        if offered.contains(name.as_str()) {
            kept.push(name);
        } else {
            missing.push(name);
        }
    }
    (kept, missing)
}

/// The restart delay after `current`: doubling, at most 60 s.
pub fn next_backoff(current: Duration) -> Duration {
    current
        .checked_mul(2)
        .unwrap_or(Duration::from_secs(60))
        .min(Duration::from_secs(60))
}

/// Reads eslogger's output into the channel. A full channel or an
/// over-long line counts as a drop. Returns at end of input.
pub fn pump<R: BufRead>(mut reader: R, tx: &SyncSender<String>, dropped: &AtomicU64) {
    let mut buf = Vec::new();
    loop {
        match lines::read_capped(&mut reader, &mut buf, MAX_ESLOGGER_LINE) {
            Ok(Line::Eof) | Err(_) => return,
            Ok(Line::TooLong) => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Line::Complete | Line::Cut) => {
                let Ok(text) = std::str::from_utf8(&buf) else {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                match tx.try_send(text.to_owned()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        }
    }
}

/// eslogger's state, for `status`.
#[derive(Debug)]
struct EsInfo {
    state: EsloggerState,
    events: Vec<String>,
    missing: Vec<String>,
}

/// One stderr line made safe to show: control characters dropped, at most
/// 300 characters.
fn printable_line(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_control())
        .take(MAX_STDERR_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Reads eslogger's stderr to its end, keeping the last non-empty line in
/// `slot`. Lines are read capped, so a flood cannot grow memory.
pub fn keep_last_line<R: BufRead>(mut reader: R, slot: &Mutex<Option<String>>) {
    let mut buf = Vec::new();
    loop {
        match lines::read_capped(&mut reader, &mut buf, 4096) {
            Ok(Line::Eof) | Err(_) => return,
            Ok(_) => {
                let line = printable_line(&String::from_utf8_lossy(&buf));
                if !line.is_empty() {
                    *lock(slot) = Some(line);
                }
            }
        }
    }
}

/// State shared by the daemon's threads.
#[derive(Debug)]
pub struct Shared {
    pub tracker: Mutex<Tracker>,
    pub writer: Mutex<LogWriter>,
    pub log_dir: PathBuf,
    pub dropped: AtomicU64,
    connections: AtomicUsize,
    es: Mutex<EsInfo>,
    /// eslogger's last stderr line, shown while it is not running.
    es_error: Mutex<Option<String>>,
}

impl Shared {
    /// Opens the log (resuming its chain) with an empty tracker.
    pub fn open(log_dir: &Path) -> io::Result<Shared> {
        Ok(Shared {
            tracker: Mutex::new(Tracker::new()),
            writer: Mutex::new(LogWriter::open(log_dir)?),
            log_dir: log_dir.to_path_buf(),
            dropped: AtomicU64::new(0),
            connections: AtomicUsize::new(0),
            es: Mutex::new(EsInfo {
                state: EsloggerState::Stopped,
                events: Vec::new(),
                missing: Vec::new(),
            }),
            es_error: Mutex::new(None),
        })
    }

    fn set_es(&self, state: EsloggerState, kept: Option<(Vec<String>, Vec<String>)>) {
        let mut es = lock(&self.es);
        es.state = state;
        if let Some((events, missing)) = kept {
            es.events = events;
            es.missing = missing;
        }
    }

    /// The `status` answer.
    pub fn status(&self) -> Status {
        let (tracked, grants) = {
            let t = lock(&self.tracker);
            (t.tracked_count(), t.grant_count())
        };
        let es = lock(&self.es);
        // The last complaint matters only while eslogger is not running.
        let eslogger_error = match es.state {
            EsloggerState::Running => None,
            _ => lock(&self.es_error).clone(),
        };
        let log_bytes = log::list_logs(&self.log_dir)
            .map(|files| files.iter().fold(0u64, |a, f| a.saturating_add(f.bytes)))
            .unwrap_or(0);
        Status {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            eslogger: es.state,
            eslogger_error,
            events: es.events.clone(),
            missing_events: es.missing.clone(),
            tracked: tracked as u64,
            grants: grants as u64,
            dropped: self.dropped.load(Ordering::Relaxed),
            log_bytes,
        }
    }

    /// Processes and logs one eslogger line; compresses a day that just
    /// rolled over. Returns the logged event.
    pub fn ingest(&self, line: &str) -> Option<ActivityEvent> {
        let ev = process_line(&mut lock(&self.tracker), line)?;
        let (logged, finished) = {
            let mut w = lock(&self.writer);
            let logged = w.append(ev);
            (logged, w.take_finished())
        };
        for day in finished {
            let _ = retention::compress(&self.log_dir, day);
        }
        match logged {
            Ok(ev) => Some(ev),
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Compresses finished plain days and applies retention. The day the
    /// writer is on is never compressed or deleted.
    pub fn maintain(&self, now_ms: i64) {
        let current = lock(&self.writer).current_day();
        if let (Some(current), Ok(files)) = (current, log::list_logs(&self.log_dir)) {
            for f in files.iter().filter(|f| !f.compressed && f.day < current) {
                let _ = retention::compress(&self.log_dir, f.day);
            }
        }
        let _ = retention::apply(
            &self.log_dir,
            timefmt::day_of_ms(now_ms),
            current,
            retention::KEEP_DAYS,
            retention::CAP_BYTES,
        );
    }
}

/// Which socket a connection came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketKind {
    Register,
    Read,
}

/// Handles one registration connection: up to 64 request lines.
pub fn handle_register<R: BufRead, W: Write>(shared: &Shared, mut reader: R, mut writer: W) {
    let mut buf = Vec::new();
    for _ in 0..protocol::MAX_REQUESTS_PER_CONNECTION {
        let req = match lines::read_capped(&mut reader, &mut buf, protocol::MAX_REQUEST_BYTES) {
            Ok(Line::Complete) => protocol::parse_register(&buf),
            Ok(Line::Eof) | Err(_) => return,
            Ok(Line::TooLong) => Err("too long"),
            Ok(Line::Cut) => Err("no newline"),
        };
        let (reply, close) = match req {
            Ok(req) => match apply_register(&mut lock(&shared.tracker), &req) {
                Ok(()) => (protocol::reply_ok(), false),
                Err(e) => (protocol::reply_error(e), false),
            },
            Err(e) => (protocol::reply_error(e), true),
        };
        if writer.write_all(reply.as_bytes()).is_err() || close {
            return;
        }
    }
}

/// Handles one read connection: one request.
pub fn handle_read<R: BufRead, W: Write>(shared: &Shared, mut reader: R, mut writer: W) {
    let mut buf = Vec::new();
    let req = match lines::read_capped(&mut reader, &mut buf, protocol::MAX_REQUEST_BYTES) {
        Ok(Line::Complete) => protocol::parse_read(&buf),
        Ok(Line::Eof) | Err(_) => return,
        Ok(Line::TooLong) => Err("too long"),
        Ok(Line::Cut) => Err("no newline"),
    };
    let result = match req {
        Err(e) => writer.write_all(protocol::reply_error(e).as_bytes()),
        Ok(ReadRequest::Status) => {
            writer.write_all(protocol::reply_status(&shared.status()).as_bytes())
        }
        Ok(ReadRequest::Events {
            since,
            until,
            chat_id,
            limit,
        }) => match log::read_events(&shared.log_dir, since, until, &chat_id, limit) {
            Err(_) => writer.write_all(protocol::reply_error("log unreadable").as_bytes()),
            Ok(res) => (|| {
                let mut out = io::BufWriter::new(&mut writer);
                for ev in &res.events {
                    out.write_all(protocol::reply_event(ev).as_bytes())?;
                }
                out.write_all(protocol::reply_end(res.truncated, res.chain_ok).as_bytes())?;
                out.flush()
            })(),
        },
    };
    let _ = result.and_then(|()| writer.flush());
}

/// The caller check: true when the peer on this stream may talk.
pub type Allow = dyn Fn(&UnixStream) -> bool + Send + Sync;

struct ConnGuard<'a>(&'a AtomicUsize);

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Takes one connection slot; the caller releases it with a [`ConnGuard`].
fn try_acquire(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_CONNECTIONS).then_some(n + 1)
        })
        .is_ok()
}

/// Serves one connection: timeouts, the caller check, then the handler.
pub fn handle_connection(shared: &Shared, allow: &Allow, kind: SocketKind, stream: UnixStream) {
    if stream.set_read_timeout(Some(SOCKET_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(SOCKET_TIMEOUT)).is_err()
    {
        return;
    }
    let mut writer = &stream;
    if !allow(&stream) {
        let _ = writer.write_all(protocol::reply_error("refused").as_bytes());
        return;
    }
    let reader = BufReader::new(&stream);
    match kind {
        SocketKind::Register => handle_register(shared, reader, writer),
        SocketKind::Read => handle_read(shared, reader, writer),
    }
}

/// Accepts connections forever, one thread each, at most 8 at a time.
pub fn serve(listener: UnixListener, shared: Arc<Shared>, allow: Arc<Allow>, kind: SocketKind) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        if !try_acquire(&shared.connections) {
            continue; // Dropping the stream closes it.
        }
        let shared2 = Arc::clone(&shared);
        let allow2 = Arc::clone(&allow);
        let spawned = thread::Builder::new()
            .name("collector-conn".into())
            .spawn(move || {
                let _guard = ConnGuard(&shared2.connections);
                handle_connection(&shared2, &*allow2, kind, stream);
            });
        if spawned.is_err() {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Creates a folder (or checks an existing one) that is a real directory,
/// not a symlink, and sets its mode.
pub fn ensure_plain_dir(dir: &Path, mode: u32) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err(io::Error::other("not a plain directory"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir_all(dir)?,
        Err(e) => return Err(e),
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(mode))
}

/// `sun_path` holds 104 bytes on macOS (xnu `bsd/sys/un.h`) and 108 on
/// Linux (unix(7)), the NUL included. The smaller one holds everywhere.
pub const MAX_SOCKET_PATH: usize = 104;

/// A socket path that fits `sun_path` on macOS, with its NUL.
pub fn check_socket_path(path: &Path) -> io::Result<()> {
    let len = path.as_os_str().len();
    if len >= MAX_SOCKET_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path is {len} bytes; it must be under {MAX_SOCKET_PATH} (sun_path on macOS): {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Binds a socket at `path` (mode 0666; every caller is checked),
/// replacing only a stale socket, never another kind of file.
pub fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    check_socket_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => return Err(io::Error::other("socket path is taken")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Runs eslogger forever, restarting it with backoff when it exits.
pub fn supervise(shared: &Shared, program: &Path, tx: &SyncSender<String>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let listed = Command::new(program)
            .arg("--list-events")
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        let split = match listed {
            Ok(out) if out.status.success() => {
                Some(split_events(&String::from_utf8_lossy(&out.stdout)))
            }
            Ok(out) => {
                keep_last_line(&out.stderr[..], &shared.es_error);
                None
            }
            Err(_) => None,
        };
        let Some((kept, missing)) = split.filter(|(kept, _)| !kept.is_empty()) else {
            shared.set_es(EsloggerState::Missing, None);
            thread::sleep(backoff);
            backoff = next_backoff(backoff);
            continue;
        };
        let started = Instant::now();
        let child = Command::new(program)
            .args(&kept)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match child {
            Ok(mut child) => {
                shared.set_es(EsloggerState::Running, Some((kept, missing)));
                // eslogger says why it stopped on stderr, such as "Caller
                // lacks TCC authorization for Full Disk Access" (Mac test 3.4).
                // Scoped threads join at the end of the scope.
                thread::scope(|scope| {
                    if let Some(err) = child.stderr.take() {
                        let slot = &shared.es_error;
                        scope.spawn(move || keep_last_line(BufReader::new(err), slot));
                    }
                    if let Some(stdout) = child.stdout.take() {
                        pump(BufReader::new(stdout), tx, &shared.dropped);
                    }
                    let _ = child.kill();
                });
                let _ = child.wait();
                shared.set_es(EsloggerState::Stopped, None);
            }
            Err(_) => shared.set_es(EsloggerState::Missing, Some((kept, missing))),
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        thread::sleep(backoff);
        backoff = next_backoff(backoff);
    }
}

/// Logs every line the channel delivers.
pub fn process_all(shared: &Shared, rx: Receiver<String>) {
    for line in rx {
        shared.ingest(&line);
    }
}

/// Where the daemon keeps things.
#[derive(Clone, Debug)]
pub struct Config {
    pub log_dir: PathBuf,
    pub socket_dir: PathBuf,
    pub eslogger: PathBuf,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            log_dir: PathBuf::from(LOG_DIR),
            socket_dir: PathBuf::from(SOCKET_DIR),
            eslogger: PathBuf::from(ESLOGGER),
        }
    }
}

/// Runs the daemon. Returns only on a setup error.
pub fn run(config: &Config, allow: Arc<Allow>) -> io::Result<()> {
    ensure_plain_dir(&config.socket_dir, 0o755)?;
    let shared = Arc::new(Shared::open(&config.log_dir)?);
    shared.maintain(timefmt::now_ms());
    let register = bind_socket(&config.socket_dir.join("register.sock"))?;
    let read = bind_socket(&config.socket_dir.join("read.sock"))?;

    let (tx, rx) = mpsc::sync_channel::<String>(CHANNEL_CAP);
    let s = Arc::clone(&shared);
    thread::Builder::new()
        .name("collector-process".into())
        .spawn(move || process_all(&s, rx))?;
    let s = Arc::clone(&shared);
    thread::Builder::new()
        .name("collector-maintain".into())
        .spawn(move || loop {
            thread::sleep(MAINTENANCE_EVERY);
            s.maintain(timefmt::now_ms());
        })?;
    for (listener, kind) in [(register, SocketKind::Register), (read, SocketKind::Read)] {
        let s = Arc::clone(&shared);
        let a = Arc::clone(&allow);
        thread::Builder::new()
            .name("collector-listen".into())
            .spawn(move || serve(listener, s, a, kind))?;
    }
    supervise(&shared, &config.eslogger, &tx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::tests::temp_dir;
    use std::io::{Cursor, Read};

    /// A short folder for sockets: `temp_dir()` on macOS is too long for
    /// `sun_path` (Mac test 0.6).
    fn socket_dir() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(format!(
            "/tmp/mc-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_socket_path_too_long_for_macos_is_refused_with_a_clear_error() {
        let dir = socket_dir();
        ensure_plain_dir(&dir, 0o755).unwrap();
        let name = "s".repeat(MAX_SOCKET_PATH - 1 - dir.as_os_str().len());
        let long = dir.join(name);
        assert_eq!(long.as_os_str().len(), MAX_SOCKET_PATH);
        let err = bind_socket(&long).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("must be under 104"), "{err}");
        let fits = dir.join("s".repeat(MAX_SOCKET_PATH - 2 - dir.as_os_str().len()));
        assert!(bind_socket(&fits).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn splits_offered_events() {
        let (kept, missing) = split_events(
            "exec\nfork\n exit \ncreate\nrename\nunlink\nclose\nopen\nmmap\ntruncate\nlink\ncopyfile\nclone\nexchangedata\nwrite\n",
        );
        assert_eq!(kept.len(), 13);
        assert_eq!(missing, vec!["tcc_modify"]);
        // `write` is offered but never asked for (note AA, appendix A).
        assert!(!kept.contains(&"write".to_owned()));
        let (kept, missing) = split_events("");
        assert!(kept.is_empty());
        assert_eq!(missing.len(), RawKind::ALL.len());
    }

    #[test]
    fn backoff_doubles_to_sixty() {
        let mut d = Duration::from_secs(1);
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(d.as_secs());
            d = next_backoff(d);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn pump_counts_drops() {
        let (tx, rx) = mpsc::sync_channel(2);
        let dropped = AtomicU64::new(0);
        let mut input = String::from("a\nb\nc\n");
        input.push_str(&"x".repeat(MAX_ESLOGGER_LINE + 1));
        input.push('\n');
        pump(Cursor::new(input.into_bytes()), &tx, &dropped);
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    fn register_session(shared: &Shared, input: &str) -> String {
        let mut out = Vec::new();
        handle_register(shared, Cursor::new(input.as_bytes().to_vec()), &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn registration_connection() {
        let dir = temp_dir("daemon-reg");
        let shared = Shared::open(&dir).unwrap();
        let out = register_session(
            &shared,
            "{\"v\":1,\"op\":\"register\",\"pid\":7,\"chatId\":\"c\",\"kind\":\"harness\",\"at\":1}\n\
             {\"v\":1,\"op\":\"grant\",\"chatId\":\"c\",\"bundleId\":\"x\",\"at\":1}\n\
             bad\n\
             {\"v\":1,\"op\":\"grantEnded\",\"chatId\":\"c\",\"bundleId\":\"x\",\"at\":1}\n",
        );
        assert_eq!(
            out,
            "{\"ok\":true}\n{\"ok\":true}\n{\"ok\":false,\"error\":\"bad json\"}\n"
        );
        assert_eq!(lock(&shared.tracker).root_count(), 1);
        assert_eq!(lock(&shared.tracker).grant_count(), 1);
        // 64 lines at most; the 65th is not read.
        let line = "{\"v\":1,\"op\":\"grantEnded\",\"chatId\":\"c\",\"bundleId\":\"x\",\"at\":1}\n";
        let out = register_session(&shared, &line.repeat(70));
        assert_eq!(out.lines().count(), 64);
        let out = register_session(&shared, "{\"v\":1,\"op\":\"grantEnded\"");
        assert_eq!(out, "{\"ok\":false,\"error\":\"no newline\"}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ingest_then_read_and_status() {
        let dir = temp_dir("daemon-read");
        let shared = Shared::open(&dir).unwrap();
        register_session(
            &shared,
            "{\"v\":1,\"op\":\"register\",\"pid\":7,\"chatId\":\"c\",\"kind\":\"harness\",\"at\":1}\n",
        );
        let line = r#"{"event_type":9,"time":"2025-10-09T12:00:00Z","process":{"audit_token":{"pid":7},"ppid":1,"executable":{"path":"/bin/zsh"}},"event":{"exec":{"target":{"executable":{"path":"/bin/ls"}},"args":["ls"]}}}"#;
        let ev = shared.ingest(line).unwrap();
        assert_eq!(ev.id, "es-2025-10-09-1");
        assert!(shared
            .ingest(&line.replace("\"pid\":7", "\"pid\":8"))
            .is_none());

        let mut out = Vec::new();
        let req =
            "{\"v\":1,\"op\":\"events\",\"since\":0,\"until\":9999999999999,\"chatId\":\"c\"}\n";
        handle_read(&shared, Cursor::new(req.as_bytes().to_vec()), &mut out);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let back: ActivityEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(back, ev);
        assert_eq!(
            lines[1],
            "{\"end\":true,\"truncated\":false,\"chainOk\":true}"
        );

        let mut out = Vec::new();
        handle_read(
            &shared,
            Cursor::new(b"{\"v\":1,\"op\":\"status\"}\n".to_vec()),
            &mut out,
        );
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["tracked"], 1);
        assert_eq!(v["eslogger"], "stopped");
        assert!(v["logBytes"].as_u64().unwrap() > 0);

        let mut out = Vec::new();
        handle_read(
            &shared,
            Cursor::new(b"{\"v\":1,\"op\":\"register\"}\n".to_vec()),
            &mut out,
        );
        assert_eq!(out, b"{\"ok\":false,\"error\":\"unknown op\"}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sockets_check_callers_and_cap_connections() {
        let dir = socket_dir();
        let shared = Arc::new(Shared::open(&dir.join("log")).unwrap());
        ensure_plain_dir(&dir.join("run"), 0o755).unwrap();
        let path = dir.join("run").join("read.sock");
        let listener = bind_socket(&path).unwrap();
        let mode = fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o666);
        // A stale socket is replaced; a regular file is not.
        drop(listener);
        let listener = bind_socket(&path).unwrap();
        fs::write(dir.join("run").join("file"), "x").unwrap();
        assert!(bind_socket(&dir.join("run").join("file")).is_err());

        let refuse_all: Arc<Allow> = Arc::new(|_: &UnixStream| false);
        let s = Arc::clone(&shared);
        thread::spawn(move || serve(listener, s, refuse_all, SocketKind::Read));
        // The refusal is sent before any request is read.
        let mut c = UnixStream::connect(&path).unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "{\"ok\":false,\"error\":\"refused\"}\n");

        let path2 = dir.join("run").join("register.sock");
        let listener = bind_socket(&path2).unwrap();
        let allow_all: Arc<Allow> = Arc::new(|_: &UnixStream| true);
        let s = Arc::clone(&shared);
        thread::spawn(move || serve(listener, s, allow_all, SocketKind::Register));
        // Fill the 8 slots with idle connections; the 9th is closed at once.
        let wait_for = |n: usize| {
            for _ in 0..500 {
                if shared.connections.load(Ordering::Acquire) == n {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("connections never reached {n}");
        };
        // The refused connection releases its slot just after closing.
        wait_for(0);
        let idle: Vec<UnixStream> = (0..MAX_CONNECTIONS)
            .map(|_| UnixStream::connect(&path2).unwrap())
            .collect();
        wait_for(MAX_CONNECTIONS);
        let mut extra = UnixStream::connect(&path2).unwrap();
        extra
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut sink = Vec::new();
        assert_eq!(extra.read_to_end(&mut sink).unwrap(), 0);
        drop(idle);
        thread::sleep(Duration::from_millis(200));
        let mut ok = UnixStream::connect(&path2).unwrap();
        ok.write_all(b"{\"v\":1,\"op\":\"register\",\"pid\":3,\"chatId\":\"c\",\"kind\":\"harness\",\"at\":1}\n")
            .unwrap();
        ok.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        ok.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "{\"ok\":true}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_reports_esloggers_last_stderr_line() {
        // Mac test 3.4: the page said only "eslogger is stopped" when the
        // reason was Full Disk Access.
        let dir = temp_dir("daemon-stderr");
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-eslogger");
        fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = --list-events ]; then echo exec; exit 0; fi\necho 'starting' >&2\necho 'Caller lacks TCC authorization for Full Disk Access' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let shared = Arc::new(Shared::open(&dir.join("log")).unwrap());
        let (tx, _rx) = mpsc::sync_channel(4);
        let s = Arc::clone(&shared);
        let program = script.clone();
        thread::spawn(move || supervise(&s, &program, &tx));
        let mut seen = serde_json::Value::Null;
        for _ in 0..300 {
            let v: serde_json::Value =
                serde_json::from_str(&protocol::reply_status(&shared.status())).unwrap();
            if !v["esloggerError"].is_null() {
                seen = v;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            seen["esloggerError"],
            "Caller lacks TCC authorization for Full Disk Access"
        );
        assert_eq!(seen["eslogger"], "stopped");
    }

    #[test]
    fn a_stderr_line_is_capped_and_printable() {
        let slot = Mutex::new(None);
        let long = format!("a\u{7}b{}\n", "x".repeat(2000));
        keep_last_line(Cursor::new(format!("first\n{long}\n").into_bytes()), &slot);
        let kept = lock(&slot).clone().unwrap();
        assert!(kept.starts_with("ab"));
        assert_eq!(kept.chars().count(), MAX_STDERR_CHARS);
    }

    #[test]
    fn maintain_compresses_finished_days_only() {
        let dir = temp_dir("daemon-maint");
        let shared = Shared::open(&dir).unwrap();
        register_session(
            &shared,
            "{\"v\":1,\"op\":\"register\",\"pid\":7,\"chatId\":\"c\",\"kind\":\"harness\",\"at\":1}\n",
        );
        let line = |t: &str| {
            format!(
                r#"{{"time":"{t}","process":{{"audit_token":{{"pid":7}}}},"event":{{"unlink":{{"target":{{"path":"/tmp/a"}}}}}}}}"#
            )
        };
        shared.ingest(&line("2025-10-08T23:59:59Z")).unwrap();
        // Rolling over compresses the finished day at once.
        shared.ingest(&line("2025-10-09T00:00:01Z")).unwrap();
        let files = log::list_logs(&dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0].compressed && !files[1].compressed);
        // Days later, maintenance keeps the writer's current day.
        shared.maintain(timefmt::parse_rfc3339_ms("2025-10-30T00:00:00Z").unwrap());
        let files = log::list_logs(&dir).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(timefmt::day_name(files[0].day), "2025-10-09");
        fs::remove_dir_all(&dir).unwrap();
    }
}

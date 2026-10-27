//! `tk-audit-feed`: the Linux OS feed's process.
//!
//! - `tk-audit-feed rules [--uid N] [--home PATH]` prints the audit rules
//!   file for the user (default: this user). It installs nothing.
//! - `tk-audit-feed run [--log PATH] [--from-start]` tails the audit log
//!   (default `/var/log/audit/audit.log`, readable by the `adm` group on
//!   Ubuntu), learns chats from the daemon's `run.start` and `run.stop`
//!   records, and posts the chats' events to the daemon. It runs as the
//!   user, never as root; the user needs read access to the log.
//! - `tk-audit-feed parse FILE --root PID --chat ID [--folder DIR]...`
//!   prints, one JSON line each, the events one recorded log gives for one
//!   root. Offline; for the box test and for reading a fixture.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tk_audit_linux::client::{self, Client};
use tk_audit_linux::feed::Feed;
use tk_audit_linux::rules::{plain_path, render, RulesInput};
use tk_audit_linux::tree::Root;
use tk_core::event::Event;

const DEFAULT_LOG: &str = "/var/log/audit/audit.log";
/// Records already in the daemon's log are read again from this far back,
/// so chats started before the feed are known.
const TAIL_BACKLOG_MS: i64 = 24 * 60 * 60 * 1000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("rules") => cmd_rules(&args[1..]),
        Some("run") => cmd_run(&args[1..]),
        Some("parse") => cmd_parse(&args[1..]),
        _ => {
            eprintln!("usage: tk-audit-feed <rules|run|parse> ...");
            2
        }
    };
    ExitCode::from(code)
}

fn value<'a>(args: &'a [String], i: &mut usize, flag: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i)
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| h.starts_with('/'))
}

fn cmd_rules(args: &[String]) -> u8 {
    // SAFETY: getuid has no preconditions and cannot fail.
    let mut uid = unsafe { libc::getuid() };
    let mut home = home();
    let mut i = 0;
    while i < args.len() {
        let r = match args[i].as_str() {
            "--uid" => value(args, &mut i, "--uid")
                .and_then(|v| v.parse().map_err(|_| "--uid needs a number".into()))
                .map(|v| uid = v),
            "--home" => value(args, &mut i, "--home").map(|v| home = Some(v.to_string())),
            other => Err(format!("unknown argument {other}")),
        };
        if let Err(e) = r {
            eprintln!("tk-audit-feed rules: {e}");
            return 2;
        }
        i += 1;
    }
    let Some(home) = home else {
        eprintln!("tk-audit-feed rules: no home folder (set HOME or pass --home)");
        return 2;
    };
    if !plain_path(&home) {
        eprintln!(
            "tk-audit-feed rules: the home folder {home:?} cannot go into a rules file unquoted"
        );
        return 2;
    }
    if uid == 0 {
        eprintln!("tk-audit-feed rules: refusing uid 0; pass the uid the harnesses run as");
        return 2;
    }
    let exists = |p: &str| Path::new(p).exists();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    let (text, skipped) = render(&RulesInput {
        uid,
        home: &home,
        runtime_dir: runtime_dir.as_deref(),
        exists: &exists,
    });
    print!("{text}");
    for s in skipped {
        eprintln!("tk-audit-feed rules: no rule for {s} (it does not exist; reading it later is not recorded)");
    }
    0
}

fn cmd_parse(args: &[String]) -> u8 {
    let mut file = None;
    let mut pid = None;
    let mut chat = None;
    let mut folders = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let r: Result<(), String> = match args[i].as_str() {
            "--root" => value(args, &mut i, "--root")
                .and_then(|v| v.parse().map_err(|_| "--root needs a pid".into()))
                .map(|v| pid = Some(v)),
            "--chat" => value(args, &mut i, "--chat").map(|v| chat = Some(v.to_string())),
            "--folder" => value(args, &mut i, "--folder").map(|v| folders.push(v.to_string())),
            f if !f.starts_with('-') && file.is_none() => {
                file = Some(f.to_string());
                Ok(())
            }
            other => Err(format!("unknown argument {other}")),
        };
        if let Err(e) = r {
            eprintln!("tk-audit-feed parse: {e}");
            return 2;
        }
        i += 1;
    }
    let (Some(file), Some(pid), Some(chat)) = (file, pid, chat) else {
        eprintln!("usage: tk-audit-feed parse FILE --root PID --chat ID [--folder DIR]...");
        return 2;
    };
    let text = match std::fs::read(&file) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            eprintln!("tk-audit-feed parse: {file}: {e}");
            return 1;
        }
    };
    let root = Root {
        chat,
        harness: String::new(),
        pid,
        folders,
        state: vec![],
        at: 0,
        stopped: false,
    };
    for e in tk_audit_linux::parse_for_root(&text, root, &home().unwrap_or_default()) {
        println!("{}", serde_json::to_string(&e).unwrap_or_default());
    }
    0
}

enum Msg {
    Line(String),
    Idle,
    Daemon(Event),
    Gone(String),
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn cmd_run(args: &[String]) -> u8 {
    let mut log = PathBuf::from(DEFAULT_LOG);
    let mut from_start = false;
    let mut i = 0;
    while i < args.len() {
        let r: Result<(), String> = match args[i].as_str() {
            "--log" => value(args, &mut i, "--log").map(|v| log = PathBuf::from(v)),
            "--from-start" => {
                from_start = true;
                Ok(())
            }
            other => Err(format!("unknown argument {other}")),
        };
        if let Err(e) = r {
            eprintln!("tk-audit-feed run: {e}");
            return 2;
        }
        i += 1;
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!(
            "tk-audit-feed run: refusing to run as root; run it as the user whose daemon it feeds"
        );
        return 2;
    }
    let Some(home) = home() else {
        eprintln!("tk-audit-feed run: HOME is not set");
        return 2;
    };
    let Some(socket) = client::socket_path() else {
        eprintln!("tk-audit-feed run: no socket path (HOME and XDG_RUNTIME_DIR are unset)");
        return 2;
    };
    let dir = socket.parent().map(Path::to_path_buf).unwrap_or_default();
    let token = match client::read_daemon_token(&dir) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tk-audit-feed run: {e}");
            return 1;
        }
    };
    let (tx, rx) = mpsc::channel::<Msg>();
    let tail = match Client::connect(&socket, &token, "tk-audit-feed tail")
        .and_then(|(c, _)| c.tail(Some(now_ms() - TAIL_BACKLOG_MS)))
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tk-audit-feed run: {e}");
            return 1;
        }
    };
    let (mut poster, _) = match Client::connect(&socket, &token, "tk-audit-feed") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tk-audit-feed run: {e}");
            return 1;
        }
    };
    {
        let tx = tx.clone();
        let mut tail = tail;
        std::thread::spawn(move || loop {
            match tail.next_event() {
                Ok(e) => {
                    if tx.send(Msg::Daemon(e)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Gone(format!("daemon tail: {e}")));
                    return;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        let log = log.clone();
        std::thread::spawn(move || {
            if let Err(e) = follow(&log, from_start, &tx) {
                let _ = tx.send(Msg::Gone(format!("{}: {e}", log.display())));
            }
        });
    }
    drop(tx);
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok();
    let mut feed = Feed::new(&home, runtime_dir.as_deref());
    let mut last_prune = Instant::now();
    let mut failed_posts: u64 = 0;
    eprintln!(
        "tk-audit-feed: reading {} for {}",
        log.display(),
        socket.display()
    );
    for msg in rx {
        let events = match msg {
            Msg::Line(l) => feed.push_line(&l),
            Msg::Idle => feed.flush(),
            Msg::Daemon(e) => feed.on_daemon_event(&e, &alive),
            Msg::Gone(why) => {
                eprintln!("tk-audit-feed: stopping: {why}");
                return 1;
            }
        };
        for e in events {
            if let Err(err) = poster.post(&e) {
                failed_posts += 1;
                eprintln!("tk-audit-feed: post failed ({failed_posts} so far): {err}");
                match Client::connect(&socket, &token, "tk-audit-feed") {
                    Ok((c, _)) => poster = c,
                    Err(e) => {
                        eprintln!("tk-audit-feed: stopping: {e}");
                        return 1;
                    }
                }
            }
        }
        if last_prune.elapsed() > Duration::from_secs(60) {
            feed.prune(&alive);
            last_prune = Instant::now();
        }
    }
    0
}

/// Follows the log like `tail -F`: from the end (or the start), across
/// rotation (a new inode or a file that shrank). Sends `Idle` after a quiet
/// half second so open events are flushed.
fn follow(path: &Path, from_start: bool, tx: &mpsc::Sender<Msg>) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let mut inode = file.metadata()?.ino();
    if !from_start {
        file.seek(SeekFrom::End(0))?;
    }
    let mut reader = BufReader::new(file);
    let mut pos = reader.stream_position()?;
    let mut quiet = Instant::now();
    let mut idle_sent = true;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = (&mut reader).take(64 * 1024).read_until(b'\n', &mut line)?;
        if n > 0 && line.ends_with(b"\n") {
            pos += n as u64;
            if tx
                .send(Msg::Line(String::from_utf8_lossy(&line).into_owned()))
                .is_err()
            {
                return Ok(());
            }
            quiet = Instant::now();
            idle_sent = false;
            continue;
        }
        if n > 0 {
            // A partial line: wait for the rest.
            reader.seek(SeekFrom::Start(pos))?;
        }
        if !idle_sent && quiet.elapsed() > Duration::from_millis(500) {
            if tx.send(Msg::Idle).is_err() {
                return Ok(());
            }
            idle_sent = true;
        }
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.ino() != inode || meta.len() < pos {
                let f = File::open(path)?;
                inode = f.metadata()?.ino();
                reader = BufReader::new(f);
                pos = 0;
            }
        }
    }
}

use std::io::Read as _;

//! `tk status`: what covers each agent, and what the daemon sees.
//!
//! Per agent, from its row in `contracts/harness-presets.json` (the
//! `coverage` fields integration w1 added, note AQ section 4): its tier
//! (G gate, A rule files only, O outside shape only), whether tk's gate is
//! built or planned and by which wave, whether a crash or a timeout of the
//! gate lets the call through ("fails open", rules case 70), where an `ask`
//! goes (case 71), the version the row was verified at, whether the agent's
//! program is on PATH, and, for the agents whose gate is built, whether it
//! is registered in your user-level config (read from the file, never by
//! running the agent). With `--probe`, `<agent> --version` is run for the
//! agents found on PATH, so the installed version can be set beside the
//! verified one (case 81); without it nothing is run.
//!
//! Then the daemon: whether it answers, which process serves its socket,
//! the feeds seen in the last hour (an OS feed that is off is said, case
//! 64), and the chats `tk run` started that have not stopped.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::conn::{classify, Conn, Incoming};
use super::peer;
use super::register::{self, Registered};
use super::text::{clean, clock, now_ms};
use super::Args;

pub const FLAGS: &[super::Flag] = &[("--json", false), ("--probe", false), ("--socket", true)];

pub const USAGE: &str = "usage: tk status [--json] [--probe] [--socket <path>]

  --probe   also run `<agent> --version` for each agent found on PATH
            (each runs as you; some agents check for updates when they do)";

const PRESETS: &str = include_str!("../../../../contracts/harness-presets.json");

/// How long `--probe` waits for one `--version`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn main(args: &Args) -> u8 {
    let env = Env::real();
    let socket = match args.socket() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tk status: {e}");
            return 2;
        }
    };
    let rows = agents(&env, args.flag("--probe"));
    let daemon = daemon_state(&socket);
    let out = std::io::stdout();
    let mut out = out.lock();
    if args.flag("--json") {
        let doc = json!({"agents": rows, "daemon": daemon});
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
    } else {
        let _ = writeln!(out, "{}", render(&rows, &daemon));
    }
    0
}

/// What `tk status` reads from the machine. Tests build their own.
pub struct Env {
    pub home: Option<PathBuf>,
    pub path: String,
    pub vars: Vec<(String, String)>,
    pub macos: bool,
}

impl Env {
    pub fn real() -> Env {
        let keep = [
            "XDG_CONFIG_HOME",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "PI_CODING_AGENT_DIR",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
        ];
        Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            path: std::env::var("PATH").unwrap_or_default(),
            vars: keep
                .iter()
                .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
                .collect(),
            macos: cfg!(target_os = "macos"),
        }
    }

    pub fn var(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }
}

/// One row per agent: the 14 rows tk connect writes for, then the planned
/// ones, in the table's order.
pub fn agents(env: &Env, probe: bool) -> Vec<Value> {
    let table: Value = serde_json::from_str(PRESETS).unwrap_or(Value::Null);
    let mut out = Vec::new();
    for (list, planned) in [("harnesses", false), ("planned", true)] {
        for row in table
            .get(list)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            out.push(agent_row(&row, planned, env, probe));
        }
    }
    out
}

fn agent_row(row: &Value, planned_row: bool, env: &Env, probe: bool) -> Value {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let id = s(row, "id");
    let cov = row.get("coverage").cloned().unwrap_or(Value::Null);
    let gate = cov.get("gate").cloned().unwrap_or(Value::Null);
    let status = s(&gate, "status");
    let cli = row
        .get("cli")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty() && *c != "TODO" && plain_name(c))
        .map(str::to_string);
    let found = cli.as_deref().and_then(|c| find_on_path(c, &env.path));
    let registered = if status == "built" {
        register::check(&id, env)
    } else {
        Registered::NotBuilt
    };
    let installed_version = match (&found, probe) {
        (Some(p), true) => Some(probe_version(p)),
        _ => None,
    };
    json!({
        "id": id,
        "name": s(row, "name"),
        "tier": cov.get("tier").cloned().unwrap_or(Value::Null),
        "gate": {
            "status": if planned_row && status.is_empty() { "planned".to_string() } else { status },
            "family": s(&gate, "family"),
            "wave": gate.get("wave").cloned().unwrap_or(Value::Null),
            "lane": gate.get("lane").cloned().unwrap_or(Value::Null),
            "where": s(&gate, "where"),
        },
        "registered": registered.word(),
        "registeredDetail": registered.detail(),
        "failMode": s(&cov, "failMode"),
        "failSwitch": cov.get("failSwitch").cloned().unwrap_or(Value::Null),
        "ask": s(&cov, "ask"),
        "verifiedAt": s(&cov, "verifiedAt"),
        "cli": cli,
        "onPath": found.as_ref().map(|p| p.display().to_string()),
        "installedVersion": installed_version,
        "note": cov.get("note").cloned().unwrap_or(Value::Null),
    })
}

/// A program name with no folder part: what a row's `cli` must be.
fn plain_name(c: &str) -> bool {
    !c.contains('/') && !c.starts_with('-') && c.bytes().all(|b| b.is_ascii_graphic())
}

/// The first `<dir>/<name>` on PATH that is a regular file with an execute
/// bit, in an absolute folder. A relative PATH entry (`.`, empty) is
/// skipped: it names whatever folder `tk status` runs in.
pub fn find_on_path(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    for dir in path.split(':') {
        if dir.is_empty() || !Path::new(dir).is_absolute() {
            continue;
        }
        let p = Path::new(dir).join(name);
        if let Ok(m) = std::fs::metadata(&p) {
            if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// `<program> --version`, argv only, stdin closed, a clean environment but
/// HOME and PATH, killed after `PROBE_TIMEOUT`. The first line of output,
/// cleaned and cut, or why there is none.
pub fn probe_version(program: &Path) -> String {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear();
    for k in ["HOME", "PATH", "LANG", "TERM"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("not run: {e}"),
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < PROBE_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return format!("no answer in {} s", PROBE_TIMEOUT.as_secs());
            }
        }
    }
    let mut text = String::new();
    if let Some(mut o) = child.stdout.take() {
        use std::io::Read;
        let _ = (&mut o).take(4096).read_to_string(&mut text);
    }
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first.is_empty() {
        "printed nothing".into()
    } else {
        clean(first, 80)
    }
}

/// The daemon as `tk status` sees it.
pub fn daemon_state(socket: &Path) -> Value {
    let mut conn = match Conn::open(socket, "tk status") {
        Ok(c) => c,
        Err(e) => {
            return json!({"running": false, "socket": socket.display().to_string(), "error": e});
        }
    };
    let info = conn.hello.get("daemon").cloned().unwrap_or(Value::Null);
    let said_pid = info.get("pid").and_then(Value::as_u64).map(|p| p as u32);
    let served_by = conn.peer().map(|p| {
        let program = p
            .pid
            .and_then(peer::program)
            .map(|x| x.display().to_string());
        json!({
            "pid": p.pid,
            "uid": p.uid,
            "yours": p.uid == peer::me(),
            "program": program,
            "matchesHello": p.pid.is_some() && p.pid == said_pid,
        })
    });
    let now = now_ms();
    let report = conn
        .request(&json!({"op": "report", "since": now - 3_600_000, "until": now}))
        .ok()
        .and_then(|r| r.get("report").cloned());
    let chats = running_chats(socket, now);
    json!({
        "running": true,
        "socket": socket.display().to_string(),
        "daemon": info,
        "servedBy": served_by,
        "lastHour": report.as_ref().map(|r| json!({
            "events": r.get("events"),
            "flags": r.get("flags"),
            "bySource": r.get("bySource"),
            "chainOk": r.get("chainOk"),
            "collectorReachable": r.get("check").and_then(|c| c.get("collectorReachable")),
        })),
        "chats": chats,
    })
}

/// Chats with a `run.start` in the last 7 days and no `run.stop`, from the
/// daemon's tail backlog. The backlog has no end marker, so it is read
/// until the daemon is quiet for half a second; at most 5,000 events come
/// back (contract section 2), so on a busy daemon an old start can be
/// missing.
pub fn running_chats(socket: &Path, now: i64) -> Value {
    let Ok(mut conn) = Conn::open(socket, "tk status") else {
        return Value::Null;
    };
    let since = now - 7 * 86_400_000;
    if conn.send(&json!({"op": "tail", "since": since})).is_err() || conn.line().is_err() {
        return Value::Null;
    }
    let _ = conn.quiet_timeout(Duration::from_millis(500));
    let mut starts: Vec<Value> = Vec::new();
    let mut stopped: Vec<String> = Vec::new();
    while let Ok(line) = conn.line() {
        if let Incoming::Event(ev) = classify(line) {
            if ev.get("source").and_then(Value::as_str) != Some("run") {
                continue;
            }
            let chat = ev.get("chatId").and_then(Value::as_str).unwrap_or("").to_string();
            match ev.get("kind").and_then(Value::as_str) {
                Some("run.start") => starts.push(ev),
                Some("run.stop") => stopped.push(chat),
                _ => {}
            }
        }
    }
    let rows: Vec<Value> = starts
        .into_iter()
        .filter(|e| {
            let c = e.get("chatId").and_then(Value::as_str).unwrap_or("");
            !stopped.iter().any(|s| s == c)
        })
        .map(|e| {
            let pid = e.get("pid").and_then(Value::as_u64).map(|p| p as u32);
            let d = e.get("detail").cloned().unwrap_or(Value::Null);
            json!({
                "chat": e.get("chatId"),
                "harness": e.get("harness"),
                "boundary": d.get("boundary"),
                "since": e.get("at"),
                "pid": pid,
                "alive": pid.map(alive),
                "fullAccessFlags": d.get("dangerousFlags"),
            })
        })
        .collect();
    Value::Array(rows)
}

/// Whether `pid` still runs. A pid can be reused after an exit, so this
/// is a hint, said as one.
fn alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only checks that the process exists and
    // that we may signal it.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub fn render(rows: &[Value], daemon: &Value) -> String {
    let mut out = String::new();
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    out.push_str(&format!(
        "{:<14} {:<4} {:<26} {:<12} {:<22} {:<18} {:<20} {}\n",
        "agent", "tier", "gate", "registered", "when the gate fails", "ask goes to", "verified at", "installed"
    ));
    for r in rows {
        let gate = r.get("gate").cloned().unwrap_or(Value::Null);
        let gate_text = match s(&gate, "status").as_str() {
            "built" => format!("built ({})", s(&gate, "family")),
            "planned" => match (gate.get("wave").and_then(Value::as_u64), gate.get("lane").and_then(Value::as_str)) {
                (Some(w), Some(l)) => format!("planned, wave {w} lane {l}"),
                _ => "planned".into(),
            },
            "none" => "no in-harness gate".into(),
            other => clean(other, 26),
        };
        let tier = r.get("tier").and_then(Value::as_str).unwrap_or("-");
        let installed = match (r.get("onPath").and_then(Value::as_str), r.get("installedVersion").and_then(Value::as_str)) {
            (Some(_), Some(v)) => clean(v, 40),
            (Some(_), None) => "on PATH (not probed)".into(),
            (None, _) if r.get("cli").map(Value::is_null).unwrap_or(true) => "-".into(),
            (None, _) => "not found".into(),
        };
        out.push_str(&format!(
            "{:<14} {:<4} {:<26} {:<12} {:<22} {:<18} {:<20} {}\n",
            clean(&s(r, "id"), 14),
            tier,
            gate_text,
            s(r, "registered"),
            fail_text(&s(r, "failMode"), r.get("failSwitch").and_then(Value::as_str)),
            ask_text(&s(r, "ask")),
            clean(&s(r, "verifiedAt"), 20),
            installed,
        ));
    }
    let notes: Vec<String> = rows
        .iter()
        .filter(|r| s(r, "registered") == "unknown" || s(r, "registered") == "no")
        .filter_map(|r| {
            r.get("registeredDetail")
                .and_then(Value::as_str)
                .map(|d| format!("  {}: {}", s(r, "id"), clean(d, 160)))
        })
        .collect();
    if !notes.is_empty() {
        out.push_str("\nregistration:\n");
        for n in notes {
            out.push_str(&n);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(&render_daemon(daemon));
    out.trim_end().to_string()
}

fn fail_text(mode: &str, switch: Option<&str>) -> String {
    match mode {
        "blocks" => "blocks".into(),
        "blocks-with-switch" => "blocks (switch set)".into(),
        "open" => "FAILS OPEN".into(),
        "open-on-timeout" => "FAILS OPEN on timeout".into(),
        _ => match switch {
            Some(_) => "unknown".into(),
            None => "unknown".into(),
        },
    }
}

fn ask_text(ask: &str) -> String {
    match ask {
        "own-prompt" => "its own prompt".into(),
        "approver-or-deny" => "approver, else deny".into(),
        _ => "unknown".into(),
    }
}

fn render_daemon(d: &Value) -> String {
    let mut out = String::new();
    if d.get("running").and_then(Value::as_bool) != Some(true) {
        out.push_str(&format!(
            "daemon: not reachable: {}\n  without it there is no feed, no approver and no chat tokens; `tk run` refuses to start\n",
            clean(d.get("error").and_then(Value::as_str).unwrap_or(""), 200)
        ));
        return out;
    }
    let info = d.get("daemon").cloned().unwrap_or(Value::Null);
    out.push_str(&format!(
        "daemon: running, version {}, protocol {}\n",
        clean(info.get("version").and_then(Value::as_str).unwrap_or("?"), 32),
        info.get("protocol").and_then(Value::as_u64).unwrap_or(0)
    ));
    if let Some(by) = d.get("servedBy").filter(|v| !v.is_null()) {
        let pid = by.get("pid").and_then(Value::as_u64);
        let program = by.get("program").and_then(Value::as_str).unwrap_or("unknown program");
        let yours = by.get("yours").and_then(Value::as_bool) == Some(true);
        let matches = by.get("matchesHello").and_then(Value::as_bool) == Some(true);
        out.push_str(&format!(
            "  socket served by pid {} ({}){}{}\n",
            pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
            clean(program, 120),
            if yours { "" } else { ", NOT your user" },
            if matches { "" } else { ", not the pid the daemon gave" },
        ));
    }
    if let Some(h) = d.get("lastHour").filter(|v| !v.is_null()) {
        let by = h.get("bySource").and_then(Value::as_object);
        let n = |k: &str| by.and_then(|m| m.get(k)).and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!(
            "  last hour: {} events, {} flags; otel {}, adapter {}, run {}, approval {}\n",
            h.get("events").and_then(Value::as_u64).unwrap_or(0),
            h.get("flags").and_then(Value::as_u64).unwrap_or(0),
            n("otel"),
            n("adapter"),
            n("run"),
            n("approval"),
        ));
        let os_events = n("auditd") + n("eslogger");
        let reach = h.get("collectorReachable").and_then(Value::as_str).unwrap_or("unknown");
        if os_events == 0 && reach != "yes" {
            out.push_str("  os feed: off (no auditd or collector events in the last hour); the feed shows the harnesses' own records only\n");
        } else {
            out.push_str(&format!(
                "  os feed: auditd {}, collector {} ({})\n",
                n("auditd"),
                n("eslogger"),
                clean(reach, 12)
            ));
        }
        if h.get("chainOk").and_then(Value::as_bool) == Some(false) {
            out.push_str("  CHAIN BROKEN in the last hour: run `tk watch report --since 1h`\n");
        }
    }
    match d.get("chats").and_then(Value::as_array) {
        Some(chats) if !chats.is_empty() => {
            out.push_str("  chats started by tk run and not stopped:\n");
            for c in chats {
                let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
                let alive = match c.get("alive").and_then(Value::as_bool) {
                    Some(true) => "running",
                    Some(false) => "gone, no run.stop",
                    None => "pid unknown",
                };
                let full = c
                    .get("fullAccessFlags")
                    .and_then(Value::as_array)
                    .filter(|a| !a.is_empty())
                    .map(|_| ", full-access flags")
                    .unwrap_or("");
                out.push_str(&format!(
                    "    {}  {:<12} boundary {:<5} since {}  {}{}\n",
                    clean(s("chat"), 24),
                    clean(s("harness"), 12),
                    clean(s("boundary"), 5),
                    clock(c.get("since").and_then(Value::as_i64).unwrap_or(0)),
                    alive,
                    full
                ));
            }
        }
        Some(_) => out.push_str("  no chats started by tk run are open\n"),
        None => out.push_str("  chats: could not read the backlog\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &Path) -> Env {
        Env {
            home: Some(home.to_path_buf()),
            path: String::new(),
            vars: Vec::new(),
            macos: false,
        }
    }

    #[test]
    fn every_preset_row_has_a_status_row() {
        let table: Value = serde_json::from_str(PRESETS).unwrap();
        let n = table["harnesses"].as_array().unwrap().len() + table["planned"].as_array().unwrap().len();
        let dir = std::env::temp_dir().join(format!("tk-status-{}", std::process::id()));
        let rows = agents(&env(&dir), false);
        assert_eq!(rows.len(), n);
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"pi") && ids.contains(&"kilo") && ids.contains(&"codebuff"));
    }

    #[test]
    fn codex_and_gemini_say_they_fail_open_and_planned_rows_say_planned() {
        let dir = std::env::temp_dir().join(format!("tk-status2-{}", std::process::id()));
        let rows = agents(&env(&dir), false);
        let text = render(&rows, &json!({"running": false, "error": "x"}));
        let line = |id: &str| text.lines().find(|l| l.starts_with(&format!("{id} "))).unwrap().to_string();
        assert!(line("codex").contains("FAILS OPEN"), "{}", line("codex"));
        assert!(line("gemini").contains("planned, wave 2 lane I"), "{}", line("gemini"));
        assert!(line("droid").contains("planned, wave 4 lane L"), "{}", line("droid"));
        assert!(line("copilot-cli").contains("on timeout"));
        assert!(line("zed").contains("no in-harness gate"));
        assert!(line("pi").contains("built (pi-extension)"));
        assert!(line("gemini").contains("n/a"), "{}", line("gemini"));
    }

    #[test]
    fn a_daemon_that_is_down_is_said_with_what_that_costs() {
        let t = render_daemon(&json!({"running": false, "error": "the daemon is not running (x)"}));
        assert!(t.contains("not reachable") && t.contains("no approver"), "{t}");
    }

    #[test]
    fn an_os_feed_with_no_events_is_said_off() {
        let t = render_daemon(&json!({"running": true, "daemon": {"version": "0.0.0", "protocol": 1},
            "lastHour": {"events": 3, "flags": 0, "bySource": {"adapter": 3}, "chainOk": true, "collectorReachable": "unknown"},
            "chats": []}));
        assert!(t.contains("os feed: off"), "{t}");
        assert!(t.contains("no chats started by tk run are open"), "{t}");
    }

    #[test]
    fn path_lookup_skips_relative_entries_and_non_executables() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tk-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin/pi");
        std::fs::write(&exe, "#!/bin/sh\necho 'pi 1.4.0'\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("bin/claude"), "x").unwrap();
        let path = format!(".:{}:", dir.join("bin").display());
        assert_eq!(find_on_path("pi", &path), Some(exe.clone()));
        assert_eq!(find_on_path("claude", &path), None, "not executable");
        assert_eq!(find_on_path("pi", ".:bin"), None, "relative entries only");
        assert_eq!(probe_version(&exe), "pi 1.4.0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_that_hangs_is_killed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tk-hang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("slow");
        std::fs::write(&exe, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let t = std::time::Instant::now();
        assert!(probe_version(&exe).starts_with("no answer"));
        assert!(t.elapsed() < PROBE_TIMEOUT + Duration::from_secs(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_printing_escapes_is_cleaned() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tk-esc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("loud");
        std::fs::write(&exe, "#!/bin/sh\nprintf '\\033]0;owned\\007v1\\n'\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let v = probe_version(&exe);
        assert!(!v.contains('\x1b') && v.ends_with("v1"), "{v:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

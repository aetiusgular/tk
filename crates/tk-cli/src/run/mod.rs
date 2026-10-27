//! `tk run` (lane D, T1.4): start a harness inside a boundary, with the
//! chat's token and endpoints in its environment, and record the run in the
//! daemon.
//!
//! ```text
//! tk run pi --boundary sbx            # Docker Sandboxes, the pi kit
//! tk run pi --boundary srt --model-url http://127.0.0.1:8080/v1
//! tk run claude-code --boundary none -- --continue
//! ```
//!
//! Order: read the daemon's token, mint a chat token (`token.mint`), build
//! the plan (`plan.rs`, pure), write srt's settings (0600), start the child,
//! post `run.start` with its pid (the Linux feed places the process tree
//! from it), poll `sbx policy log` under sbx, wait, post `run.stop`, exit
//! with the child's code. The daemon not running is an error unless
//! `--no-daemon` says to run with no feed (fail closed, rules case 9).
//!
//! Contract: `contracts/daemon-protocol.md` sections 2 and 3. Sources: the
//! lane D status doc, Research.

mod args;
mod exec;
mod harness;
mod plan;
mod policy_log;
mod preflight;

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tk_audit_linux::client::{self, Client};
use tk_core::event::{Event, Source};

use args::Boundary;

const POLL: Duration = Duration::from_secs(2);
const POLICY_LOG_TIMEOUT: Duration = Duration::from_secs(10);

/// What `tk run` reads from its own environment, gathered in one place so
/// the tests can give a fake daemon and fake programs.
struct RunEnv {
    socket: Option<PathBuf>,
    home: Option<String>,
    gateway: Option<String>,
    /// Program name to path, for tests only (`main` passes none).
    programs: Vec<(String, String)>,
    /// `TK_LOG_CONTENT=1`: run records keep the harness's own arguments
    /// (still through the secret redactor). Off by default (finding M8).
    log_content: bool,
    /// `TK_POLICY`, the policy file the harness's gate will read.
    policy: Option<String>,
    /// `PATH`, searched for srt's own dependencies (BX4).
    path: String,
    /// `/proc/sys/kernel/apparmor_restrict_unprivileged_userns` (BX3), when
    /// it exists.
    userns: Option<String>,
}

impl RunEnv {
    fn real() -> RunEnv {
        RunEnv {
            socket: client::socket_path(),
            home: std::env::var("HOME").ok(),
            gateway: std::env::var("TK_GATEWAY_URL").ok(),
            programs: Vec::new(),
            log_content: std::env::var("TK_LOG_CONTENT").is_ok_and(|v| v == "1"),
            policy: std::env::var("TK_POLICY").ok(),
            path: std::env::var("PATH").unwrap_or_default(),
            userns: std::fs::read_to_string(
                "/proc/sys/kernel/apparmor_restrict_unprivileged_userns",
            )
            .ok(),
        }
    }

    fn program<'a>(&'a self, name: &'a str) -> &'a str {
        self.programs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, p)| p.as_str())
            .unwrap_or(name)
    }
}

pub fn main(argv: &[String]) -> u8 {
    match run(argv, &RunEnv::real()) {
        Ok(code) => code,
        Err((code, msg)) => {
            eprintln!("tk run: {msg}");
            code
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes at
    // most that many bytes.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return String::new();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn new_chat_id() -> String {
    format!("tk-{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

fn event(
    chat: &str,
    harness: &str,
    kind: &str,
    summary: String,
    detail: serde_json::Value,
    pid: Option<u32>,
) -> Event {
    Event {
        id: format!("run-{chat}-{}", uuid::Uuid::new_v4().simple()),
        at: now_ms(),
        chat_id: chat.to_string(),
        harness: harness.to_string(),
        source: Source::Run,
        kind: kind.to_string(),
        summary: summary.chars().take(200).collect(),
        detail,
        pid,
        approval_id: None,
    }
}

/// Writes srt's settings as a new 0600 file in a 0700 folder, refusing a
/// symlink at the file's place.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_file() => {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )))
        }
        Ok(_) => std::fs::remove_file(path)?,
        Err(_) => {}
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

struct Daemon {
    socket: PathBuf,
    token: String,
}

impl Daemon {
    fn post(&self, e: &Event) -> Result<(), String> {
        let (mut c, _) =
            Client::connect(&self.socket, &self.token, "tk run").map_err(|e| e.to_string())?;
        c.post(e).map(|_| ()).map_err(|e| e.to_string())
    }

    /// One post with the token file read again, for a daemon that restarted
    /// since `tk run` started. The new token must still give daemon scope,
    /// as at the start.
    fn post_after_restart(&self, e: &Event) -> Result<(), String> {
        let dir = self.socket.parent().ok_or("the socket has no folder")?;
        let token = client::read_daemon_token(dir).map_err(|e| e.to_string())?;
        if token == self.token {
            return Err("the token file has not changed".into());
        }
        let (mut c, hello) =
            Client::connect(&self.socket, &token, "tk run").map_err(|e| e.to_string())?;
        if hello.scope != "daemon" {
            return Err("the daemon's token file gave chat scope, not daemon scope".into());
        }
        c.post(e).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// `lstat`, not `stat`: a regular file (no link followed), owned by this
/// user, one hard link, no group or other write bit (fix pass review R1).
fn regular_private_file(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m| {
        // SAFETY: getuid takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        m.file_type().is_file() && m.uid() == uid && m.nlink() == 1 && m.mode() & 0o022 == 0
    })
}

/// The argv a run record keeps. tk's own part (the wrapper and its flags,
/// the harness program) is kept; the harness's own arguments, which every
/// plan appends last, keep their flag names and lose every other word to its
/// length, unless `content` (Mac test w1, finding M8: a `-p "<prompt>"` sat
/// in the activity log). Both go through the secret redactor (S13).
fn recorded_argv(program: &str, args: &[String], rest: &[String], content: bool) -> Vec<String> {
    let mask = |w: &String| -> String {
        if content {
            return w.clone();
        }
        let n = |v: &str| format!("[{} chars]", v.chars().count());
        match w.strip_prefix('-') {
            Some(_) if w.len() > 1 => match w.split_once('=') {
                Some((flag, value)) => format!("{flag}={}", n(value)),
                None => w.clone(),
            },
            _ => n(w),
        }
    };
    let split = if args.ends_with(rest) {
        args.len() - rest.len()
    } else {
        // Not expected (plan.rs appends `rest` last); mask everything.
        0
    };
    let mut out = vec![program.to_string()];
    out.extend(args[..split].iter().cloned());
    out.extend(args[split..].iter().map(mask));
    tk_audit_linux::redact::argv(&out)
}

fn run(argv: &[String], env: &RunEnv) -> Result<u8, (u8, String)> {
    let a = args::parse(argv).map_err(|e| (2, e))?;
    let cwd_raw = match &a.cwd {
        Some(c) => PathBuf::from(c),
        None => std::env::current_dir().map_err(|e| (1, format!("current folder: {e}")))?,
    };
    let cwd = std::fs::canonicalize(&cwd_raw)
        .map_err(|e| (2, format!("--cwd {}: {e}", cwd_raw.display())))?;
    if !cwd.is_dir() {
        return Err((2, format!("{} is not a folder", cwd.display())));
    }
    let cwd = cwd
        .to_str()
        .ok_or((2, "the working folder's path is not UTF-8".to_string()))?
        .to_string();
    let home = env
        .home
        .clone()
        .filter(|h| h.starts_with('/'))
        .ok_or((2, "HOME is not set to an absolute path".to_string()))?;
    let chat = a.chat.clone().unwrap_or_else(new_chat_id);

    // The daemon, unless the user said to run without it.
    let socket = env.socket.clone();
    let dir = socket
        .as_ref()
        .and_then(|s| s.parent().map(Path::to_path_buf));
    let mut daemon = None;
    let mut token = None;
    let mut otlp = None;
    if !a.no_daemon {
        let (Some(socket), Some(dir)) = (socket.clone(), dir.clone()) else {
            return Err((
                1,
                "no daemon socket path (HOME and XDG_RUNTIME_DIR are unset)".into(),
            ));
        };
        otlp = client::read_otlp_endpoint(&dir);
        if !a.dry_run {
            let hint = "start tk-daemon, or pass --no-daemon to run with no token and no feed";
            let dtoken =
                client::read_daemon_token(&dir).map_err(|e| (1, format!("{e}; {hint}")))?;
            let (mut c, hello) = Client::connect(&socket, &dtoken, "tk run")
                .map_err(|e| (1, format!("{e}; {hint}")))?;
            if hello.scope != "daemon" {
                return Err((
                    1,
                    "the daemon's token file gave chat scope, not daemon scope".into(),
                ));
            }
            token = Some(
                c.mint(&chat, a.harness.id)
                    .map_err(|e| (1, e.to_string()))?,
            );
            daemon = Some(Daemon {
                socket,
                token: dtoken,
            });
        }
        if otlp.is_none() {
            eprintln!("tk run: the daemon published no loopback OTLP endpoint; the harness's emitter stays off");
        }
    } else {
        eprintln!("tk run: --no-daemon: no chat token, no feed, nothing recorded");
    }
    let gateway = a
        .gateway_url
        .clone()
        .or_else(|| env.gateway.clone().filter(|v| !v.is_empty()));
    let state_root = dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(&home).join(".tk/run/tk"));
    let settings_path = state_root.join("srt").join(format!("{chat}.json"));
    let token_path = state_root.join("chats").join(format!("{chat}.token"));
    let inputs = plan::Inputs {
        args: &a,
        chat: chat.clone(),
        cwd: cwd.clone(),
        home: home.clone(),
        token: token.clone(),
        otlp,
        gateway,
        daemon_dir: dir.as_ref().and_then(|d| d.to_str()).map(str::to_string),
        settings_path: settings_path.to_string_lossy().into_owned(),
        token_file: token_path.to_string_lossy().into_owned(),
        policy: env
            .policy
            .clone()
            .filter(|p| p.starts_with('/'))
            .unwrap_or_else(|| format!("{}/.tk/policy.json", home.trim_end_matches('/'))),
        policy_regular: false,
    };
    let inputs = plan::Inputs {
        policy_regular: regular_private_file(Path::new(&inputs.policy)),
        ..inputs
    };
    let p = plan::build(&inputs).map_err(|e| (2, e))?;
    // srt's dependencies and the userns sysctl, before srt fails on them with
    // its own errors (box test w1, BX3 and BX4).
    if a.boundary == Boundary::Srt {
        match preflight::srt(&env.path, cfg!(target_os = "linux"), env.userns.as_deref()) {
            Ok(notes) => notes.iter().for_each(|n| eprintln!("{n}")),
            Err(e) if a.dry_run => eprintln!("tk run: {e}"),
            Err(e) => return Err((1, e)),
        }
    }
    if a.dry_run {
        print!("{}", plan::describe(&p));
        if !a.no_daemon {
            println!("(dry run: no token minted, nothing started, nothing recorded)");
        }
        return Ok(0);
    }
    for n in &p.notes {
        eprintln!("{n}");
    }
    if let Some(text) = &p.srt_settings {
        write_private(&settings_path, text)
            .map_err(|e| (1, format!("{}: {e}", settings_path.display())))?;
    }
    let token_file_named = p.env_set.iter().any(|(k, _)| k == "TK_CHAT_TOKEN_FILE");
    if let (true, Some(t)) = (token_file_named, &token) {
        write_private(&token_path, &format!("{t}\n"))
            .map_err(|e| (1, format!("{}: {e}", token_path.display())))?;
    }

    // sbx: what the log holds before the start is not this run's.
    let mut differ = policy_log::Differ::default();
    let log_args = p.sandbox_name.as_ref().map(|n| {
        vec![
            "policy".to_string(),
            "log".into(),
            n.clone(),
            "--json".into(),
        ]
    });
    if let Some(la) = &log_args {
        if let Some(out) = exec::output_with_timeout(env.program("sbx"), la, POLICY_LOG_TIMEOUT) {
            differ.baseline(&policy_log::parse(&out));
        }
    }

    exec::install_handlers();
    let started = Instant::now();
    let mut launch = p.clone_for_start();
    launch.program = env.program(&p.program).to_string();
    let mut child = exec::spawn(&launch, &cwd).map_err(|e| (127, format!("{}: {e}", p.program)))?;
    let pid = child.id();
    // The record keeps the shape of the command, not a key passed on it
    // (integration w1, security finding S13; the audit feed's own redactor),
    // and not the harness's own words unless TK_LOG_CONTENT=1 (finding M8).
    let full_argv = recorded_argv(&p.program, &p.args, &a.rest, env.log_content);
    let dangerous: Vec<&String> = a
        .rest
        .iter()
        .filter(|x| x.to_ascii_lowercase().starts_with("--dangerously") || x.as_str() == "--yolo")
        .collect();
    let base = json!({
        "boundary": a.boundary.name(),
        "argv": full_argv,
        "kit": p.kit,
        "host": hostname(),
        "cwd": cwd,
        "folders": p.folders,
        "stateDirs": p.state_dirs,
        "sandboxName": p.sandbox_name,
        "runsAs": if a.boundary == Boundary::None { Some("you") } else { None },
        "dangerousFlags": dangerous,
        "trust": "client",
    });
    if let Some(d) = &daemon {
        let summary = format!("{} under {}", a.harness.id, a.boundary.name());
        if let Err(e) = d.post(&event(
            &chat,
            a.harness.id,
            "run.start",
            summary,
            base.clone(),
            Some(pid),
        )) {
            eprintln!("tk run: run.start was not recorded: {e}");
        }
    }

    // sbx: poll the policy log while the harness runs.
    let stop = Arc::new(AtomicBool::new(false));
    let poller = match (&daemon, &log_args) {
        (Some(d), Some(la)) => {
            let sbx_path = env.program("sbx").to_string();
            let (socket, dtoken, la, chat, harness, stop) = (
                d.socket.clone(),
                d.token.clone(),
                la.clone(),
                chat.clone(),
                a.harness.id.to_string(),
                stop.clone(),
            );
            Some(std::thread::spawn(move || {
                let d = Daemon {
                    socket,
                    token: dtoken,
                };
                let mut last = false;
                loop {
                    if let Some(out) = exec::output_with_timeout(&sbx_path, &la, POLICY_LOG_TIMEOUT)
                    {
                        for e in differ.new_entries(&policy_log::parse(&out)) {
                            let _ = d.post(&policy_log::to_event(&e, &chat, &harness, now_ms()));
                        }
                    }
                    if last {
                        return;
                    }
                    let t = Instant::now();
                    while t.elapsed() < POLL && !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    // One more poll after the stop, for what the last
                    // seconds blocked.
                    last = stop.load(Ordering::SeqCst);
                }
            }))
        }
        _ => None,
    };

    let status =
        exec::wait(&mut child).map_err(|e| (1, format!("waiting for {}: {e}", p.program)))?;
    let (code, signal) = exec::code(&status);
    stop.store(true, Ordering::SeqCst);
    if let Some(t) = poller {
        let _ = t.join();
    }
    if let Some(d) = &daemon {
        let mut detail = base;
        detail["exitCode"] = json!(status.code());
        detail["signal"] = json!(signal);
        detail["durationMs"] = json!(started.elapsed().as_millis() as u64);
        let summary = format!(
            "{} under {} ended, code {code}",
            a.harness.id,
            a.boundary.name()
        );
        let stop_event = event(&chat, a.harness.id, "run.stop", summary, detail, Some(pid));
        let posted = match d.post(&stop_event) {
            // The daemon restarted during the run and its token rotated
            // (box test w1, BX8): read the token file again, and post once
            // more under daemon scope, marked so the log says why.
            Err(e) if e.contains("bad token") => {
                let mut again = stop_event.clone();
                again.detail["daemonRestarted"] = json!(true);
                d.post_after_restart(&again)
                    .map_err(|e2| format!("{e}; after reading the token again: {e2}"))
            }
            other => other,
        };
        if let Err(e) = posted {
            eprintln!("tk run: run.stop was not recorded: {e}");
        }
    }
    if p.srt_settings.is_some() {
        let _ = std::fs::remove_file(&settings_path);
    }
    if token_file_named {
        let _ = std::fs::remove_file(&token_path);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ids_are_valid_and_distinct() {
        let a = new_chat_id();
        assert!(args::valid_chat(&a), "{a}");
        assert_ne!(a, new_chat_id());
    }

    #[test]
    fn the_settings_file_is_private_and_never_follows_a_link() {
        let dir = std::env::temp_dir().join(format!("tk-run-w-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("srt/c.json");
        write_private(&f, "{}").unwrap();
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(f.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        write_private(&f, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"a\":1}");
        // A link planted at the file's place is refused, its target untouched.
        let target = dir.join("target");
        std::fs::write(&target, "keep").unwrap();
        std::fs::remove_file(&f).unwrap();
        std::os::unix::fs::symlink(&target, &f).unwrap();
        assert!(write_private(&f, "x").is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_errors_exit_2() {
        assert_eq!(main(&[]), 2);
        assert_eq!(main(&["pi".into()]), 2);
        assert_eq!(
            main(&[
                "pi".into(),
                "--boundary".into(),
                "none".into(),
                "--cwd".into(),
                "/nonexistent/x".into()
            ]),
            2
        );
    }
}

/// `tk run` end to end against the scripted fake daemon
/// (`tk_audit_linux::client::fake`) and fake `pi` and `srt` scripts.
#[cfg(test)]
mod end_to_end {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tk_audit_linux::client::fake;

    fn script(dir: &Path, name: &str, body: &str) -> String {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    struct Setup {
        fake: fake::Fake,
        work: PathBuf,
        env: RunEnv,
    }

    fn setup(name: &str) -> Setup {
        let fake = fake::start(&format!("run-{name}"), fake::Mode::Normal);
        let work = fake.dir.join("work dir");
        std::fs::create_dir_all(&work).unwrap();
        let out = fake.dir.join("seen");
        // The fake pi writes what it was given and exits 3.
        let pi = script(
            &fake.dir,
            "pi",
            &format!(
                "{{ echo \"token=$TK_CHAT_TOKEN\"; echo \"chat=$TK_CHAT_ID\"; echo \"otlp=$TK_OTLP_ENDPOINT\"; \
                 echo \"boundary=$TK_BOUNDARY\"; echo \"proxy=$NODE_USE_ENV_PROXY\"; echo \"pwd=$(pwd)\"; \
                 for a in \"$@\"; do echo \"arg=$a\"; done; }} > '{}'; exit 3",
                out.display()
            ),
        );
        // The fake srt checks its settings file is private, keeps a copy,
        // and runs what follows `--` with `pi` pointed at the fake.
        let srt = script(
            &fake.dir,
            "srt",
            &format!(
                "[ \"$1\" = --settings ] || exit 90; f=\"$2\"; [ \"$3\" = -- ] || exit 91; \
                 m=$(stat -c %a \"$f\" 2>/dev/null || stat -f %Lp \"$f\"); [ \"$m\" = 600 ] || exit 92; cp \"$f\" '{}/settings.copy'; \
                 shift 4; exec '{pi}' \"$@\"",
                fake.dir.display()
            ),
        );
        // srt's own dependencies, for the preflight (BX4).
        let deps = fake.dir.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        for b in ["bwrap", "socat", "rg"] {
            script(&deps, b, "exit 0");
        }
        let env = RunEnv {
            socket: Some(fake.socket.clone()),
            home: Some(fake.dir.to_string_lossy().into_owned()),
            gateway: None,
            programs: vec![("pi".into(), pi), ("srt".into(), srt)],
            log_content: false,
            policy: None,
            path: deps.join("bin").to_string_lossy().into_owned(),
            userns: None,
        };
        Setup { fake, work, env }
    }

    fn seen(st: &Setup) -> String {
        std::fs::read_to_string(st.fake.dir.join("seen")).unwrap_or_default()
    }

    fn events(st: &Setup) -> Vec<serde_json::Value> {
        st.fake
            .lines()
            .into_iter()
            .filter(|l| l["op"] == "event")
            .map(|l| l["event"].clone())
            .collect()
    }

    #[test]
    fn run_records_never_hold_a_secret_from_the_harness_argv() {
        // Integration w1, security finding S13: a key passed to the harness
        // on its command line would sit in the activity log for 7 days.
        // Its own fake folder: `setup` names the folder after this label and
        // the process id, so two tests sharing a label race.
        let st = setup("none-redact");
        let cwd = st.work.to_string_lossy().into_owned();
        let secret = format!("sk-ant-{}", "x".repeat(24));
        run(
            &s(&[
                "pi",
                "--boundary",
                "none",
                "--chat",
                "chat-r",
                "--cwd",
                &cwd,
                "--",
                "--api-key",
                &secret,
                "--model=sk-ant-inline-secret-value-0001",
            ]),
            &st.env,
        )
        .unwrap();
        let ev = events(&st);
        assert_eq!(ev[0]["kind"], "run.start");
        let all = serde_json::to_string(&ev).unwrap();
        assert!(!all.contains(&secret), "{all}");
        assert!(!all.contains("sk-ant-inline"), "{all}");
        assert!(all.contains("--api-key"), "{all}");
    }

    #[test]
    fn recorded_argv_keeps_tk_s_part_and_the_words_with_content_on() {
        let args = s(&["--settings", "/x/s.json", "--", "pi", "-p", "hello there"]);
        let rest = s(&["-p", "hello there"]);
        assert_eq!(
            recorded_argv("srt", &args, &rest, false),
            s(&[
                "srt",
                "--settings",
                "/x/s.json",
                "--",
                "pi",
                "-p",
                "[11 chars]"
            ])
        );
        assert_eq!(
            recorded_argv("srt", &args, &rest, true),
            s(&[
                "srt",
                "--settings",
                "/x/s.json",
                "--",
                "pi",
                "-p",
                "hello there"
            ])
        );
        // With content on, a key is still redacted.
        let rest = s(&["--api-key", "sk-ant-aaaaaaaaaaaaaaaaaaaaaaaa"]);
        assert_eq!(
            recorded_argv("pi", &rest, &rest, true),
            s(&["pi", "--api-key", "[redacted]"])
        );
    }

    #[test]
    fn run_records_hold_no_prompt_text_from_the_harness_argv() {
        // Mac test w1, finding M8: `claude -p "<prompt>"` put the prompt in
        // the activity log. Content is off unless TK_LOG_CONTENT=1, as in the
        // emitter and the receiver: flags stay, other words become lengths.
        let st = setup("none-prompt");
        let cwd = st.work.to_string_lossy().into_owned();
        run(
            &s(&[
                "pi",
                "--boundary",
                "none",
                "--chat",
                "chat-p",
                "--cwd",
                &cwd,
                "--",
                "--model=qwen27b",
                "-p",
                "summarise my private plan",
            ]),
            &st.env,
        )
        .unwrap();
        let ev = events(&st);
        let argv = ev[0]["detail"]["argv"].clone();
        let all = serde_json::to_string(&ev).unwrap();
        assert!(!all.contains("private plan"), "{all}");
        assert!(!all.contains("qwen27b"), "{all}");
        assert_eq!(
            argv,
            serde_json::json!(["pi", "--model=[7 chars]", "-p", "[25 chars]"]),
            "{argv}"
        );
        // The harness itself still got them.
        assert!(seen(&st).contains("arg=summarise my private plan"));
    }

    #[test]
    fn none_mints_a_token_records_start_and_stop_and_returns_the_code() {
        let st = setup("none");
        let cwd = st.work.to_string_lossy().into_owned();
        // `tk run` records the folder canonicalized (macOS: /var is a link to
        // /private/var), so the expected values are too (Mac test M2).
        let real = std::fs::canonicalize(&st.work)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let code = run(
            &s(&[
                "pi",
                "--boundary",
                "none",
                "--chat",
                "chat-e",
                "--cwd",
                &cwd,
                "--",
                "-c",
                "a b",
            ]),
            &st.env,
        )
        .unwrap();
        assert_eq!(code, 3);
        let out = seen(&st);
        assert!(
            out.contains(&format!("token={}", fake::CHAT_TOKEN)),
            "{out}"
        );
        assert!(out.contains("chat=chat-e"));
        assert!(out.contains("otlp=http://127.0.0.1:4318"));
        assert!(out.contains("boundary=none"));
        assert!(out.contains(&format!("pwd={real}")), "{out}");
        assert!(out.contains("arg=-c\narg=a b\n"));
        let ops: Vec<String> = st
            .fake
            .lines()
            .iter()
            .map(|l| l["op"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ops,
            vec!["hello", "token.mint", "hello", "event", "hello", "event"]
        );
        let ev = events(&st);
        assert_eq!(ev[0]["kind"], "run.start");
        assert_eq!(ev[0]["source"], "run");
        assert_eq!(ev[0]["detail"]["runsAs"], "you");
        assert_eq!(ev[0]["detail"]["folders"], serde_json::json!([real]));
        assert!(ev[0]["pid"].as_u64().unwrap() > 0);
        assert_eq!(ev[1]["kind"], "run.stop");
        assert_eq!(ev[1]["detail"]["exitCode"], 3);
        assert_eq!(ev[0]["pid"], ev[1]["pid"]);
        // The token is in no line the daemon got but the mint reply's.
        for l in st.fake.lines() {
            assert!(!l.to_string().contains(fake::CHAT_TOKEN), "{l}");
        }
    }

    #[test]
    fn srt_writes_private_settings_and_removes_them_after() {
        let st = setup("srt");
        let cwd = st.work.to_string_lossy().into_owned();
        let code = run(
            &s(&[
                "pi",
                "--boundary",
                "srt",
                "--chat",
                "chat-s",
                "--cwd",
                &cwd,
                "--model-url",
                "http://127.0.0.1:8080",
                "--",
                "-p",
                "x",
            ]),
            &st.env,
        )
        .unwrap();
        assert_eq!(code, 3, "{}", seen(&st));
        let out = seen(&st);
        assert!(out.contains("proxy=1"));
        assert!(out.contains("arg=-p\narg=x\n"));
        let copy: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(st.fake.dir.join("settings.copy")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            copy["network"]["allowedDomains"],
            serde_json::json!(["127.0.0.1:4318", "127.0.0.1:8080"])
        );
        assert!(copy["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d == &serde_json::json!(st.fake.dir.to_string_lossy())));
        assert!(
            !st.fake.dir.join("srt/chat-s.json").exists(),
            "removed after the run"
        );
    }

    #[test]
    fn run_stop_is_recorded_after_the_daemon_restarted() {
        // Box test w1, BX8: `systemctl --user restart tk-daemon` during a
        // chat rotated the daemon's token, and the chat's `run.stop` was
        // refused ("bad token"), leaving an unpaired `run.start`. tk run
        // reads the token file again and posts once more.
        let mut st = setup("restart");
        let pi = script(
            &st.fake.dir,
            "pi-restart",
            &format!(
                "printf '%s\\n' {new} > '{dir}/daemon.token'; : > '{dir}/restarted'; exit 4",
                new = fake::RESTARTED_TOKEN,
                dir = st.fake.dir.display()
            ),
        );
        st.env.programs.retain(|(n, _)| n != "pi");
        st.env.programs.push(("pi".into(), pi));
        let cwd = st.work.to_string_lossy().into_owned();
        let code = run(
            &s(&[
                "pi",
                "--boundary",
                "none",
                "--chat",
                "chat-r2",
                "--cwd",
                &cwd,
            ]),
            &st.env,
        )
        .unwrap();
        assert_eq!(code, 4);
        let ev = events(&st);
        let kinds: Vec<&str> = ev.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["run.start", "run.stop"], "{ev:?}");
        assert_eq!(ev[1]["detail"]["daemonRestarted"], true);
        let hellos: Vec<String> = st
            .fake
            .lines()
            .iter()
            .filter(|l| l["op"] == "hello")
            .map(|l| l["token"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            hellos.last().map(String::as_str),
            Some(fake::RESTARTED_TOKEN)
        );
    }

    #[test]
    fn srt_re_opens_the_policy_only_when_it_is_a_plain_file() {
        // Fix pass review R1: a link at ~/.tk/policy.json to ~/.tk/backups
        // was bound read-only into the sandbox, backups and all.
        let st = setup("srt-link");
        let tk = st.fake.dir.join(".tk");
        std::fs::create_dir_all(tk.join("backups")).unwrap();
        let cwd = st.work.to_string_lossy().into_owned();
        let allow = |chat: &str| -> serde_json::Value {
            run(
                &s(&["pi", "--boundary", "srt", "--chat", chat, "--cwd", &cwd, "--model-url", "http://127.0.0.1:8080"]),
                &st.env,
            )
            .unwrap();
            let c: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(st.fake.dir.join("settings.copy")).unwrap(),
            )
            .unwrap();
            c["filesystem"]["allowRead"].clone()
        };
        std::os::unix::fs::symlink(tk.join("backups"), tk.join("policy.json")).unwrap();
        assert_eq!(allow("chat-l1"), serde_json::json!([]), "a link");
        std::fs::remove_file(tk.join("policy.json")).unwrap();
        std::fs::write(tk.join("backups/a.json"), "{}").unwrap();
        std::fs::hard_link(tk.join("backups/a.json"), tk.join("policy.json")).unwrap();
        assert_eq!(allow("chat-l2"), serde_json::json!([]), "a second hard link");
        std::fs::remove_file(tk.join("policy.json")).unwrap();
        std::fs::write(tk.join("policy.json"), "{}").unwrap();
        std::fs::set_permissions(tk.join("policy.json"), std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(allow("chat-l3"), serde_json::json!([]), "writable by others");
        std::fs::set_permissions(tk.join("policy.json"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let path = tk.join("policy.json").to_string_lossy().into_owned();
        assert_eq!(
            allow("chat-l4"),
            serde_json::json!([{"path": path, "literal": true}]),
            "a plain private file"
        );
    }

    #[test]
    fn srt_without_its_dependencies_starts_nothing() {
        // Box test w1, BX4: srt's own "ripgrep (rg) not found" came after
        // the token was minted; now tk refuses first and starts nothing.
        let mut st = setup("srt-deps");
        st.env.path = String::new();
        let cwd = st.work.to_string_lossy().into_owned();
        let err = run(
            &s(&["pi", "--boundary", "srt", "--chat", "chat-d", "--cwd", &cwd]),
            &st.env,
        )
        .unwrap_err();
        assert_eq!(err.0, 1);
        // Linux needs bubblewrap, socat and ripgrep; macOS ripgrep only.
        let first = if cfg!(target_os = "linux") {
            "bwrap (package bubblewrap)"
        } else {
            "rg (package ripgrep)"
        };
        assert!(err.1.contains(first), "{}", err.1);
        assert!(seen(&st).is_empty(), "the harness never started");
        assert!(!st.fake.dir.join("srt/chat-d.json").exists());
        assert!(events(&st).is_empty(), "nothing recorded");
    }

    #[test]
    fn sbx_names_the_variables_and_posts_the_blocks_its_log_shows() {
        let st = setup("sbx");
        let n = st.fake.dir.join("polls");
        // `policy log`: nothing before the start (no sandbox yet), then one
        // block. `run`: checks it got the token by name only, waits for a poll.
        let sbx = script(
            &st.fake.dir,
            "sbx",
            &format!(
                "if [ \"$1\" = policy ]; then echo x >> '{n}'; \
                 if [ \"$(wc -l < '{n}')\" -le 1 ]; then echo 'no such sandbox' >&2; exit 1; fi; \
                 echo '{{\"blocked\":[{{\"Sandbox\":\"tk-chat-x\",\"Host\":\"evil.example:443\",\"Rule\":\"<default policy>\",\"Count\":1}}]}}'; exit 0; fi; \
                 [ \"$1\" = run ] || exit 80; \
                 case \"$*\" in *\"{tok}\"*) exit 81;; esac; \
                 case \"$*\" in *\"-e TK_CHAT_TOKEN\"*) ;; *) exit 82;; esac; \
                 [ \"$TK_CHAT_TOKEN\" = {tok} ] || exit 83; \
                 [ \"$TK_OTLP_ENDPOINT\" = http://host.docker.internal:4318 ] || exit 84; \
                 sleep 3; exit 0",
                n = n.display(),
                tok = fake::CHAT_TOKEN
            ),
        );
        let mut env = RunEnv {
            socket: st.env.socket.clone(),
            home: st.env.home.clone(),
            gateway: None,
            programs: st.env.programs.clone(),
            log_content: false,
            policy: None,
            path: String::new(),
            userns: None,
        };
        env.programs.push(("sbx".into(), sbx));
        let cwd = st.work.to_string_lossy().into_owned();
        let code = run(
            &s(&["pi", "--boundary", "sbx", "--chat", "chat-x", "--cwd", &cwd]),
            &env,
        )
        .unwrap();
        assert_eq!(code, 0);
        let ev = events(&st);
        let kinds: Vec<&str> = ev.iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds.first(), Some(&"run.start"));
        assert_eq!(kinds.last(), Some(&"run.stop"));
        let block = ev
            .iter()
            .find(|e| e["kind"] == "net.connect")
            .expect("the block was posted");
        assert_eq!(block["detail"]["host"], "evil.example:443");
        assert_eq!(block["detail"]["blocked"], true);
        assert_eq!(block["chatId"], "chat-x");
        assert_eq!(
            ev.iter().filter(|e| e["kind"] == "net.connect").count(),
            1,
            "posted once"
        );
        assert_eq!(ev[0]["detail"]["kit"], "docker.io/sbx/pi-kit:latest");
        assert_eq!(ev[0]["detail"]["sandboxName"], "tk-chat-x");
    }

    #[test]
    fn no_daemon_is_an_error_and_starts_nothing() {
        let st = setup("down");
        let env = RunEnv {
            socket: Some(st.fake.dir.join("absent/daemon.sock")),
            home: st.env.home.clone(),
            gateway: None,
            programs: st.env.programs.clone(),
            log_content: false,
            policy: None,
            path: String::new(),
            userns: None,
        };
        let cwd = st.work.to_string_lossy().into_owned();
        let err = run(&s(&["pi", "--boundary", "none", "--cwd", &cwd]), &env).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(err.1.contains("--no-daemon"), "{}", err.1);
        assert!(seen(&st).is_empty(), "the harness never started");
    }

    #[test]
    fn no_daemon_flag_runs_without_token_or_records() {
        let st = setup("nod");
        let cwd = st.work.to_string_lossy().into_owned();
        let code = run(
            &s(&["pi", "--boundary", "none", "--no-daemon", "--cwd", &cwd]),
            &st.env,
        )
        .unwrap();
        assert_eq!(code, 3);
        assert!(seen(&st).contains("token=\n"));
        assert!(st.fake.lines().is_empty(), "the daemon heard nothing");
    }

    #[test]
    fn dry_run_contacts_nobody_and_starts_nothing() {
        let st = setup("dry");
        let cwd = st.work.to_string_lossy().into_owned();
        assert_eq!(
            run(
                &s(&["pi", "--boundary", "sbx", "--dry-run", "--cwd", &cwd]),
                &st.env
            )
            .unwrap(),
            0
        );
        assert!(st.fake.lines().is_empty());
        assert!(seen(&st).is_empty());
    }
}

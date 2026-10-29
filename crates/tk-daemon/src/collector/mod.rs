//! Lane K's seam in the daemon (wave 2): what runs beside lane B's daemon.
//!
//! `main.rs` calls `start` once the daemon is up and before it serves the
//! socket. It starts two things, each on its own thread, and returns at once:
//!
//! - **The collector registration** (toolkit plan T1.3, Mac half). The
//!   macOS activity collector (`macos/activity-collector/`, a root launchd
//!   daemon around `eslogger`) follows only the processes it is told about.
//!   The fork told it at each harness spawn; in tk, `tk run` posts
//!   `run.start` with the harness's pid, so the daemon tails its own feed
//!   and registers each such pid with the collector for the chat
//!   (`activity::register_process`, lane B's copy of the fork's registry,
//!   which replays the live set whenever the collector comes back). The
//!   read side already runs: `report` asks the collector for its events
//!   (lane B). Only a `run.start` the daemon scope posted counts: a chat
//!   token cannot post one (`Daemon::op_event`). macOS only.
//! - **Computer use** (T1.5), behind the `computer-use` feature, on by
//!   default since lane K: `computer_use::service`. macOS only.
//!
//! Neither makes an outbound connection (rules, section 5, item 8). The
//! collector is reached over its local sockets; its install stays behind
//! your click and your Developer ID (R10).

use std::sync::Arc;

use serde_json::Value;
use tk_core::event::{Event, Source};

use crate::activity::ProcessKind;
use crate::daemon::{Daemon, Outbox, Paths, Who};
use crate::protocol::Request;

/// What one event asks of the collector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registration {
    /// Follow `pid` and its descendants for `chat`.
    Follow { pid: u32, chat: String },
    /// The harness exited: stop replaying it to a restarted collector.
    Exited { pid: u32 },
}

/// The registration an event asks for, if any: a daemon-scope `run.start`
/// or `run.stop` with a pid, for a chat id the collector takes.
pub fn registration(event: &Event) -> Option<Registration> {
    if event.source != Source::Run {
        return None;
    }
    let pid = event.pid.filter(|p| *p > 1)?;
    match event.kind.as_str() {
        "run.start" if crate::activity::registry::valid_chat_id(&event.chat_id) => {
            Some(Registration::Follow {
                pid,
                chat: event.chat_id.clone(),
            })
        }
        "run.stop" => Some(Registration::Exited { pid }),
        _ => None,
    }
}

/// Applies one registration through lane B's registry.
fn apply(registration: Registration) {
    match registration {
        Registration::Follow { pid, chat } => {
            crate::activity::register_process(pid, &chat, ProcessKind::Harness)
        }
        Registration::Exited { pid } => crate::activity::process_exited(pid),
    }
}

/// The event inside one `tail` line, if the line is a pushed event.
pub fn pushed_event(line: &str) -> Option<Event> {
    let value: Value = serde_json::from_str(line).ok()?;
    serde_json::from_value(value.get("event")?.clone()).ok()
}

/// Subscribes to every event the daemon takes from now on, as the daemon
/// scope, and calls `each` for every pushed event until the daemon drops
/// the subscription. Runs on the caller's thread.
pub fn follow(daemon: &Arc<Daemon>, mut each: impl FnMut(Event)) {
    let (out, rx, _) = Outbox::new(daemon.next_conn());
    daemon.handle(
        &Who::Daemon,
        Request::Tail {
            since: None,
            chat: None,
        },
        &out,
    );
    for line in rx {
        if let Some(event) = pushed_event(&line) {
            each(event);
        }
    }
    daemon.forget(out.conn);
}

/// The collector's install plan for a development build (the fork's
/// terminal route, `activity/collector.rs` `terminal_plan`, lane B's copy):
/// the exact commands, the plist and the hashes, for you to read and run
/// yourself in Terminal (rules, section 5, item 2). Nothing here runs them.
///
/// - `binary`: the collector you built (`cargo build --release` in
///   `macos/activity-collector`), absolute, a regular file. Its SHA-256 goes
///   into the root shell that installs it, so a swapped file is refused.
/// - `peer`: the daemon binary the collector will take as its only caller
///   (`--dev-peer-path`, exact path; PROTOCOL.md "Caller check"). Usually
///   the running `tk-daemon`.
///
/// The plist is staged in `<data>/activity/install/` (0600), as the fork
/// staged it in the app's data folder. The names are still MonoCode's
/// (`com.monocode.desktop.activity-collector`) until the toolkit's name is
/// settled (toolkit plan, section 11, call 1): the collector's own
/// identifiers and socket paths change with them, in one commit.
pub fn install_plan(
    paths: &Paths,
    binary: &std::path::Path,
    peer: &std::path::Path,
) -> Result<crate::activity::collector::InstallPlan, String> {
    use crate::activity::collector;
    for (what, path) in [("collector binary", binary), ("daemon binary", peer)] {
        if !path.is_absolute() {
            return Err(format!("the {what} needs an absolute path"));
        }
        let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{what}: {e}"))?;
        if !meta.file_type().is_file() {
            return Err(format!("the {what} is not a regular file"));
        }
    }
    let hash = collector::file_sha256(binary).map_err(|e| format!("collector binary: {e}"))?;
    let names = collector::Names::current();
    let stage = paths.data.join("activity").join("install");
    crate::socket::private_dir(&stage).map_err(|e| format!("stage folder: {e}"))?;
    let staged = stage.join(format!("{}.plist", names.label));
    let mut plan = collector::terminal_plan(
        &names,
        &binary.to_string_lossy(),
        &hash,
        &staged.to_string_lossy(),
        &peer.to_string_lossy(),
    )?;
    let peer_hash = collector::file_sha256(peer).map_err(|e| format!("daemon binary: {e}"))?;
    plan.explanation = explanation(
        &binary.to_string_lossy(),
        &hash,
        &peer.to_string_lossy(),
        &peer_hash,
        writable_by_me(peer),
        &staged.to_string_lossy(),
        &names.label,
    );
    crate::service::write_private(&staged, &plan.plist).map_err(|e| format!("plist: {e}"))?;
    Ok(plan)
}

/// The plan's words for tk (Mac test w2, MW5). The fork's text
/// (`activity/collector.rs` `explanation`) spoke for MonoCode's signed app;
/// tk's development install trusts its caller by path, which is weaker, and
/// says what that costs.
fn explanation(
    binary: &str,
    binary_sha: &str,
    peer: &str,
    peer_sha: &str,
    peer_writable: bool,
    staged: &str,
    label: &str,
) -> Vec<String> {
    let mut lines = vec![
        "The collector runs as root, outside any sandbox, and starts /usr/bin/eslogger.".to_string(),
        "It needs Full Disk Access, which you grant it in System Settings, Privacy & Security.".to_string(),
        "It records only: processes `tk run` started for a chat (the daemon names each one) and their children, and apps you granted for computer use while the grant holds.".to_string(),
        "It records what they ran, the files they created, wrote, moved or deleted, and opens of sensitive files such as ~/.ssh. Command lines are kept.".to_string(),
        "It blocks nothing. It sends nothing anywhere. Its log is root-owned, kept 7 days, at most 1 GB.".to_string(),
        format!("It answers one caller: the program at {peer} (SHA-256 {peer_sha} today), matched by its exact path, because this build is unsigned. That is weaker than a signature."),
    ];
    if peer_writable {
        lines.push(format!(
            "You can write {peer} or a folder on the way to it, so any process that runs as you can replace that file, an agent under `tk run --boundary none` included, and the replacement becomes the collector's caller. To close that, copy tk-daemon into a folder only root can write, run the daemon from there, and give that path to collector-plan."
        ));
    } else {
        lines.push(format!(
            "Neither {peer} nor any folder on the way to it is yours or writable by you, so a process running as you cannot replace it (an administrator still can)."
        ));
    }
    lines.push(format!(
        "The commands check the collector binary and the plist against the hashes shown here before anything starts. The binary installed is {binary}, SHA-256 {binary_sha}."
    ));
    lines.push(format!(
        "Its names are still MonoCode's: the label {label}, /var/run/monocode-activity and /Library/Application Support/MonoCode/activity, until tk's name is settled (toolkit plan, section 11, call 1)."
    ));
    lines.push(format!(
        "This plan ran no command. It wrote one file, the staged plist at {staged} (0600), which the first command copies."
    ));
    lines
}

/// Whether this user could replace `path`: the file, or any folder on the
/// way to it, is this user's or writable by this user, by owner, group, other
/// bits or an ACL, as the kernel decides it (`faccessat` with `AT_EACCESS`).
/// A folder anywhere up the chain decides a rename that swaps the exact path,
/// so every one is checked, for the path as given and for its real path
/// (fix pass 1's review, FP1-S3: the first version looked at the file and
/// its folder, by owner and other-write only). A path it cannot check counts
/// as writable.
fn writable_by_me(path: &std::path::Path) -> bool {
    writable_with(path, can_change)
}

fn writable_with(path: &std::path::Path, can: impl Fn(&std::path::Path) -> bool) -> bool {
    let mut chains = vec![path.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(path) {
        chains.push(real);
    }
    chains
        .iter()
        .any(|p| p.ancestors().any(|a| !a.as_os_str().is_empty() && can(a)))
}

/// One path: owned by this user (who can always chmod it), or writable by
/// this user's effective ids.
fn can_change(p: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no failure mode.
    let me = unsafe { libc::getuid() };
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return true;
    };
    if meta.uid() == me {
        return true;
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: a valid C string; faccessat only reads it. AT_EACCESS checks
    // the effective ids, as a rename would. AT_SYMLINK_NOFOLLOW is not passed:
    // a link's own entry is its folder's, which the chain checks.
    unsafe { libc::faccessat(libc::AT_FDCWD, c.as_ptr(), libc::W_OK, libc::AT_EACCESS) == 0 }
}

/// Starts what lane K adds to a running daemon. Returns at once.
pub fn start(daemon: &Arc<Daemon>, paths: &Paths) {
    if cfg!(target_os = "macos") {
        let daemon = daemon.clone();
        let _ = std::thread::Builder::new()
            .name("tk-daemon-collector".into())
            .spawn(move || {
                follow(&daemon, |event| {
                    if let Some(r) = registration(&event) {
                        apply(r);
                    }
                })
            });
    }
    #[cfg(feature = "computer-use")]
    crate::computer_use::service::start(daemon, paths);
    #[cfg(not(feature = "computer-use"))]
    let _ = paths;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_event(kind: &str, chat: &str, pid: Option<u32>, source: Source) -> Event {
        Event {
            id: format!("{kind}-1"),
            at: 0,
            chat_id: chat.into(),
            harness: "pi".into(),
            source,
            kind: kind.into(),
            summary: String::new(),
            detail: json!({}),
            pid,
            approval_id: None,
        }
    }

    #[test]
    fn a_run_start_with_a_pid_is_followed_for_its_chat() {
        assert_eq!(
            registration(&run_event("run.start", "chat-1", Some(4242), Source::Run)),
            Some(Registration::Follow {
                pid: 4242,
                chat: "chat-1".into()
            })
        );
        assert_eq!(
            registration(&run_event("run.stop", "chat-1", Some(4242), Source::Run)),
            Some(Registration::Exited { pid: 4242 })
        );
    }

    #[test]
    fn nothing_else_registers_a_process() {
        // A chat token posts only adapter and otel records, so a `run.start`
        // of any other source is not one tk run wrote.
        for source in [
            Source::Adapter,
            Source::Otel,
            Source::Eslogger,
            Source::Auditd,
            Source::Cua,
            Source::Approval,
        ] {
            assert_eq!(
                registration(&run_event("run.start", "chat-1", Some(4242), source)),
                None
            );
        }
        // No pid, pid 0 or 1 (init), a chat id the collector refuses.
        assert_eq!(
            registration(&run_event("run.start", "chat-1", None, Source::Run)),
            None
        );
        for pid in [0, 1] {
            assert_eq!(
                registration(&run_event("run.start", "chat-1", Some(pid), Source::Run)),
                None
            );
        }
        assert_eq!(
            registration(&run_event("run.start", "bad id/..", Some(9), Source::Run)),
            None
        );
        assert_eq!(
            registration(&run_event("tool.call", "chat-1", Some(9), Source::Run)),
            None
        );
    }

    #[test]
    fn a_pushed_line_yields_its_event_and_nothing_else_does() {
        let event = run_event("run.start", "chat-1", Some(7), Source::Run);
        let line = serde_json::to_string(&crate::protocol::Push::Event(event.clone())).unwrap();
        assert_eq!(pushed_event(&line), Some(event));
        assert_eq!(pushed_event(r#"{"ok":true}"#), None);
        assert_eq!(pushed_event("not json"), None);
        assert_eq!(
            pushed_event(
                r#"{"approve":{"id":"a","chat":"c","harness":"pi","tool":"t","summary":""}}"#
            ),
            None
        );
    }

    #[test]
    fn follow_sees_what_the_daemon_scope_posts_and_registers_it() {
        use crate::activity::store::ActivityLog;
        use std::sync::mpsc;
        let root =
            std::env::temp_dir().join(format!("tk-collector-{}", uuid::Uuid::new_v4().simple()));
        let data = root.join("data");
        let log: &'static ActivityLog =
            Box::leak(Box::new(ActivityLog::start(data.join("activity"))));
        let daemon = Daemon::new(
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            log,
            &data,
            false,
        )
        .unwrap();
        let (tx, rx) = mpsc::channel();
        let following = daemon.clone();
        std::thread::spawn(move || {
            follow(&following, |event| {
                let _ = tx.send(registration(&event));
            })
        });
        // The subscription is made on the follower's thread; post until it
        // hears one, so the test does not race the subscribe.
        let (out, replies, _) = Outbox::new(daemon.next_conn());
        let mut seen = None;
        for n in 0..50 {
            daemon.handle(
                &Who::Daemon,
                Request::Event {
                    event: run_event("run.start", "chat-2", Some(5000 + n), Source::Run),
                },
                &out,
            );
            let _ = replies.recv_timeout(std::time::Duration::from_secs(5));
            if let Ok(r) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                seen = r;
                break;
            }
        }
        match seen {
            Some(Registration::Follow { chat, .. }) => assert_eq!(chat, "chat-2"),
            other => panic!("no registration: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_install_plan_pins_the_binary_and_the_peer_and_stages_the_plist() {
        let root = std::env::temp_dir().join(format!(
            "tk-collector-plan-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("monocode-activity-collector");
        std::fs::write(&binary, b"not really a collector").unwrap();
        let peer = root.join("tk-daemon");
        std::fs::write(&peer, b"daemon").unwrap();
        let paths = Paths {
            socket: root.join("run/tk/daemon.sock"),
            data: root.join("data"),
        };
        let plan = install_plan(&paths, &binary, &peer).unwrap();
        assert_eq!(plan.route, "terminal");
        let hash = crate::activity::collector::file_sha256(&binary).unwrap();
        assert!(
            plan.commands.iter().any(|c| c.contains(&hash)),
            "{:?}",
            plan.commands
        );
        assert!(plan.plist.contains("--dev-peer-path"));
        assert!(plan.plist.contains(&*peer.to_string_lossy()));
        let staged = plan.staged_plist.clone().unwrap();
        assert_eq!(std::fs::read_to_string(&staged).unwrap(), plan.plist);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // Every command names its tools by absolute path.
        assert!(plan
            .commands
            .iter()
            .all(|c| c.starts_with("/usr/bin/sudo /")));

        // A relative path, a link and a missing file are refused.
        assert!(install_plan(&paths, std::path::Path::new("rel/bin"), &peer).is_err());
        let link = root.join("link");
        std::os::unix::fs::symlink(&binary, &link).unwrap();
        assert!(install_plan(&paths, &link, &peer).is_err());
        assert!(install_plan(&paths, &root.join("absent"), &peer).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_peer_is_replaceable_when_any_folder_up_its_path_is_ours() {
        // Fix pass 1's review, FP1-S3: the first check looked at the file
        // and its own folder, by owner and other-write. A user-owned /opt/tk
        // above a root-owned /opt/tk/bin/tk-daemon still lets the user rename
        // bin away and put another tk-daemon at the exact path.
        let only = |hit: &'static str| move |p: &std::path::Path| p == std::path::Path::new(hit);
        let peer = std::path::Path::new("/opt/tk/bin/tk-daemon");
        assert!(writable_with(peer, only("/opt/tk")));
        assert!(writable_with(peer, only("/opt")));
        assert!(writable_with(peer, only("/opt/tk/bin/tk-daemon")));
        assert!(!writable_with(peer, only("/elsewhere")));
        // On the real system: /bin/sh is root's all the way up, so only root
        // can replace it (CI's macOS runner and a Linux user both run this
        // as a user).
        // SAFETY: getuid has no failure mode.
        let root = unsafe { libc::getuid() } == 0;
        assert_eq!(writable_by_me(std::path::Path::new("/bin/sh")), root);
        // A folder of ours anywhere up the given path counts, even when the
        // path is a link to a file we cannot write.
        let dir = std::env::temp_dir().join(format!("tk-chain-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let link = dir.join("bin").join("tk-daemon");
        std::os::unix::fs::symlink("/bin/sh", &link).unwrap();
        assert!(writable_by_me(&link));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_plan_speaks_for_tk_and_says_what_trusting_the_peer_by_path_costs() {
        // Mac test w2, MW5: the plan said "Only MonoCode may talk to it" and
        // "processes MonoCode started", and did not say that the caller it
        // trusts by path sits in a folder you (and an agent running as you)
        // can write; and it writes a staged plist, which "runs nothing" hid.
        let root = std::env::temp_dir().join(format!(
            "tk-collector-words-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let binary = root.join("monocode-activity-collector");
        std::fs::write(&binary, b"not really a collector").unwrap();
        let peer = root.join("tk-daemon");
        std::fs::write(&peer, b"daemon").unwrap();
        let paths = Paths {
            socket: root.join("run/tk/daemon.sock"),
            data: root.join("data"),
        };
        let plan = install_plan(&paths, &binary, &peer).unwrap();
        let text = plan.explanation.join("\n");
        assert!(!text.contains("Only MonoCode"), "{text}");
        assert!(!text.contains("processes MonoCode started"), "{text}");
        assert!(text.contains("tk run"), "{text}");
        let peer_hash = crate::activity::collector::file_sha256(&peer).unwrap();
        assert!(
            text.contains(&*peer.to_string_lossy()) && text.contains(&peer_hash),
            "{text}"
        );
        assert!(text.contains("can replace that file"), "{text}");
        assert!(
            text.contains("wrote one file") && text.contains(&plan.staged_plist.clone().unwrap()),
            "{text}"
        );
        assert!(
            text.contains("com.monocode.desktop.activity-collector"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}

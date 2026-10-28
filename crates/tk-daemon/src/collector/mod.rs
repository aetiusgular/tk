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
}

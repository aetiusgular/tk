//! The feed's own first look at an action, stamped on the event as
//! `detail.preflags`. The daemon's checks (`crates/tk-daemon/src/activity/
//! check.rs`, lane B) stay the ones that decide; these exist so the box
//! test's "a write outside the folder shows a flag within a second" can be
//! seen in `watch tail` from the feed alone. A preflag can only add to what
//! the daemon decides, never clear it (rules case 35).
//!
//! The rules mirror check.rs: `inside` read literally with `..` refused
//! (`check.rs:145-152`), the same scratch and harness-state noise
//! (`check.rs:249-300`), sensitive paths (`sensitive.rs`), and a host flagged
//! once per chat (`check.rs:1853`).

use std::collections::HashSet;

use crate::action::{Action, What};
use crate::sensitive::{is_sensitive, is_trusted_file};
use crate::tree::Root;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// A write, rename, unlink, link or truncate outside the chat's folders.
    WriteOutside,
    /// A read or write of a secret.
    Sensitive,
    /// A write to a file something else will run (git hooks, shell rc).
    TrustedFile,
    /// A connection to an address the chat has not reached before.
    NewHost,
    /// The kernel or the boundary refused the call (EPERM, EACCES, EROFS).
    Denied,
}

impl Reason {
    pub fn key(self) -> &'static str {
        match self {
            Reason::WriteOutside => "writeOutside",
            Reason::Sensitive => "sensitive",
            Reason::TrustedFile => "trustedFile",
            Reason::NewHost => "newHost",
            Reason::Denied => "denied",
        }
    }
}

/// True when `path` is the folder or below it, read literally; `..`
/// anywhere is never inside (as `check.rs:145-152`).
pub fn inside(path: &str, folder: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    if folder.is_empty() || !path.starts_with('/') || path.split('/').any(|p| p == "..") {
        return false;
    }
    path == folder || path.starts_with(&format!("{folder}/"))
}

/// Scratch space and the harnesses' own state, as `check.rs` `noise_path`
/// reads it, in Linux paths. A path that could not be resolved (`?name`)
/// is never noise.
pub fn noise(path: &str, home: &str, state: &[String]) -> bool {
    const SYSTEM: &[&str] = &["/tmp", "/var/tmp", "/dev", "/proc"];
    if SYSTEM.iter().any(|p| inside(path, p)) {
        return true;
    }
    let home = home.trim_end_matches('/');
    // Folders inside a harness home that the harness runs as code are never
    // noise (`check.rs` HARNESS_RUNS, review Y1).
    const RUNS: &[&str] = &[
        "shell-snapshots",
        "session-env",
        "shell_snapshots",
        "extensions",
        "hooks",
    ];
    let runs_code = |dir: &str| {
        path.strip_prefix(&format!("{}/", dir.trim_end_matches('/')))
            .and_then(|rest| rest.split('/').next())
            .is_some_and(|first| RUNS.contains(&first))
    };
    let mut dirs: Vec<String> = [".claude", ".codex", ".cache", ".npm", ".cargo/registry"]
        .iter()
        .filter(|_| !home.is_empty())
        .map(|d| format!("{home}/{d}"))
        .collect();
    dirs.extend(state.iter().cloned());
    if dirs.iter().any(|d| inside(path, d) && !runs_code(d)) {
        return !is_sensitive(path, home, None);
    }
    !home.is_empty() && path == format!("{home}/.claude.json")
}

fn loopback(addr: &str) -> bool {
    addr.starts_with("127.") || addr == "::1" || addr.starts_with("::ffff:127.")
}

#[derive(Default)]
pub struct Checker {
    seen_hosts: HashSet<(String, String)>,
}

impl Checker {
    pub fn new() -> Checker {
        Checker::default()
    }

    /// Forgets a stopped chat's hosts.
    pub fn forget(&mut self, chat: &str) {
        self.seen_hosts.retain(|(c, _)| c != chat);
    }

    /// Keeps only the hosts of these chats.
    pub fn retain_chats(&mut self, chats: &std::collections::HashSet<String>) {
        self.seen_hosts.retain(|(c, _)| chats.contains(c));
    }

    pub fn check(
        &mut self,
        root: &Root,
        action: &Action,
        home: &str,
        runtime_dir: Option<&str>,
    ) -> Vec<Reason> {
        let mut out = Vec::new();
        if action.denied {
            out.push(Reason::Denied);
        }
        let written: Vec<&str> = match &action.what {
            What::Write { path, .. } | What::Truncate { path } | What::Unlink { path } => {
                vec![path]
            }
            What::Rename { from, to } => vec![from, to],
            What::Link { to, .. } => vec![to],
            _ => vec![],
        };
        if !action.denied {
            if written
                .iter()
                .any(|p| !root.folders.iter().any(|f| inside(p, f)) && !noise(p, home, &root.state))
            {
                out.push(Reason::WriteOutside);
            }
            if written.iter().any(|p| is_trusted_file(p, home)) {
                out.push(Reason::TrustedFile);
            }
        }
        let touched_secret = match &action.what {
            What::Open { path } => {
                is_sensitive(path, home, runtime_dir) && !project_env(path, &root.folders)
            }
            _ => written
                .iter()
                .any(|p| is_sensitive(p, home, runtime_dir) && !project_env(p, &root.folders)),
        };
        if touched_secret {
            out.push(Reason::Sensitive);
        }
        if let What::Connect { addr, port, .. } = &action.what {
            if !loopback(addr) {
                let host = host_key(addr, *port);
                if self.seen_hosts.insert((root.chat.clone(), host)) {
                    out.push(Reason::NewHost);
                }
            }
        }
        out
    }
}

/// `addr:port`, with IPv6 in brackets.
pub fn host_key(addr: &str, port: u16) -> String {
    if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

/// A project's own `.env` inside the chat's folders is expected
/// (`check.rs` `expected_sensitive`).
fn project_env(path: &str, folders: &[String]) -> bool {
    let name = path.rsplit('/').next().unwrap_or("");
    (name == ".env" || name.starts_with(".env.")) && folders.iter().any(|f| inside(path, f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Family;

    const H: &str = "/home/ubuntu";

    fn root() -> Root {
        Root {
            chat: "c1".into(),
            harness: "pi".into(),
            pid: 10,
            folders: vec!["/home/ubuntu/proj".into()],
            state: vec!["/home/ubuntu/.pi/agent".into()],
            at: 0,
            stopped: false,
        }
    }

    fn act(what: What) -> Action {
        Action {
            ts_ms: 1,
            serial: 1,
            pid: 10,
            ppid: 1,
            uid: Some(1000),
            comm: "x".into(),
            exe: "/x".into(),
            key: "".into(),
            syscall: "openat",
            denied: false,
            cwd: None,
            what,
        }
    }

    fn write(p: &str) -> What {
        What::Write {
            path: p.into(),
            created: false,
            flags_known: true,
        }
    }

    #[test]
    fn writes_inside_pass_and_outside_flag() {
        let mut c = Checker::new();
        assert!(c
            .check(&root(), &act(write("/home/ubuntu/proj/a.txt")), H, None)
            .is_empty());
        assert_eq!(
            c.check(&root(), &act(write("/home/ubuntu/out.txt")), H, None),
            vec![Reason::WriteOutside]
        );
        assert_eq!(
            c.check(
                &root(),
                &act(write("/home/ubuntu/proj/../out.txt")),
                H,
                None
            ),
            vec![Reason::WriteOutside, Reason::Sensitive],
            "`..` is outside, and read as sensitive"
        );
        assert_eq!(
            c.check(&root(), &act(write("?rel.txt")), H, None),
            vec![Reason::WriteOutside]
        );
    }

    #[test]
    fn noise_and_harness_state_pass_but_code_folders_do_not() {
        let mut c = Checker::new();
        for p in [
            "/tmp/x",
            "/dev/null",
            "/home/ubuntu/.cache/pip/x",
            "/home/ubuntu/.pi/agent/sessions/s.jsonl",
        ] {
            assert!(c.check(&root(), &act(write(p)), H, None).is_empty(), "{p}");
        }
        let r = c.check(
            &root(),
            &act(write("/home/ubuntu/.pi/agent/extensions/x.ts")),
            H,
            None,
        );
        assert_eq!(r, vec![Reason::WriteOutside, Reason::TrustedFile]);
        let r = c.check(
            &root(),
            &act(write("/home/ubuntu/.pi/agent/auth.json")),
            H,
            None,
        );
        assert_eq!(r, vec![Reason::WriteOutside, Reason::Sensitive]);
    }

    #[test]
    fn a_rename_out_flags_and_a_hook_inside_flags() {
        let mut c = Checker::new();
        let r = c.check(
            &root(),
            &act(What::Rename {
                from: "/home/ubuntu/proj/a".into(),
                to: "/home/ubuntu/Desktop/a".into(),
            }),
            H,
            None,
        );
        assert_eq!(r, vec![Reason::WriteOutside]);
        let r = c.check(
            &root(),
            &act(write("/home/ubuntu/proj/.git/hooks/pre-commit")),
            H,
            None,
        );
        assert_eq!(r, vec![Reason::TrustedFile]);
    }

    #[test]
    fn sensitive_reads_flag_but_the_projects_env_does_not() {
        let mut c = Checker::new();
        let r = c.check(
            &root(),
            &act(What::Open {
                path: "/home/ubuntu/.ssh/id_test".into(),
            }),
            H,
            None,
        );
        assert_eq!(r, vec![Reason::Sensitive]);
        let r = c.check(
            &root(),
            &act(What::Open {
                path: "/home/ubuntu/proj/.env".into(),
            }),
            H,
            None,
        );
        assert!(r.is_empty());
    }

    #[test]
    fn a_new_host_flags_once_per_chat_and_loopback_never() {
        let mut c = Checker::new();
        let to = |a: &str| {
            act(What::Connect {
                family: Family::Inet,
                addr: a.into(),
                port: 443,
            })
        };
        assert_eq!(
            c.check(&root(), &to("93.184.215.14"), H, None),
            vec![Reason::NewHost]
        );
        assert!(c.check(&root(), &to("93.184.215.14"), H, None).is_empty());
        assert!(c.check(&root(), &to("127.0.0.1"), H, None).is_empty());
        c.forget("c1");
        assert_eq!(
            c.check(&root(), &to("93.184.215.14"), H, None),
            vec![Reason::NewHost]
        );
    }

    #[test]
    fn a_denied_write_is_denied_not_outside() {
        let mut c = Checker::new();
        let mut a = act(write("/home/ubuntu/out.txt"));
        a.denied = true;
        assert_eq!(c.check(&root(), &a, H, None), vec![Reason::Denied]);
    }
}

//! Which chat an action belongs to, by process tree.
//!
//! The kernel cannot filter by tree (auditctl 3.1.2 has `ppid`, one level,
//! and nothing that follows descendants), so the rules filter by user and
//! this places each action. A process is in a chat's tree when it is the
//! root `tk run` started, when a member forked it (the parent's `clone`,
//! `fork`, `vfork` or `clone3` record names the child in `exit`), or when
//! its `ppid` is a member. The fork record matters: an orphan made with
//! `( setsid sh -c ... & )` already has `ppid=1` on its own first record
//! (`tests/fixtures/scenario1.raw.audit`, pid 1037).
//!
//! Records can arrive before `tk run`'s `run.start` reaches the feed (the
//! harness's own exec is the first). Unplaced actions wait in a bounded
//! buffer and are replayed when a root registers.

use std::collections::{HashMap, VecDeque};

use crate::action::{Action, What};

/// How long an unplaced action waits for its root, by its own timestamp.
pub const PENDING_MS: i64 = 15_000;
/// At most this many unplaced actions wait.
pub const PENDING_MAX: usize = 20_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub chat: String,
    pub harness: String,
    pub pid: u32,
    /// The chat's folders: writes inside them are expected.
    pub folders: Vec<String>,
    /// The harness's own state folders (`~/.pi/agent` for Pi), noise for
    /// the write check.
    pub state: Vec<String>,
    pub at: i64,
    /// `run.stop` was seen; the root process has ended.
    pub stopped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub chat: String,
    pub harness: String,
    pub root_pid: u32,
    /// The action is the root process's own.
    pub is_root: bool,
}

#[derive(Default)]
pub struct Tracker {
    roots: HashMap<u32, Root>,
    members: HashMap<u32, u32>,
    pending: VecDeque<Action>,
    dropped: u64,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    pub fn root(&self, root_pid: u32) -> Option<&Root> {
        self.roots.get(&root_pid)
    }

    pub fn roots(&self) -> impl Iterator<Item = &Root> {
        self.roots.values()
    }

    /// Unplaced actions dropped because the buffer was full or they aged out.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Registers a root and replays the waiting actions, returning those now
    /// placed in timestamp order. A second root with the same pid replaces
    /// the first (a reused pid after the first chat stopped).
    pub fn register(&mut self, root: Root) -> Vec<(Action, Placement)> {
        let pid = root.pid;
        self.unregister_pid(pid);
        self.members.insert(pid, pid);
        self.roots.insert(pid, root);
        let waiting: Vec<Action> = self.pending.drain(..).collect();
        let mut placed = Vec::new();
        for action in waiting {
            match self.place_inner(&action) {
                Some(p) => placed.push((action, p)),
                None => self.pending.push_back(action),
            }
        }
        placed
    }

    /// A chat's root process ended. Members still alive stay placed, so a
    /// process the harness left running (a daemon it started) is still the
    /// chat's; dead members are dropped, and the root goes when none is left.
    pub fn stop(&mut self, chat: &str, alive: &dyn Fn(u32) -> bool) {
        for r in self.roots.values_mut().filter(|r| r.chat == chat) {
            r.stopped = true;
        }
        self.prune(alive);
    }

    /// Drops members that no longer run, and stopped roots with no member
    /// left. The feed calls it after a stop and once a minute.
    pub fn prune(&mut self, alive: &dyn Fn(u32) -> bool) {
        let stopped: Vec<u32> = self
            .roots
            .values()
            .filter(|r| r.stopped)
            .map(|r| r.pid)
            .collect();
        self.members.retain(|pid, root| {
            if stopped.contains(root) {
                // The root process itself has ended; its pid may be reused.
                *pid != *root && alive(*pid)
            } else {
                *pid == *root || alive(*pid)
            }
        });
        let held: std::collections::HashSet<u32> = self.members.values().copied().collect();
        self.roots.retain(|pid, r| !r.stopped || held.contains(pid));
    }

    /// Drops a chat's root and every member under it.
    pub fn unregister(&mut self, chat: &str) {
        let pids: Vec<u32> = self
            .roots
            .values()
            .filter(|r| r.chat == chat)
            .map(|r| r.pid)
            .collect();
        for pid in pids {
            self.unregister_pid(pid);
        }
    }

    fn unregister_pid(&mut self, root_pid: u32) {
        if self.roots.remove(&root_pid).is_some() {
            self.members.retain(|_, r| *r != root_pid);
        }
    }

    /// Places one action, or keeps it waiting. Fork actions update the tree.
    pub fn place(&mut self, action: &Action) -> Option<Placement> {
        let placed = self.place_inner(action);
        if placed.is_none() {
            self.wait(action.clone());
        }
        placed
    }

    fn place_inner(&mut self, action: &Action) -> Option<Placement> {
        let root_pid = match self.members.get(&action.pid) {
            Some(r) => *r,
            None => {
                let r = *self.members.get(&action.ppid)?;
                self.members.insert(action.pid, r);
                r
            }
        };
        if let What::Fork { child } = action.what {
            self.members.insert(child, root_pid);
        }
        let root = self.roots.get(&root_pid)?;
        Some(Placement {
            chat: root.chat.clone(),
            harness: root.harness.clone(),
            root_pid,
            is_root: action.pid == root_pid,
        })
    }

    fn wait(&mut self, action: Action) {
        // A fork outside every tree that hands out a pid a tree still holds
        // means that pid was reused: the new process is not the member.
        if let What::Fork { child } = action.what {
            if self.members.get(&child).is_some_and(|r| *r != child) {
                self.members.remove(&child);
            }
        }
        let newest = action.ts_ms;
        self.pending.push_back(action);
        while self
            .pending
            .front()
            .is_some_and(|a| newest - a.ts_ms > PENDING_MS)
            || self.pending.len() > PENDING_MAX
        {
            self.pending.pop_front();
            self.dropped += 1;
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(ts: i64, pid: u32, ppid: u32, what: What) -> Action {
        Action {
            ts_ms: ts,
            serial: ts as u64,
            pid,
            ppid,
            uid: Some(1000),
            comm: "x".into(),
            exe: "/bin/x".into(),
            key: "tk_exec".into(),
            syscall: "execve",
            denied: false,
            cwd: None,
            what,
        }
    }

    fn root(pid: u32) -> Root {
        Root {
            chat: "c1".into(),
            harness: "pi".into(),
            pid,
            folders: vec!["/w".into()],
            state: vec![],
            at: 0,
            stopped: false,
        }
    }

    fn exec() -> What {
        What::Exec {
            argv: vec!["x".into()],
        }
    }

    #[test]
    fn an_orphan_is_followed_through_its_fork_record() {
        let mut t = Tracker::new();
        t.register(root(10));
        assert!(t.place(&act(1, 10, 1, What::Fork { child: 11 })).is_some());
        assert!(t.place(&act(2, 11, 10, What::Fork { child: 12 })).is_some());
        // 12 was reparented to init before its first own record.
        let p = t.place(&act(3, 12, 1, exec())).unwrap();
        assert_eq!(p.chat, "c1");
        assert!(!p.is_root);
        assert!(t.place(&act(4, 10, 1, exec())).unwrap().is_root);
    }

    #[test]
    fn records_before_the_root_registers_are_replayed() {
        let mut t = Tracker::new();
        assert!(t.place(&act(1, 10, 5, exec())).is_none());
        assert!(t.place(&act(2, 10, 5, What::Fork { child: 11 })).is_none());
        assert!(t.place(&act(3, 11, 10, exec())).is_none());
        assert!(t.place(&act(3, 99, 5, exec())).is_none());
        let placed = t.register(root(10));
        assert_eq!(placed.len(), 3);
        assert_eq!(t.pending_len(), 1, "the unrelated process still waits");
    }

    #[test]
    fn the_pending_buffer_is_bounded_in_time() {
        let mut t = Tracker::new();
        t.place(&act(0, 50, 1, exec()));
        t.place(&act(PENDING_MS + 1, 51, 1, exec()));
        assert_eq!(t.pending_len(), 1);
        assert_eq!(t.dropped(), 1);
    }

    #[test]
    fn an_unregistered_chat_places_nothing_more() {
        let mut t = Tracker::new();
        t.register(root(10));
        t.place(&act(1, 10, 1, What::Fork { child: 11 }));
        t.unregister("c1");
        assert!(t.place(&act(2, 11, 10, exec())).is_none());
    }

    #[test]
    fn a_process_left_running_after_the_stop_stays_the_chats() {
        let mut t = Tracker::new();
        t.register(root(10));
        t.place(&act(1, 10, 1, What::Fork { child: 11 }));
        t.place(&act(1, 10, 1, What::Fork { child: 12 }));
        // The root (10) and 12 have exited; 11 is a daemon the harness left.
        t.stop("c1", &|pid| pid == 11);
        assert_eq!(t.place(&act(2, 11, 1, exec())).unwrap().chat, "c1");
        assert!(t.place(&act(2, 12, 1, exec())).is_none());
        // Once 11 is gone too, the root goes.
        t.prune(&|_| false);
        assert!(t.root(10).is_none());
        assert!(t.place(&act(3, 11, 1, exec())).is_none());
    }

    #[test]
    fn a_reused_pid_forked_from_outside_leaves_the_tree() {
        let mut t = Tracker::new();
        t.register(root(10));
        t.place(&act(1, 10, 1, What::Fork { child: 11 }));
        // 11 exits; an unrelated shell forks a new 11.
        t.place(&act(2, 300, 1, What::Fork { child: 11 }));
        assert!(t.place(&act(3, 11, 300, exec())).is_none());
    }

    #[test]
    fn unrelated_processes_of_the_same_user_are_not_placed() {
        let mut t = Tracker::new();
        t.register(root(10));
        assert!(t.place(&act(1, 77, 1, exec())).is_none());
    }
}

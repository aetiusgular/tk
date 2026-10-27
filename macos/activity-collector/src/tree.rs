//! The process tracker: which pids belong to which chat.
//!
//! A registered pid (a root) and every descendant, through `fork` and
//! `exec`, until it exits. During an open grant, any process whose
//! executable sits under `<appPath>/Contents/`, and its descendants, are
//! attributed to the grant's chat. Everything else is untracked. All maps
//! are bounded.

use std::collections::{BTreeMap, HashMap};

use crate::event::{RawEvent, RawKind};

/// At most this many registered roots.
pub const MAX_ROOTS: usize = 4096;
/// At most this many open grants.
pub const MAX_GRANTS: usize = 64;
/// At most this many tracked pids in all (roots included).
pub const MAX_TRACKED: usize = 65_536;

/// What kind of process MonoCode registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessKind {
    Harness,
    CuaRuntime,
}

/// Why a registration or grant was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackError {
    /// The roots or grants table is full.
    Full,
}

/// Who an event belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribution {
    pub chat_id: String,
    /// The bundle id, when the process is followed because of a grant.
    pub grant: Option<String>,
    /// The acting pid is a registered root, not a descendant.
    pub root: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    attribution: Attribution,
    root: bool,
    /// Insertion order, for evicting the oldest descendant when full.
    seq: u64,
}

#[derive(Clone, Debug)]
struct Grant {
    chat_id: String,
    bundle_id: String,
    /// `<appPath>/Contents/`, or `None` (such a grant records nothing).
    contents_prefix: Option<String>,
}

/// The tracker. Not thread-safe by itself; the daemon wraps it in a mutex.
#[derive(Debug, Default)]
pub struct Tracker {
    tracked: HashMap<u32, Entry>,
    /// Non-root entries by insertion order.
    order: BTreeMap<u64, u32>,
    roots: usize,
    grants: Vec<Grant>,
    next_seq: u64,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// Registered roots currently tracked.
    pub fn root_count(&self) -> usize {
        self.roots
    }

    /// All tracked pids, roots and descendants.
    pub fn tracked_count(&self) -> usize {
        self.tracked.len()
    }

    /// Open grants.
    pub fn grant_count(&self) -> usize {
        self.grants.len()
    }

    fn remove(&mut self, pid: u32) -> Option<Entry> {
        let entry = self.tracked.remove(&pid)?;
        if entry.root {
            self.roots = self.roots.saturating_sub(1);
        } else {
            self.order.remove(&entry.seq);
        }
        Some(entry)
    }

    fn insert(&mut self, pid: u32, attribution: Attribution, root: bool) {
        self.remove(pid);
        if self.tracked.len() >= MAX_TRACKED {
            // Evict the oldest descendant; its exit was likely dropped.
            if let Some((_, old)) = self.order.pop_first() {
                self.tracked.remove(&old);
            } else {
                return;
            }
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        if root {
            self.roots += 1;
        } else {
            self.order.insert(seq, pid);
        }
        self.tracked.insert(
            pid,
            Entry {
                attribution,
                root,
                seq,
            },
        );
    }

    /// Tracks `pid` as a root for `chat_id`. A pid already tracked (PID
    /// reuse, or a second registration) is replaced.
    pub fn register(
        &mut self,
        pid: u32,
        chat_id: &str,
        _kind: ProcessKind,
        _at: i64,
    ) -> Result<(), TrackError> {
        let already_root = self.tracked.get(&pid).is_some_and(|e| e.root);
        if !already_root && self.roots >= MAX_ROOTS {
            return Err(TrackError::Full);
        }
        let attribution = Attribution {
            chat_id: chat_id.to_owned(),
            grant: None,
            root: true,
        };
        self.insert(pid, attribution, true);
        Ok(())
    }

    /// Opens (or replaces) the grant of `bundle_id` to `chat_id`.
    pub fn grant(
        &mut self,
        chat_id: &str,
        bundle_id: &str,
        app_path: Option<&str>,
        _at: i64,
    ) -> Result<(), TrackError> {
        let contents_prefix = app_path.map(|p| format!("{}/Contents/", p.trim_end_matches('/')));
        if let Some(existing) = self
            .grants
            .iter_mut()
            .find(|g| g.chat_id == chat_id && g.bundle_id == bundle_id)
        {
            existing.contents_prefix = contents_prefix;
            return Ok(());
        }
        if self.grants.len() >= MAX_GRANTS {
            return Err(TrackError::Full);
        }
        self.grants.push(Grant {
            chat_id: chat_id.to_owned(),
            bundle_id: bundle_id.to_owned(),
            contents_prefix,
        });
        Ok(())
    }

    /// Closes a grant. Processes followed only because of it stop being
    /// followed: after the grant the app is the user's again.
    pub fn grant_ended(&mut self, chat_id: &str, bundle_id: &str, _at: i64) {
        self.grants
            .retain(|g| !(g.chat_id == chat_id && g.bundle_id == bundle_id));
        let gone: Vec<u32> = self
            .tracked
            .iter()
            .filter(|(_, e)| {
                e.attribution.chat_id == chat_id
                    && e.attribution.grant.as_deref() == Some(bundle_id)
            })
            .map(|(pid, _)| *pid)
            .collect();
        for pid in gone {
            self.remove(pid);
        }
    }

    fn grant_for_exe(&self, exe: &str) -> Option<Attribution> {
        self.grants.iter().find_map(|g| {
            let prefix = g.contents_prefix.as_deref()?;
            exe.starts_with(prefix).then(|| Attribution {
                chat_id: g.chat_id.clone(),
                grant: Some(g.bundle_id.clone()),
                root: false,
            })
        })
    }

    /// Updates the tracker with one event and says whom it belongs to, or
    /// `None` when it is not followed.
    pub fn observe(&mut self, ev: &RawEvent) -> Option<Attribution> {
        let mut attribution = self.tracked.get(&ev.pid).map(|e| Attribution {
            root: e.root,
            ..e.attribution.clone()
        });
        if attribution.is_none() {
            if let Some(exe) = ev.exe.as_deref() {
                if let Some(found) = self.grant_for_exe(exe) {
                    if ev.raw_kind() != RawKind::Exit {
                        self.insert(ev.pid, found.clone(), false);
                    }
                    attribution = Some(found);
                }
            }
        }
        let attribution = attribution?;
        match ev.raw_kind() {
            RawKind::Fork => {
                if let Some(child) = ev.child_pid {
                    if child != ev.pid {
                        let inherited = Attribution {
                            root: false,
                            ..attribution.clone()
                        };
                        self.insert(child, inherited, false);
                    }
                }
            }
            RawKind::Exit => {
                self.remove(ev.pid);
            }
            _ => {}
        }
        Some(attribution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: RawKind, pid: u32) -> RawEvent {
        RawEvent {
            kind: Some(kind),
            pid,
            ..RawEvent::default()
        }
    }

    fn fork(pid: u32, child: u32) -> RawEvent {
        RawEvent {
            child_pid: Some(child),
            ..ev(RawKind::Fork, pid)
        }
    }

    fn exec(pid: u32, exe: &str) -> RawEvent {
        RawEvent {
            exe: Some(exe.into()),
            ..ev(RawKind::Exec, pid)
        }
    }

    fn chat(a: Option<Attribution>) -> Option<String> {
        a.map(|a| a.chat_id)
    }

    #[test]
    fn follows_descendants_until_exit() {
        let mut t = Tracker::new();
        t.register(100, "c1", ProcessKind::Harness, 0).unwrap();
        assert_eq!(chat(t.observe(&fork(100, 101))), Some("c1".into()));
        assert_eq!(
            chat(t.observe(&exec(101, "/usr/bin/git"))),
            Some("c1".into())
        );
        assert_eq!(chat(t.observe(&fork(101, 102))), Some("c1".into()));
        assert_eq!(
            chat(t.observe(&ev(RawKind::Unlink, 102))),
            Some("c1".into())
        );
        assert_eq!(chat(t.observe(&ev(RawKind::Exit, 102))), Some("c1".into()));
        assert_eq!(t.observe(&ev(RawKind::Unlink, 102)), None);
        assert_eq!(t.tracked_count(), 2);
        t.observe(&ev(RawKind::Exit, 100));
        assert_eq!(t.root_count(), 0);
        // The child outlives the root and stays attributed.
        assert_eq!(chat(t.observe(&ev(RawKind::Open, 101))), Some("c1".into()));
    }

    #[test]
    fn untracked_is_dropped() {
        let mut t = Tracker::new();
        t.register(100, "c1", ProcessKind::Harness, 0).unwrap();
        assert_eq!(t.observe(&fork(200, 201)), None);
        assert_eq!(t.observe(&ev(RawKind::Unlink, 201)), None);
        assert_eq!(t.tracked_count(), 1);
    }

    #[test]
    fn register_replaces_and_pid_reuse() {
        let mut t = Tracker::new();
        t.register(100, "c1", ProcessKind::Harness, 0).unwrap();
        t.register(100, "c2", ProcessKind::CuaRuntime, 0).unwrap();
        assert_eq!(t.root_count(), 1);
        assert_eq!(chat(t.observe(&ev(RawKind::Open, 100))), Some("c2".into()));
        t.observe(&fork(100, 101));
        t.observe(&ev(RawKind::Exit, 101));
        // A new process that reuses 101 is not tracked.
        assert_eq!(t.observe(&ev(RawKind::Open, 101)), None);
        // A descendant pid re-registered becomes a root.
        t.observe(&fork(100, 102));
        t.register(102, "c3", ProcessKind::Harness, 0).unwrap();
        assert_eq!(t.root_count(), 2);
        assert_eq!(t.tracked_count(), 2);
    }

    #[test]
    fn grant_attributes_app_and_descendants() {
        let mut t = Tracker::new();
        let app = "/System/Applications/Calculator.app";
        t.grant("c1", "com.apple.calculator", Some(app), 0).unwrap();
        let calc = format!("{app}/Contents/MacOS/Calculator");
        let a = t.observe(&exec(300, &calc)).unwrap();
        assert_eq!(a.grant.as_deref(), Some("com.apple.calculator"));
        assert_eq!(a.chat_id, "c1");
        let child = t.observe(&fork(300, 301)).unwrap();
        assert_eq!(child.grant.as_deref(), Some("com.apple.calculator"));
        // A look-alike outside Contents is not attributed.
        assert_eq!(
            t.observe(&exec(400, "/System/Applications/Calculator.app.evil/x")),
            None
        );
        t.grant_ended("c1", "com.apple.calculator", 1);
        assert_eq!(t.observe(&ev(RawKind::Open, 300)), None);
        assert_eq!(t.observe(&exec(500, &calc)), None);
        assert_eq!(t.grant_count(), 0);
    }

    #[test]
    fn grant_without_app_path_records_nothing() {
        let mut t = Tracker::new();
        t.grant("c1", "com.x", None, 0).unwrap();
        assert_eq!(
            t.observe(&exec(1, "/Applications/X.app/Contents/MacOS/X")),
            None
        );
    }

    #[test]
    fn bounded_tables() {
        let mut t = Tracker::new();
        for pid in 0..MAX_ROOTS as u32 {
            t.register(pid, "c", ProcessKind::Harness, 0).unwrap();
        }
        assert_eq!(
            t.register(999_999, "c", ProcessKind::Harness, 0),
            Err(TrackError::Full)
        );
        // Re-registering a tracked root is still allowed.
        t.register(5, "d", ProcessKind::Harness, 0).unwrap();
        for i in 0..MAX_GRANTS {
            t.grant("c", &format!("b{i}"), None, 0).unwrap();
        }
        assert_eq!(t.grant("c", "more", None, 0), Err(TrackError::Full));
        t.grant("c", "b0", Some("/A.app"), 0).unwrap();
        // Descendants: fill past the cap; the oldest is evicted, roots stay.
        let base = 1_000_000u32;
        let extra = (MAX_TRACKED - MAX_ROOTS + 10) as u32;
        for i in 0..extra {
            t.observe(&fork(0, base + i));
        }
        assert_eq!(t.tracked_count(), MAX_TRACKED);
        assert_eq!(t.root_count(), MAX_ROOTS);
        assert_eq!(t.observe(&ev(RawKind::Open, base)), None);
        assert!(t.observe(&ev(RawKind::Open, base + extra - 1)).is_some());
    }
}

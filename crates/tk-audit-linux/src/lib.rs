//! The Linux OS feed (lane D, T1.3 Linux half): `auditd` records of a
//! harness's process tree, turned into the daemon's events.
//!
//! - `rules`: the audit rules file for one user (printed; the root install
//!   is shown and confirmed in the box test).
//! - `record`, `assemble`, `action`: audit lines to records, records to
//!   events, events to actions.
//! - `tree`: which chat an action belongs to, by process tree, from the
//!   `run.start` records `tk run` posts.
//! - `checks`, `sensitive`: the feed's preflags (writes outside the chat's
//!   folders, secrets, files that run later, new hosts, denials).
//! - `map`: actions to `tk_core::event::Event` with `Source::Auditd`, in the
//!   macOS collector's kind and detail vocabulary so the daemon's checks
//!   read both feeds alike.
//! - `client`: the daemon socket client, shared with `tk run`.
//! - `feed`: the pure loop; `src/bin/tk-audit-feed.rs` is the I/O.
//!
//! Sources and what was run are in `plans/parallel/tk_lane_D.md`.

pub mod action;
pub mod assemble;
pub mod checks;
pub mod client;
pub mod feed;
pub mod map;
pub mod record;
pub mod redact;
pub mod rules;
pub mod sensitive;
pub mod syscalls;
pub mod tree;

use tk_core::event::Event;

/// Parses a batch of audit text for one known root and returns its events,
/// as `tk-audit-feed parse` prints them. A convenience over `feed::Feed`.
pub fn parse_for_root(audit_text: &str, root: tree::Root, home: &str) -> Vec<Event> {
    let mut f = feed::Feed::new(home, None);
    let mut out = f.register(root);
    for line in audit_text.split('\n') {
        out.extend(f.push_line(line));
    }
    out.extend(f.flush());
    out
}

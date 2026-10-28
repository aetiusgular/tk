//! Lane K's seam in the daemon (wave 2, scaffolded by integration w1).
//!
//! `main.rs` calls `start` once the daemon is up and before it serves the
//! socket. Lane K fills it inside this folder: the macOS activity
//! collector's registration with the daemon (T1.3, Mac half) and, behind the
//! `computer-use` feature, the computer-use bridge (T1.5). Until then it
//! does nothing, so the daemon behaves as lane B built it.

use std::sync::Arc;

use crate::daemon::{Daemon, Paths};

/// Starts what lane K adds to a running daemon. Must return at once: run
/// anything long-lived on its own thread. Must make no outbound connection
/// (rules, section 5, item 8).
pub fn start(daemon: &Arc<Daemon>, paths: &Paths) {
    let _ = (daemon, paths);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_scaffold_does_nothing_yet() {
        // Lane K replaces this test with its own.
        let _ = super::start;
    }
}

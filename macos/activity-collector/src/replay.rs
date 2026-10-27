//! `parse-fixture`: what the daemon would log for a file of eslogger JSON
//! lines, given one registered pid (and optionally grants). Used by tests
//! and by the Mac test with a real capture.

use std::io::{self, BufRead};

use crate::daemon::{self, MAX_ESLOGGER_LINE};
use crate::event::ActivityEvent;
use crate::lines::{self, Line};
use crate::tree::{ProcessKind, Tracker};

/// What `replay` starts from.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// `(pid, chat id)` registered as harness roots before the first line.
    pub roots: Vec<(u32, String)>,
    /// `(chat id, bundle id, app path)` grants open before the first line.
    pub grants: Vec<(String, String, String)>,
}

/// The events the daemon would log for these lines, in order, without ids.
/// Lines over 1 MiB and malformed lines are skipped, as in the daemon.
pub fn replay<R: BufRead>(mut reader: R, setup: &Setup) -> io::Result<Vec<ActivityEvent>> {
    let mut tracker = Tracker::new();
    for (pid, chat) in &setup.roots {
        tracker
            .register(*pid, chat, ProcessKind::Harness, 0)
            .map_err(|_| io::Error::other("too many roots"))?;
    }
    for (chat, bundle, app) in &setup.grants {
        tracker
            .grant(chat, bundle, Some(app), 0)
            .map_err(|_| io::Error::other("too many grants"))?;
    }
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        match lines::read_capped(&mut reader, &mut buf, MAX_ESLOGGER_LINE)? {
            Line::Eof => break,
            Line::TooLong => continue,
            Line::Complete | Line::Cut => {
                let Ok(text) = std::str::from_utf8(&buf) else {
                    continue;
                };
                if let Some(ev) = daemon::process_line(&mut tracker, text) {
                    out.push(ev);
                }
            }
        }
    }
    Ok(out)
}

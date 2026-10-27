//! The Linux OS feed (lane D, T1.3 Linux half). STUB. Lane D owns this
//! crate: an `auditd` rules file keyed on a harness's PID tree, a reader
//! that turns audit records into `tk_core::event::Event` with
//! `Source::Auditd`, and the checks the macOS collector feeds.

use tk_core::event::Event;

/// Parses one batch of audit text into events. STUB: returns nothing.
pub fn parse(_audit_text: &str) -> Vec<Event> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_parses_to_nothing() {
        assert!(super::parse("type=SYSCALL msg=audit(1.0:1): arch=c000003e").is_empty());
    }
}

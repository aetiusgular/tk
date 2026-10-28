//! Records to events. A syscall event is several records with one
//! `(timestamp, serial)`: SYSCALL, then EXECVE, CWD, PATH (one per item),
//! SOCKADDR, and PROCTITLE last. auditd's ENRICHED log on Ubuntu 24.04 writes
//! no EOE (seen in `tests/fixtures/scenario1.raw.audit`); a raw kernel stream
//! does. So an event ends at PROCTITLE or EOE, or when it is the oldest of
//! more than `MAX_OPEN` open events, or at `flush`.

use crate::record::Record;

/// Open events kept while waiting for their last record.
pub const MAX_OPEN: usize = 16;

/// All records of one event, in the order they arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub ts_ms: i64,
    pub serial: u64,
    pub records: Vec<Record>,
}

impl AuditEvent {
    pub fn first(&self, kind: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.kind == kind)
    }

    pub fn all<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Record> + 'a {
        self.records.iter().filter(move |r| r.kind == kind)
    }
}

#[derive(Default)]
pub struct Assembler {
    open: Vec<AuditEvent>,
}

impl Assembler {
    pub fn new() -> Assembler {
        Assembler::default()
    }

    /// Adds a record; returns the events it completed, oldest first.
    pub fn push(&mut self, record: Record) -> Vec<AuditEvent> {
        let key = record.event_key();
        let ends = record.kind == "PROCTITLE" || record.kind == "EOE";
        let pos = self.open.iter().position(|e| (e.ts_ms, e.serial) == key);
        let idx = match pos {
            Some(i) => i,
            None => {
                self.open.push(AuditEvent {
                    ts_ms: key.0,
                    serial: key.1,
                    records: Vec::new(),
                });
                self.open.len() - 1
            }
        };
        if record.kind != "EOE" {
            self.open[idx].records.push(record);
        }
        let mut done = Vec::new();
        if ends {
            done.push(self.open.remove(idx));
        }
        while self.open.len() > MAX_OPEN {
            done.push(self.open.remove(0));
        }
        done.retain(|e| !e.records.is_empty());
        done
    }

    /// Ends every open event, oldest first.
    pub fn flush(&mut self) -> Vec<AuditEvent> {
        let mut done: Vec<AuditEvent> = std::mem::take(&mut self.open);
        done.retain(|e| !e.records.is_empty());
        done
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::parse_line;

    fn rec(s: &str) -> Record {
        parse_line(s).unwrap()
    }

    #[test]
    fn proctitle_ends_an_event() {
        let mut a = Assembler::new();
        assert!(a
            .push(rec("type=SYSCALL msg=audit(1.000:1): syscall=59"))
            .is_empty());
        assert!(a
            .push(rec("type=CWD msg=audit(1.000:1): cwd=\"/\""))
            .is_empty());
        let done = a.push(rec("type=PROCTITLE msg=audit(1.000:1): proctitle=6C73"));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].records.len(), 3);
        assert_eq!(a.open_count(), 0);
    }

    #[test]
    fn interleaved_events_stay_apart_and_eoe_ends_one() {
        let mut a = Assembler::new();
        a.push(rec("type=SYSCALL msg=audit(1.000:1): syscall=59"));
        a.push(rec("type=SYSCALL msg=audit(1.000:2): syscall=42"));
        let done = a.push(rec("type=EOE msg=audit(1.000:1): "));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].serial, 1);
        let rest = a.flush();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].serial, 2);
    }

    #[test]
    fn too_many_open_events_push_out_the_oldest() {
        let mut a = Assembler::new();
        let mut out = Vec::new();
        for n in 0..(MAX_OPEN as u64 + 3) {
            out.extend(a.push(rec(&format!(
                "type=SYSCALL msg=audit(1.000:{n}): syscall=59"
            ))));
        }
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].serial, 0);
        assert_eq!(a.open_count(), MAX_OPEN);
    }
}

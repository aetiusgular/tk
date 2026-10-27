//! The app's side of the activity log: a daily JSONL file per UTC day, each
//! line chained by hash (`chain.rs`), in the app's data folder.
//!
//! What it holds: approvals (B1), lane A's telemetry and adapter events, lane
//! C's computer-use records. The kernel events live in the collector's own
//! root-owned log and are read over its socket (`collector.rs`).
//!
//! Writes go through a bounded queue to one writer thread, so a caller never
//! waits on the disk and a full queue drops (and counts) rather than blocks
//! (rules, section 4, case 9). Every record is bounded before it is written.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::thread;

use super::chain::{self, GENESIS};
use super::retention::{self, LOG_PREFIX, LOG_SUFFIX};
use super::types::{ActivityEvent, ApprovalRecord};

pub const QUEUE_CAPACITY: usize = 4_096;
pub const MAX_ID: usize = 128;
pub const MAX_KIND: usize = 64;
pub const MAX_SUMMARY: usize = 500;
pub const MAX_TARGET: usize = 1_024;
pub const MAX_DETAIL_BYTES: usize = 16 * 1024;
const KEYS: &[&str] = &["event", "approval"];

/// One thing to write.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Event(ActivityEvent),
    Approval(ApprovalRecord),
}

fn cut(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Ids and chat ids keep to a safe alphabet, so a record cannot carry a line
/// break or markup into anything that shows it.
fn clean_id(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
        .take(MAX_ID)
        .collect()
}

fn one_line(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cut(&flat, max_chars)
}

/// The bounded form of an event, as it is written.
pub fn bound_event(mut event: ActivityEvent) -> ActivityEvent {
    event.id = clean_id(&event.id);
    event.chat_id = clean_id(&event.chat_id);
    event.harness = clean_id(&event.harness);
    event.kind = clean_id(&event.kind).chars().take(MAX_KIND).collect();
    event.summary = one_line(&event.summary, MAX_SUMMARY);
    event.approval_id = event.approval_id.map(|id| clean_id(&id));
    let size = serde_json::to_string(&event.detail)
        .map(|s| s.len())
        .unwrap_or(usize::MAX);
    if size > MAX_DETAIL_BYTES {
        event.detail = serde_json::json!({ "truncated": true, "bytes": size });
    } else if !event.detail.is_object() {
        event.detail = serde_json::json!({ "value": event.detail });
    }
    event
}

/// The bounded form of an approval record.
pub fn bound_approval(mut record: ApprovalRecord) -> ApprovalRecord {
    record.id = clean_id(&record.id);
    record.chat_id = clean_id(&record.chat_id);
    record.harness = clean_id(&record.harness);
    record.target = one_line(&record.target, MAX_TARGET);
    record
}

/// Keeps a sequence of lines chained across days and restarts.
pub struct ChainWriter {
    dir: PathBuf,
    day: Option<i64>,
    seq: u64,
    prev: String,
    file: Option<File>,
}

impl ChainWriter {
    pub fn new(dir: PathBuf) -> Self {
        let (seq, prev) = resume_point(&dir);
        ChainWriter {
            dir,
            day: None,
            seq,
            prev,
            file: None,
        }
    }

    fn path_for(&self, day: i64) -> PathBuf {
        self.dir.join(format!(
            "{LOG_PREFIX}{}{LOG_SUFFIX}",
            retention::day_name(day)
        ))
    }

    fn open(&mut self, day: i64) -> std::io::Result<()> {
        if self.day == Some(day) && self.file.is_some() {
            return Ok(());
        }
        create_private_dir(&self.dir)?;
        let path = self.path_for(day);
        // Never write through a symlink someone left in our folder.
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(std::io::Error::other("log path is not a plain file"));
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        self.file = Some(options.open(path)?);
        self.day = Some(day);
        Ok(())
    }

    /// Writes one entry on the day of `now_ms`.
    pub fn write(&mut self, entry: &Entry, now_ms: i64) -> std::io::Result<()> {
        self.open(retention::day_of_ms(now_ms))?;
        let (key, value) = match entry {
            Entry::Event(event) => ("event", serde_json::to_value(event)),
            Entry::Approval(record) => ("approval", serde_json::to_value(record)),
        };
        let value = value.map_err(std::io::Error::other)?;
        let seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("seq"))?;
        let (line, hash) = chain::line(seq, &self.prev, key, &value);
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| std::io::Error::other("closed"))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.seq = seq;
        self.prev = hash;
        Ok(())
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Log files by day, oldest first.
fn day_files(dir: &Path) -> Vec<(i64, PathBuf)> {
    let mut days: Vec<(i64, PathBuf)> = retention::scan(dir)
        .into_iter()
        .filter(|item| item.kind == retention::ItemKind::Log)
        .map(|item| (item.day, item.path))
        .collect();
    days.sort();
    days
}

/// The last good link on disk, so a restart continues the chain.
fn resume_point(dir: &Path) -> (u64, String) {
    for (_, path) in day_files(dir).into_iter().rev() {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let mut last = None;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Some(link) = chain::parse_line(&line, KEYS) {
                last = Some((link.seq, link.hash));
            }
        }
        if let Some(point) = last {
            return point;
        }
    }
    (0, GENESIS.to_string())
}

/// What a read returns.
#[derive(Debug, Default)]
pub struct ReadBack {
    pub events: Vec<ActivityEvent>,
    /// The latest record for each approval id.
    pub approvals: Vec<ApprovalRecord>,
    pub chain_ok: bool,
    pub truncated: bool,
}

/// Reads the records between two times. Approvals are read from every day
/// kept, since one granted last week can still hold.
pub fn read(
    dir: &Path,
    since_ms: i64,
    until_ms: i64,
    chat_id: Option<&str>,
    limit: usize,
) -> ReadBack {
    read_where(dir, since_ms, until_ms, chat_id, limit, &|_| true)
}

/// Whether a record written later under an event's id may take its place.
/// Only telemetry replaces telemetry, and a source replaces its own: a
/// webview write can never wipe a computer-use or collector record by
/// reusing its id (review V1).
fn may_replace(old: &ActivityEvent, new: &ActivityEvent) -> bool {
    use super::types::ActivitySource::{Adapter, Otel};
    old.source == new.source
        || (matches!(old.source, Adapter | Otel) && matches!(new.source, Adapter | Otel))
}

/// `read`, keeping only the new events `keep` accepts, so the cap counts
/// only those (review V5).
fn read_where(
    dir: &Path,
    since_ms: i64,
    until_ms: i64,
    chat_id: Option<&str>,
    limit: usize,
    keep: &dyn Fn(&ActivityEvent) -> bool,
) -> ReadBack {
    let mut back = ReadBack {
        chain_ok: true,
        ..ReadBack::default()
    };
    let mut approvals: HashMap<String, ApprovalRecord> = HashMap::new();
    // An event written again under its id (a telemetry call that an OTel
    // detail joined late, lane A) replaces the earlier copy: last write wins.
    let mut event_at: HashMap<String, usize> = HashMap::new();
    let first_day = retention::day_of_ms(since_ms);
    let last_day = retention::day_of_ms(until_ms);
    let mut prev_hash: Option<String> = None;
    for (day, path) in day_files(dir) {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let lines: Vec<String> = BufReader::new(file).lines().map_while(Result::ok).collect();
        let report = chain::verify(lines.iter().map(String::as_str), KEYS, prev_hash.as_deref());
        // A cut last line is what a killed app leaves; the rest still counts.
        if !report.ok && !(report.cut && report.first_bad_line == Some(report.lines)) {
            back.chain_ok = false;
        }
        for line in &lines {
            let Some(link) = chain::parse_line(line, KEYS) else {
                continue;
            };
            prev_hash = Some(link.hash.clone());
            match link.key.as_str() {
                "approval" => {
                    if let Ok(record) = serde_json::from_value::<ApprovalRecord>(link.record) {
                        if chat_id.is_none_or(|c| c == record.chat_id) {
                            approvals.insert(record.id.clone(), record);
                        }
                    }
                }
                _ if day >= first_day && day <= last_day => {
                    if let Ok(event) = serde_json::from_value::<ActivityEvent>(link.record) {
                        let in_range = event.at >= since_ms && event.at <= until_ms;
                        if in_range && chat_id.is_none_or(|c| c == event.chat_id) {
                            if let Some(&slot) = event_at.get(&event.id) {
                                if may_replace(&back.events[slot], &event) {
                                    back.events[slot] = event;
                                }
                            } else if !keep(&event) {
                                // Not wanted by this read; not counted.
                            } else if back.events.len() >= limit {
                                back.truncated = true;
                            } else {
                                event_at.insert(event.id.clone(), back.events.len());
                                back.events.push(event);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    back.events.sort_by_key(|event| event.at);
    let mut list: Vec<ApprovalRecord> = approvals.into_values().collect();
    list.sort_by_key(|record| record.granted_at);
    back.approvals = list;
    back
}

/// What the check needs from the 7 days before `before_ms`, read in one
/// pass (review V5).
pub struct Lookback {
    /// Hosts seen before, for the "new host" check. Only from MonoCode's
    /// own adapter events: a host a harness's telemetry named can raise a
    /// flag, never make a host known (security review T5).
    pub hosts: std::collections::HashSet<String>,
    /// Computer-use runtime starts and ends, for the chat asked about. A
    /// runtime lives across turns, so its start usually sits before the
    /// window (review U1), and its end may too (review V3).
    pub runtimes: Vec<ActivityEvent>,
    pub truncated: bool,
}

fn is_runtime_record(e: &ActivityEvent) -> bool {
    e.source == super::types::ActivitySource::Cua
        && (e.kind == "cua.runtime" || e.kind == "cua.runtime.stop")
}

pub fn lookback(dir: &Path, before_ms: i64, chat_id: Option<&str>) -> Lookback {
    use super::types::ActivitySource;
    let earliest = before_ms.saturating_sub(retention::KEEP_LOG_DAYS * 86_400_000);
    let keep = |e: &ActivityEvent| {
        (e.source == ActivitySource::Adapter && e.detail.get("host").is_some())
            || is_runtime_record(e)
    };
    let back = read_where(
        dir,
        earliest,
        before_ms.saturating_sub(1),
        None,
        200_000,
        &keep,
    );
    let hosts = known_hosts(&back.events);
    let runtimes = back
        .events
        .into_iter()
        .filter(|e| is_runtime_record(e) && chat_id.is_none_or(|c| c == e.chat_id))
        .collect();
    Lookback {
        hosts,
        runtimes,
        truncated: back.truncated,
    }
}

pub fn known_hosts(events: &[ActivityEvent]) -> std::collections::HashSet<String> {
    use super::types::ActivitySource;
    // Adapter only: collector events are never stored in this log, so an
    // Eslogger row here was not written by the collector (review U3).
    events
        .iter()
        .filter(|e| e.source == ActivitySource::Adapter)
        .filter_map(|e| e.detail.get("host").and_then(serde_json::Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The log's front door: callers hand records over and return at once.
pub struct ActivityLog {
    tx: SyncSender<Entry>,
    dir: PathBuf,
    dropped: AtomicU64,
    /// Serialises reads with retention passes.
    pub read_lock: Mutex<()>,
}

impl ActivityLog {
    /// Starts the writer thread for `dir`.
    pub fn start(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Entry>(QUEUE_CAPACITY);
        let writer_dir = dir.clone();
        let spawned = thread::Builder::new()
            .name("activity-log".into())
            .spawn(move || writer_loop(writer_dir, rx));
        if spawned.is_err() {
            eprintln!("activity log: writer thread did not start");
        }
        ActivityLog {
            tx,
            dir,
            dropped: AtomicU64::new(0),
            read_lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Queues an entry. Never blocks and never fails the caller.
    pub fn submit(&self, entry: Entry) {
        let entry = match entry {
            Entry::Event(event) => Entry::Event(bound_event(event)),
            Entry::Approval(record) => Entry::Approval(bound_approval(record)),
        };
        match self.tx.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn writer_loop(dir: PathBuf, rx: Receiver<Entry>) {
    let mut writer = ChainWriter::new(dir.clone());
    let mut last_retention_day: Option<i64> = None;
    let mut failures: u32 = 0;
    for entry in rx {
        let now = now_ms();
        let today = retention::day_of_ms(now);
        if last_retention_day != Some(today) {
            last_retention_day = Some(today);
            let items = retention::scan(&dir);
            retention::apply(&retention::plan(&items, today, retention::CAP_BYTES));
        }
        if writer.write(&entry, now).is_err() {
            failures = failures.saturating_add(1);
            if failures == 1 || failures.is_power_of_two() {
                eprintln!("activity log: write failed ({failures} so far)");
            }
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::types::{ActivitySource, ApprovalScope, ApprovalSubject};
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("activity-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn event(id: &str, chat: &str, at: i64) -> ActivityEvent {
        ActivityEvent {
            id: id.into(),
            at,
            chat_id: chat.into(),
            harness: "codex".into(),
            source: ActivitySource::Adapter,
            kind: "tool.call".into(),
            summary: "ran ls".into(),
            detail: json!({ "callId": "c1" }),
            pid: None,
            approval_id: None,
        }
    }

    fn approval(id: &str, chat: &str, at: i64, ended: Option<i64>) -> ApprovalRecord {
        ApprovalRecord {
            id: id.into(),
            chat_id: chat.into(),
            harness: "codex".into(),
            subject: ApprovalSubject::Command,
            target: "npm test".into(),
            scope: ApprovalScope::Chat,
            granted: true,
            granted_at: at,
            ended_at: ended,
        }
    }

    const DAY: i64 = 86_400_000;

    #[test]
    fn writes_across_days_chain_and_read_back() {
        let dir = temp_dir("days");
        let mut writer = ChainWriter::new(dir.clone());
        let day1 = 20_377 * DAY;
        writer
            .write(&Entry::Approval(approval("a1", "c1", day1, None)), day1)
            .unwrap();
        writer
            .write(&Entry::Event(event("e1", "c1", day1 + 5)), day1 + 5)
            .unwrap();
        writer
            .write(&Entry::Event(event("e2", "c2", day1 + DAY)), day1 + DAY)
            .unwrap();
        writer
            .write(
                &Entry::Approval(approval("a1", "c1", day1, Some(day1 + DAY))),
                day1 + DAY,
            )
            .unwrap();

        let back = read(&dir, day1, day1 + 2 * DAY, None, 100);
        assert!(back.chain_ok);
        assert_eq!(back.events.len(), 2);
        assert_eq!(back.approvals.len(), 1, "the later record for a1 wins");
        assert_eq!(back.approvals[0].ended_at, Some(day1 + DAY));

        let only_c1 = read(&dir, day1, day1 + 2 * DAY, Some("c1"), 100);
        assert_eq!(only_c1.events.len(), 1);

        // A restart continues the chain rather than starting a new one.
        let mut again = ChainWriter::new(dir.clone());
        again
            .write(
                &Entry::Event(event("e3", "c1", day1 + DAY + 1)),
                day1 + DAY + 1,
            )
            .unwrap();
        assert!(read(&dir, day1, day1 + 2 * DAY, None, 100).chain_ok);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_event_written_again_under_its_id_counts_once_with_the_last_copy() {
        // Integration (wave 3): lane A writes a joined call again when its
        // OTel detail arrives late.
        let dir = temp_dir("same-id");
        let mut writer = ChainWriter::new(dir.clone());
        let day = 20_377 * DAY;
        writer
            .write(&Entry::Event(event("t1", "c1", day + 5)), day + 5)
            .unwrap();
        writer
            .write(&Entry::Event(event("t2", "c1", day + 6)), day + 6)
            .unwrap();
        let mut late = event("t1", "c1", day + 5);
        late.detail = json!({ "callId": "c1", "otel": { "event": "tool_result" } });
        writer.write(&Entry::Event(late), day + 9).unwrap();

        let back = read(&dir, day, day + DAY - 1, None, 100);
        assert_eq!(back.events.len(), 2);
        let t1 = back.events.iter().find(|e| e.id == "t1").unwrap();
        assert_eq!(t1.detail["otel"]["event"], "tool_result");
        // The replacement does not count against the limit.
        let capped = read(&dir, day, day + DAY - 1, None, 2);
        assert_eq!(capped.events.len(), 2);
        assert!(!capped.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn telemetry_hosts_never_become_known() {
        // Security review T5.
        let mut adapter = event("a", "c1", 1);
        adapter.detail = json!({ "host": "Docs.rs" });
        let mut otel = event("o", "c1", 2);
        otel.source = ActivitySource::Otel;
        otel.detail = json!({ "host": "forged.test" });
        let mut joined = event("j", "c1", 3);
        joined.detail = json!({ "otel": { "host": "joined.test" } });
        let hosts = known_hosts(&[adapter, otel, joined]);
        assert_eq!(
            hosts,
            std::collections::HashSet::from(["docs.rs".to_string()])
        );
    }

    #[test]
    fn an_edited_file_reads_as_broken_and_a_cut_tail_does_not() {
        let dir = temp_dir("edit");
        let mut writer = ChainWriter::new(dir.clone());
        let day = 20_000 * DAY;
        for i in 0..3 {
            writer
                .write(
                    &Entry::Event(event(&format!("e{i}"), "c1", day + i)),
                    day + i,
                )
                .unwrap();
        }
        drop(writer);
        let path = dir.join("app-2024-10-04.jsonl");
        let text = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{}{{\"seq\":4,\"prev\"", text)).unwrap();
        assert!(
            read(&dir, day, day + DAY, None, 100).chain_ok,
            "a cut tail is not tampering"
        );
        fs::write(&path, text.replace("ran ls", "ran rm")).unwrap();
        assert!(!read(&dir, day, day + DAY, None, 100).chain_ok);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn records_are_bounded_before_they_are_written() {
        let mut long = event("id with spaces\nand a newline<b>", "chat/../x", 1);
        long.summary = "line one\nline two ".repeat(100);
        long.detail = json!({ "blob": "x".repeat(MAX_DETAIL_BYTES + 10) });
        long.kind = "k".repeat(500);
        let bounded = bound_event(long);
        assert_eq!(bounded.id, "idwithspacesandanewlineb");
        assert_eq!(bounded.chat_id, "chat..x");
        assert!(!bounded.summary.contains('\n'));
        assert!(bounded.summary.chars().count() <= MAX_SUMMARY);
        assert_eq!(bounded.detail["truncated"], true);
        assert_eq!(bounded.kind.len(), MAX_KIND);

        let mut scalar = event("e", "c", 1);
        scalar.detail = json!("text");
        assert_eq!(bound_event(scalar).detail, json!({ "value": "text" }));
    }

    #[test]
    fn the_writer_refuses_a_symlinked_day_file() {
        #[cfg(unix)]
        {
            let dir = temp_dir("link");
            fs::create_dir_all(&dir).unwrap();
            let target = dir.join("target.txt");
            fs::write(&target, b"").unwrap();
            std::os::unix::fs::symlink(&target, dir.join("app-2024-10-04.jsonl")).unwrap();
            let mut writer = ChainWriter::new(dir.clone());
            assert!(writer
                .write(&Entry::Event(event("e", "c", 1)), 20_000 * DAY)
                .is_err());
            assert_eq!(fs::read(&target).unwrap().len(), 0);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_full_queue_drops_and_counts_and_never_blocks() {
        let (tx, _rx) = mpsc::sync_channel::<Entry>(1);
        let log = ActivityLog {
            tx,
            dir: temp_dir("full"),
            dropped: AtomicU64::new(0),
            read_lock: Mutex::new(()),
        };
        for i in 0..5 {
            log.submit(Entry::Event(event(&format!("e{i}"), "c", i)));
        }
        assert_eq!(log.dropped(), 4);
    }

    #[test]
    fn the_writer_thread_writes_what_is_submitted() {
        let dir = temp_dir("thread");
        let log = ActivityLog::start(dir.clone());
        let now = now_ms();
        log.submit(Entry::Event(event("live", "c1", now)));
        let mut found = false;
        for _ in 0..100 {
            if read(&dir, now - 1, now + 1, None, 10).events.len() == 1 {
                found = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(found);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_webview_write_cannot_replace_a_computer_use_record() {
        // Review V1.
        let dir = temp_dir("replace-cua");
        let mut writer = ChainWriter::new(dir.clone());
        let day = 20_377 * DAY;
        let mut runtime = event("rt", "c1", day + 5);
        runtime.source = ActivitySource::Cua;
        runtime.kind = "cua.runtime".into();
        runtime.pid = Some(77);
        writer.write(&Entry::Event(runtime), day + 5).unwrap();
        let forged = event("rt", "c1", day + 6); // an Adapter record, same id
        writer.write(&Entry::Event(forged), day + 6).unwrap();
        let back = read(&dir, day, day + DAY - 1, None, 100);
        assert_eq!(back.events.len(), 1);
        assert_eq!(back.events[0].source, ActivitySource::Cua);
        assert_eq!(back.events[0].kind, "cua.runtime");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_lookback_caps_only_what_it_keeps_and_returns_runtime_records() {
        // Review V5 and V3.
        let dir = temp_dir("lookback");
        let mut writer = ChainWriter::new(dir.clone());
        let t = 20_377 * DAY;
        for i in 0..50 {
            writer
                .write(&Entry::Event(event(&format!("n{i}"), "c1", t + i)), t + i)
                .unwrap();
        }
        for (id, kind, chat) in [
            ("s", "cua.runtime", "c1"),
            ("e", "cua.runtime.stop", "c1"),
            ("o", "cua.runtime", "c2"),
        ] {
            let mut r = event(id, chat, t + 60);
            r.source = ActivitySource::Cua;
            r.kind = kind.into();
            r.pid = Some(7);
            writer.write(&Entry::Event(r), t + 60).unwrap();
        }
        let back = lookback(&dir, t + 100, Some("c1"));
        let ids: Vec<&str> = back.runtimes.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["s", "e"]);
        assert!(!back.truncated);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hosts_before_reads_earlier_hosts_only() {
        let dir = temp_dir("hosts");
        let mut writer = ChainWriter::new(dir.clone());
        let t = 20_377 * DAY;
        let mut old = event("h1", "c1", t);
        old.detail = json!({ "host": "Example.com" });
        let mut new = event("h2", "c1", t + 10);
        new.detail = json!({ "host": "other.dev" });
        writer.write(&Entry::Event(old), t).unwrap();
        writer.write(&Entry::Event(new), t + 10).unwrap();
        let hosts = lookback(&dir, t + 5, None).hosts;
        assert!(hosts.contains("example.com"));
        assert!(!hosts.contains("other.dev"));
        let _ = fs::remove_dir_all(&dir);
    }
}

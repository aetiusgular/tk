//! The hash-chained daily JSONL log (PROTOCOL.md, "Log lines").
//!
//! Each line is `{"seq","prev","hash","event"}` where
//! `hash = sha256_hex(prev + "\n" + serde_json::to_string(&to_value(event)))`.
//! The first line of a day takes the previous day's last hash, or 64 zeros.
//! Days are UTC. Files are `es-YYYY-MM-DD.jsonl`, rolled to `.jsonl.gz`.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::event::ActivityEvent;
use crate::lines::{self, Line};
use crate::timefmt;

/// The `prev` of the very first line.
pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// A log line longer than this is treated as damaged.
pub const MAX_LOG_LINE: usize = 1 << 20;

/// One log line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub prev: String,
    pub hash: String,
    pub event: ActivityEvent,
}

/// Lowercase hex SHA-256 of `prev`, `\n`, then the event as canonical JSON
/// (`serde_json::to_string` of the event read as a `serde_json::Value`).
pub fn chain_hash(prev: &str, event: &ActivityEvent) -> String {
    let canonical = serde_json::to_value(event)
        .and_then(|v| serde_json::to_string(&v))
        .unwrap_or_default();
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `es-YYYY-MM-DD.jsonl` or `es-YYYY-MM-DD.jsonl.gz` to `(day, compressed)`.
pub fn parse_log_name(name: &str) -> Option<(i64, bool)> {
    let rest = name.strip_prefix("es-")?;
    let (date, compressed) = if let Some(d) = rest.strip_suffix(".jsonl.gz") {
        (d, true)
    } else {
        (rest.strip_suffix(".jsonl")?, false)
    };
    Some((timefmt::parse_day(date)?, compressed))
}

/// The plain file name for a day.
pub fn log_name(day: i64) -> String {
    format!("es-{}.jsonl", timefmt::day_name(day))
}

/// One log file found in the log folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogFile {
    pub day: i64,
    pub compressed: bool,
    pub bytes: u64,
    pub path: PathBuf,
}

/// The regular (non-symlink) log files in `dir`, sorted by day, plain
/// before compressed on the same day.
pub fn list_logs(dir: &Path) -> io::Result<Vec<LogFile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((day, compressed)) = parse_log_name(&name) else {
            continue;
        };
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if !meta.file_type().is_file() {
            continue;
        }
        out.push(LogFile {
            day,
            compressed,
            bytes: meta.len(),
            path,
        });
    }
    out.sort_by_key(|f| (f.day, f.compressed));
    Ok(out)
}

/// Creates the log folder (0700) if needed, refusing a symlink.
pub fn ensure_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(io::Error::other("log folder is not a plain directory"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir_all(dir)?,
        Err(e) => return Err(e),
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// Opens a log file for reading, decompressing `.gz`; refuses symlinks.
pub fn open_log(path: &Path, compressed: bool) -> io::Result<Box<dyn BufRead>> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::other("log file is a symlink"));
    }
    let file = File::open(path)?;
    Ok(if compressed {
        Box::new(BufReader::new(GzDecoder::new(file)))
    } else {
        Box::new(BufReader::new(file))
    })
}

/// The outcome of checking one file's chain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainReport {
    /// Every line parsed, linked and hashed correctly, and none was cut.
    pub ok: bool,
    /// Lines read, the cut line included.
    pub lines: usize,
    /// 1-based number of the first line that failed (a cut line included).
    pub first_bad_line: Option<usize>,
    /// The last line had no newline: the file was cut.
    pub cut: bool,
    /// `prev` of the first good line, for linking to the day before.
    pub first_prev: Option<String>,
    /// `hash` of the last line that parsed (good, or edited but whole).
    pub last_hash: Option<String>,
    /// `seq` of that line.
    pub last_seq: Option<u64>,
}

/// Checks a single line against the previous hash and seq.
fn check_line(bytes: &[u8], prev: Option<&(String, u64)>) -> Option<LogLine> {
    let line: LogLine = serde_json::from_slice(bytes).ok()?;
    if !is_hash(&line.prev) || !is_hash(&line.hash) {
        return None;
    }
    if let Some((prev_hash, prev_seq)) = prev {
        if &line.prev != prev_hash || Some(line.seq) != prev_seq.checked_add(1) {
            return None;
        }
    }
    (chain_hash(&line.prev, &line.event) == line.hash).then_some(line)
}

/// Verifies one file and hands each good line to `each`.
pub fn verify_with<R: BufRead>(
    mut reader: R,
    mut each: impl FnMut(&LogLine),
) -> io::Result<ChainReport> {
    let mut report = ChainReport::default();
    let mut buf = Vec::new();
    let mut last: Option<(String, u64)> = None;
    loop {
        let state = lines::read_capped(&mut reader, &mut buf, MAX_LOG_LINE)?;
        if state == Line::Eof {
            break;
        }
        report.lines += 1;
        let good = match state {
            Line::Complete => check_line(&buf, last.as_ref()),
            Line::Cut => {
                report.cut = true;
                None
            }
            _ => None,
        };
        match good {
            Some(line) => {
                if report.first_prev.is_none() {
                    report.first_prev = Some(line.prev.clone());
                }
                each(&line);
                last = Some((line.hash.clone(), line.seq));
            }
            None => {
                if report.first_bad_line.is_none() {
                    report.first_bad_line = Some(report.lines);
                }
                // Lines after a bad one are still checked against each other
                // so a reader can see how much is intact, but `ok` is false.
                if let Ok(line) = serde_json::from_slice::<LogLine>(&buf) {
                    last = Some((line.hash.clone(), line.seq));
                }
            }
        }
    }
    if let Some((hash, seq)) = last {
        report.last_hash = Some(hash);
        report.last_seq = Some(seq);
    }
    report.ok = report.first_bad_line.is_none();
    Ok(report)
}

/// Verifies one file's chain.
pub fn verify<R: BufRead>(reader: R) -> io::Result<ChainReport> {
    verify_with(reader, |_| {})
}

/// The answer to an `events` read.
#[derive(Debug, Default)]
pub struct ReadResult {
    pub events: Vec<ActivityEvent>,
    pub truncated: bool,
    pub chain_ok: bool,
}

/// Reads logged events with `since <= at <= until` for `chat_id` (all chats
/// when empty), oldest first, at most `limit`. Past the limit the NEWEST
/// are kept and `truncated` is set: the panel needs what just happened
/// (Mac test 3.12b). Every file whose day falls
/// in the range is verified in full, and consecutive files must link;
/// `chain_ok` says whether all of that held.
pub fn read_events(
    dir: &Path,
    since: i64,
    until: i64,
    chat_id: &str,
    limit: usize,
) -> io::Result<ReadResult> {
    let mut result = ReadResult {
        chain_ok: true,
        ..ReadResult::default()
    };
    let first_day = timefmt::day_of_ms(since);
    let last_day = timefmt::day_of_ms(until);
    let mut files = list_logs(dir)?;
    // A day with both forms (a compress cut short) is read once, plain.
    files.dedup_by_key(|f| f.day);
    let mut prev_last: Option<String> = None;
    let mut kept: VecDeque<ActivityEvent> = VecDeque::new();
    for file in files {
        // A late event lands in the next day's file, so read one day past.
        if file.day > last_day.saturating_add(1) {
            break;
        }
        // Before the range only the day just before it matters, for linking.
        if file.day < first_day.saturating_sub(1) {
            continue;
        }
        let in_range = file.day >= first_day;
        let reader = match open_log(&file.path, file.compressed) {
            Ok(r) => r,
            Err(_) => {
                if in_range {
                    result.chain_ok = false;
                }
                prev_last = None;
                continue;
            }
        };
        if !in_range {
            // Only the last hash of the day before the range is needed.
            prev_last = verify(reader).ok().and_then(|r| r.last_hash);
            continue;
        }
        let events = &mut kept;
        let truncated = &mut result.truncated;
        let report = verify_with(reader, |line| {
            let ev = &line.event;
            if ev.at < since || ev.at > until || (!chat_id.is_empty() && ev.chat_id != chat_id) {
                return;
            }
            if limit == 0 {
                *truncated = true;
                return;
            }
            if events.len() >= limit {
                events.pop_front();
                *truncated = true;
            }
            events.push_back(ev.clone());
        })?;
        if !report.ok {
            result.chain_ok = false;
        }
        if let (Some(before), Some(first)) = (&prev_last, &report.first_prev) {
            if before != first {
                result.chain_ok = false;
            }
        }
        prev_last = report.last_hash;
    }
    result.events = kept.into();
    Ok(result)
}

/// The hash and seq to continue from, found in an existing file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Resume {
    hash: String,
    seq: u64,
    needs_newline: bool,
}

fn resume_from(file: &LogFile) -> io::Result<Option<Resume>> {
    let reader = open_log(&file.path, file.compressed)?;
    // Continue from the last line that parses, so seqs never repeat even
    // after an edited line; the chain still shows the damage.
    let report = verify(reader)?;
    let needs_newline = report.cut && !file.compressed;
    Ok(report
        .last_hash
        .zip(report.last_seq)
        .map(|(hash, seq)| Resume {
            hash,
            seq,
            needs_newline,
        }))
}

/// Appends events to today's file, rolling at UTC midnight.
#[derive(Debug)]
pub struct LogWriter {
    dir: PathBuf,
    day: Option<i64>,
    file: Option<File>,
    seq: u64,
    last_hash: String,
    needs_newline: bool,
    /// Days whose files are finished and may be compressed.
    finished: Vec<i64>,
}

impl LogWriter {
    /// Opens the log folder and resumes the chain from the newest file.
    pub fn open(dir: &Path) -> io::Result<LogWriter> {
        ensure_dir(dir)?;
        let mut writer = LogWriter {
            dir: dir.to_path_buf(),
            day: None,
            file: None,
            seq: 0,
            last_hash: ZERO_HASH.to_owned(),
            needs_newline: false,
            finished: Vec::new(),
        };
        let files = list_logs(dir)?;
        // Newest day; for a day in both forms, the plain file is the live one.
        let newest_day = files.iter().map(|f| f.day).max();
        let newest = files
            .iter()
            .filter(|f| Some(f.day) == newest_day)
            .min_by_key(|f| f.compressed);
        if let Some(file) = newest {
            if let Some(resume) = resume_from(file)? {
                writer.last_hash = resume.hash;
                if !file.compressed {
                    writer.day = Some(file.day);
                    writer.seq = resume.seq;
                    writer.needs_newline = resume.needs_newline;
                }
            }
        }
        Ok(writer)
    }

    /// The day being written, if any.
    pub fn current_day(&self) -> Option<i64> {
        self.day
    }

    /// The last hash written.
    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }

    /// Days rolled over since the last call, for compression.
    pub fn take_finished(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.finished)
    }

    fn open_day(&mut self, day: i64) -> io::Result<File> {
        let path = self.dir.join(log_name(day));
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.file_type().is_file() {
                return Err(io::Error::other("log file is not a regular file"));
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }

    /// Logs one event. The day is the event's UTC day, never earlier than
    /// the day already being written. Returns the event with its `id` set.
    pub fn append(&mut self, mut event: ActivityEvent) -> io::Result<ActivityEvent> {
        let event_day = timefmt::day_of_ms(event.at);
        let day = match self.day {
            Some(current) if current >= event_day => current,
            Some(current) => {
                self.finished.push(current);
                self.file = None;
                self.seq = 0;
                self.needs_newline = false;
                event_day
            }
            None => event_day,
        };
        if self.day != Some(day) || self.file.is_none() {
            self.file = Some(self.open_day(day)?);
            self.day = Some(day);
        }
        let seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| io::Error::other("seq overflow"))?;
        event.id = format!("es-{}-{seq}", timefmt::day_name(day));
        let hash = chain_hash(&self.last_hash, &event);
        let line = LogLine {
            seq,
            prev: self.last_hash.clone(),
            hash: hash.clone(),
            event,
        };
        let mut bytes = Vec::new();
        if self.needs_newline {
            bytes.push(b'\n');
        }
        serde_json::to_writer(&mut bytes, &line).map_err(io::Error::other)?;
        bytes.push(b'\n');
        let Some(file) = self.file.as_mut() else {
            return Err(io::Error::other("log file not open"));
        };
        file.write_all(&bytes)?;
        self.needs_newline = false;
        self.seq = seq;
        self.last_hash = hash;
        Ok(line.event)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::event::ActivitySource;
    use std::io::Cursor;

    pub fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mc-collector-{name}-{}-{}",
            std::process::id(),
            timefmt::now_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    pub fn event(at: i64, chat: &str) -> ActivityEvent {
        ActivityEvent {
            id: String::new(),
            at,
            chat_id: chat.into(),
            harness: String::new(),
            source: ActivitySource::Eslogger,
            kind: "exec".into(),
            summary: "exec ls".into(),
            detail: serde_json::json!({"argv": ["ls"], "ppid": 1}),
            pid: Some(42),
            approval_id: None,
        }
    }

    const DAY0: i64 = 20_000 * timefmt::DAY_MS;

    #[test]
    fn hash_matches_the_protocol_formula() {
        let ev = event(1, "c");
        let canonical = serde_json::to_string(&serde_json::to_value(&ev).unwrap()).unwrap();
        let mut h = Sha256::new();
        h.update(format!("{ZERO_HASH}\n{canonical}").as_bytes());
        assert_eq!(chain_hash(ZERO_HASH, &ev), hex(&h.finalize()));
        assert!(is_hash(&chain_hash(ZERO_HASH, &ev)));
    }

    #[test]
    fn names() {
        assert_eq!(parse_log_name("es-2025-10-09.jsonl"), Some((20_370, false)));
        assert_eq!(
            parse_log_name("es-2025-10-09.jsonl.gz"),
            Some((20_370, true))
        );
        for bad in [
            "es-2025-10-9.jsonl",
            "es-2025-10-09.json",
            "x-2025-10-09.jsonl",
            "es-2025-10-09.jsonl.gz.tmp",
        ] {
            assert_eq!(parse_log_name(bad), None);
        }
        assert_eq!(log_name(20_370), "es-2025-10-09.jsonl");
    }

    #[test]
    fn writes_chains_and_rolls_over() {
        let dir = temp_dir("chain");
        let mut w = LogWriter::open(&dir).unwrap();
        let a = w.append(event(DAY0 + 5, "c1")).unwrap();
        let b = w.append(event(DAY0 + 6, "c2")).unwrap();
        assert_eq!(a.id, "es-2024-10-04-1");
        assert_eq!(b.id, "es-2024-10-04-2");
        let day0_last = w.last_hash().to_owned();
        let c = w.append(event(DAY0 + timefmt::DAY_MS + 1, "c1")).unwrap();
        assert_eq!(c.id, "es-2024-10-05-1");
        assert_eq!(w.take_finished(), vec![20_000]);
        // An event dated before the current day stays in the current file.
        let d = w.append(event(DAY0 + 7, "c1")).unwrap();
        assert_eq!(d.id, "es-2024-10-05-2");
        drop(w);

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let files = list_logs(&dir).unwrap();
        assert_eq!(files.len(), 2);
        let fmode = fs::metadata(&files[0].path).unwrap().permissions().mode() & 0o777;
        assert_eq!(fmode, 0o600);

        let r0 = verify(open_log(&files[0].path, false).unwrap()).unwrap();
        assert!(r0.ok);
        assert_eq!(r0.lines, 2);
        assert_eq!(r0.first_prev.as_deref(), Some(ZERO_HASH));
        let r1 = verify(open_log(&files[1].path, false).unwrap()).unwrap();
        assert!(r1.ok);
        assert_eq!(r1.first_prev.as_deref(), Some(day0_last.as_str()));

        // Resume after a restart continues the chain and the seq.
        let mut w = LogWriter::open(&dir).unwrap();
        let e = w.append(event(DAY0 + timefmt::DAY_MS + 9, "c1")).unwrap();
        assert_eq!(e.id, "es-2024-10-05-3");
        drop(w);
        let r1 = verify(open_log(&files[1].path, false).unwrap()).unwrap();
        assert!(r1.ok);
        assert_eq!(r1.lines, 3);

        let all = read_events(&dir, 0, i64::MAX / 2, "", 100).unwrap();
        assert_eq!(all.events.len(), 5);
        assert!(all.chain_ok && !all.truncated);
        let c1 = read_events(&dir, 0, i64::MAX / 2, "c1", 2).unwrap();
        assert_eq!(c1.events.len(), 2);
        assert!(c1.truncated);
        // Past the limit the newest are kept, oldest first (Mac test 3.12b).
        let ids: Vec<&str> = c1.events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["es-2024-10-05-2", "es-2024-10-05-3"]);
        let window = read_events(&dir, DAY0 + 6, DAY0 + 6, "", 100).unwrap();
        assert_eq!(window.events.len(), 1);
        assert_eq!(window.events[0].chat_id, "c2");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_edit_cut_and_missing_tail() {
        let dir = temp_dir("edit");
        let mut w = LogWriter::open(&dir).unwrap();
        for i in 0..4 {
            w.append(event(DAY0 + i, "c1")).unwrap();
        }
        drop(w);
        let path = dir.join(log_name(20_000));
        let text = fs::read_to_string(&path).unwrap();

        let edited = text.replacen("exec ls", "exec rm", 2);
        let edited = edited.replacen("exec rm", "exec ls", 1);
        let r = verify(Cursor::new(edited)).unwrap();
        assert!(!r.ok);
        assert_eq!(r.first_bad_line, Some(2));

        let cut = &text[..text.len() - 10];
        let r = verify(Cursor::new(cut.to_owned())).unwrap();
        assert!(!r.ok && r.cut);
        assert_eq!(r.first_bad_line, Some(4));
        assert_eq!(r.lines, 4);

        // A dropped middle line breaks the link.
        let mut ls: Vec<&str> = text.lines().collect();
        ls.remove(1);
        let r = verify(Cursor::new(ls.join("\n") + "\n")).unwrap();
        assert_eq!(r.first_bad_line, Some(2));

        // Restart after a cut: new lines start on a fresh line and the cut
        // stays visible.
        fs::write(&path, cut).unwrap();
        let mut w = LogWriter::open(&dir).unwrap();
        let ev = w.append(event(DAY0 + 10, "c1")).unwrap();
        assert_eq!(ev.id, "es-2024-10-04-4");
        drop(w);
        let r = verify(open_log(&path, false).unwrap()).unwrap();
        assert_eq!(r.first_bad_line, Some(4));
        assert!(!r.cut);
        assert_eq!(r.lines, 5);
        assert!(!read_events(&dir, 0, i64::MAX / 2, "", 10).unwrap().chain_ok);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_link_between_days_is_reported() {
        let dir = temp_dir("link");
        let mut w = LogWriter::open(&dir).unwrap();
        w.append(event(DAY0, "c")).unwrap();
        w.append(event(DAY0 + timefmt::DAY_MS, "c")).unwrap();
        drop(w);
        // Rewrite day 0 with a fresh chain: each file verifies alone, but
        // the link to the next day breaks.
        fs::remove_file(dir.join(log_name(20_000))).unwrap();
        let mut w2 = LogWriter::open(&temp_dir("link-other")).unwrap();
        let other = w2.dir.clone();
        w2.append(event(DAY0 + 3, "c")).unwrap();
        drop(w2);
        fs::copy(other.join(log_name(20_000)), dir.join(log_name(20_000))).unwrap();
        let res = read_events(&dir, 0, i64::MAX / 2, "", 10).unwrap();
        assert_eq!(res.events.len(), 2);
        assert!(!res.chain_ok);
        // Reading only the later day still checks its link to the day before.
        let res = read_events(&dir, DAY0 + timefmt::DAY_MS, i64::MAX / 2, "", 10).unwrap();
        assert_eq!(res.events.len(), 1);
        assert!(!res.chain_ok);
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn refuses_symlinked_folder() {
        let dir = temp_dir("sym");
        fs::create_dir_all(&dir).unwrap();
        let link = dir.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(LogWriter::open(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}

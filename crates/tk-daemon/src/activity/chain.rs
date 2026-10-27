//! The hash chain shared by the app's activity log and the collector's
//! (`macos/activity-collector/PROTOCOL.md`, "Log lines").
//!
//! Each line is `{"seq":n,"prev":"<hex>","hash":"<hex>","<key>":<record>}`,
//! and `hash` is the SHA-256 of `prev`, a newline, then the record read as a
//! `serde_json::Value` and written back with `serde_json::to_string`. The
//! chain shows a cut or edited file. It cannot stop someone with write access
//! rewriting a whole file with a fresh chain (note X, section 3.1).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// The `prev` of the very first line.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The hash of one link.
pub fn link_hash(prev: &str, record: &Value) -> String {
    let body = serde_json::to_string(record).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(b"\n");
    hasher.update(body.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

fn is_hash(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One line, ready to write, without its newline.
pub fn line(seq: u64, prev: &str, key: &str, record: &Value) -> (String, String) {
    let hash = link_hash(prev, record);
    let mut object = serde_json::Map::new();
    object.insert("seq".into(), Value::from(seq));
    object.insert("prev".into(), Value::from(prev));
    object.insert("hash".into(), Value::from(hash.clone()));
    object.insert(key.into(), record.clone());
    (Value::Object(object).to_string(), hash)
}

/// One line read back.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub seq: u64,
    pub prev: String,
    pub hash: String,
    pub key: String,
    pub record: Value,
}

/// Reads one line. `None` for anything that is not a well-formed link,
/// including a line cut short.
pub fn parse_line(text: &str, keys: &[&str]) -> Option<Link> {
    let Value::Object(mut object) = serde_json::from_str::<Value>(text).ok()? else {
        return None;
    };
    let seq = object.get("seq")?.as_u64()?;
    let prev = object.get("prev")?.as_str()?.to_string();
    let hash = object.get("hash")?.as_str()?.to_string();
    if !is_hash(&prev) || !is_hash(&hash) {
        return None;
    }
    let key = keys
        .iter()
        .find(|key| object.contains_key(**key))?
        .to_string();
    let record = object.remove(&key)?;
    Some(Link {
        seq,
        prev,
        hash,
        key,
        record,
    })
}

/// What a chain check found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChainReport {
    pub ok: bool,
    pub lines: usize,
    /// One-based number of the first line that breaks the chain.
    pub first_bad_line: Option<usize>,
    /// The last line was cut short, as a killed writer leaves it.
    pub cut: bool,
}

/// Checks a file's lines in order. `start_prev` is the hash the first line
/// must follow: the last hash of the day before, or `None` to accept any.
pub fn verify<'a>(
    lines: impl Iterator<Item = &'a str>,
    keys: &[&str],
    start_prev: Option<&str>,
) -> ChainReport {
    let lines: Vec<&str> = lines.filter(|line| !line.trim().is_empty()).collect();
    let mut report = ChainReport {
        ok: true,
        lines: lines.len(),
        ..ChainReport::default()
    };
    let mut prev: Option<String> = start_prev.map(str::to_string);
    let mut last_seq: Option<u64> = None;
    for (index, text) in lines.iter().enumerate() {
        let number = index + 1;
        let Some(link) = parse_line(text, keys) else {
            report.ok = false;
            if number == lines.len() {
                report.cut = true;
            }
            report.first_bad_line.get_or_insert(number);
            break;
        };
        let follows = prev.as_deref().is_none_or(|p| p == link.prev);
        let counts = last_seq.is_none_or(|s| s.checked_add(1) == Some(link.seq));
        if !follows || !counts || link_hash(&link.prev, &link.record) != link.hash {
            report.ok = false;
            report.first_bad_line.get_or_insert(number);
            break;
        }
        prev = Some(link.hash);
        last_seq = Some(link.seq);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn build(records: &[Value]) -> Vec<String> {
        let mut prev = GENESIS.to_string();
        records
            .iter()
            .enumerate()
            .map(|(i, record)| {
                let (text, hash) = line(i as u64 + 1, &prev, "event", record);
                prev = hash;
                text
            })
            .collect()
    }

    #[test]
    fn a_written_chain_verifies() {
        let lines = build(&[json!({"id": "a"}), json!({"id": "b", "n": 2}), json!({})]);
        let report = verify(lines.iter().map(String::as_str), &["event"], Some(GENESIS));
        assert!(report.ok, "{report:?}");
        assert_eq!(report.lines, 3);
    }

    #[test]
    fn an_edited_line_is_found() {
        let mut lines = build(&[json!({"id": "a"}), json!({"id": "b"}), json!({"id": "c"})]);
        lines[1] = lines[1].replace("\"b\"", "\"x\"");
        let report = verify(lines.iter().map(String::as_str), &["event"], Some(GENESIS));
        assert!(!report.ok);
        assert_eq!(report.first_bad_line, Some(2));
        assert!(!report.cut);
    }

    #[test]
    fn a_removed_line_is_found() {
        let mut lines = build(&[json!({"id": "a"}), json!({"id": "b"}), json!({"id": "c"})]);
        lines.remove(1);
        let report = verify(lines.iter().map(String::as_str), &["event"], Some(GENESIS));
        assert_eq!(report.first_bad_line, Some(2));
    }

    #[test]
    fn a_cut_last_line_is_reported_as_cut() {
        let mut lines = build(&[json!({"id": "a"}), json!({"id": "b"})]);
        let cut = lines[1][..lines[1].len() / 2].to_string();
        lines[1] = cut;
        let report = verify(lines.iter().map(String::as_str), &["event"], Some(GENESIS));
        assert!(!report.ok);
        assert!(report.cut);
        assert_eq!(report.first_bad_line, Some(2));
    }

    #[test]
    fn a_record_read_back_hashes_as_written() {
        let record = json!({"b": 1, "a": 2, "detail": {"z": [1, 2], "y": "s"}});
        let written = serde_json::to_string(&record).unwrap();
        let reread: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(link_hash(GENESIS, &record), link_hash(GENESIS, &reread));
    }

    #[test]
    fn a_wrong_start_is_refused() {
        let lines = build(&[json!({"id": "a"})]);
        let other = "1".repeat(64);
        let report = verify(lines.iter().map(String::as_str), &["event"], Some(&other));
        assert_eq!(report.first_bad_line, Some(1));
    }

    #[test]
    fn junk_is_never_a_link() {
        assert!(parse_line("", &["event"]).is_none());
        assert!(parse_line("{\"seq\":1}", &["event"]).is_none());
        assert!(parse_line(
            &format!("{{\"seq\":1,\"prev\":\"{GENESIS}\",\"hash\":\"zz\",\"event\":{{}}}}"),
            &["event"]
        )
        .is_none());
    }
}

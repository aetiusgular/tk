//! One audit record line, as the kernel and auditd write it.
//!
//! The shape (auditd 3.1.2, Ubuntu 24.04, recorded in
//! `tests/fixtures/scenario1.*.audit`):
//!
//! ```text
//! type=SYSCALL msg=audit(1793114339.775:64): arch=c000003e syscall=257 ... key="tk_write"<GS>ARCH=x86_64 ...
//! ```
//!
//! - An optional `node=<name> ` prefix (remote logging).
//! - `type=<TYPE>`, then `msg=audit(<seconds>.<millis>:<serial>):`.
//! - `key=value` fields separated by spaces. A value is a quoted string
//!   (`"..."`, no escapes), a plain token, or, for strings the kernel will
//!   not print as is (spaces, non-ASCII, control bytes), upper-case hex with
//!   no quotes. Which fields can be hex is fixed per field (`name`, `cwd`,
//!   `exe`, `comm`, `proctitle`, `aN` in EXECVE); `hex_or_text` decodes them.
//! - With `log_format = ENRICHED` (Ubuntu's default) a 0x1D byte follows the
//!   raw fields and the interpreted fields come after it. Only the raw
//!   fields are read: the interpreted ones are auditd's guesses (user names)
//!   and add nothing the raw ones lack.

/// One parsed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: String,
    /// Milliseconds since the epoch, from `audit(<s>.<ms>:...)`.
    pub ts_ms: i64,
    /// The event serial; records of one event share `(ts_ms, serial)`.
    pub serial: u64,
    /// Raw fields in order, values unquoted but not hex-decoded.
    pub fields: Vec<Field>,
}

/// One `key=value`. `quoted` says the kernel printed the value in quotes,
/// which for the string fields means plain text; unquoted means hex (or a
/// marker such as `(null)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub key: String,
    pub value: String,
    pub quoted: bool,
}

impl Record {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.field(key).map(|f| f.value.as_str())
    }

    pub fn field(&self, key: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.key == key)
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key)?.parse().ok()
    }

    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key)?.parse().ok()
    }

    /// A field that the kernel prints quoted or as hex, decoded to text.
    /// Bytes that are not UTF-8 are replaced, never dropped silently.
    pub fn text(&self, key: &str) -> Option<String> {
        self.field(key).map(|f| {
            if f.quoted {
                f.value.clone()
            } else {
                hex_or_text(&f.value)
            }
        })
    }

    pub fn event_key(&self) -> (i64, u64) {
        (self.ts_ms, self.serial)
    }
}

/// The longest line read. Longer lines are refused: the kernel's own limit
/// for one record is 8,970 bytes (`MAX_AUDIT_MESSAGE_LENGTH`), so a longer
/// line is not an audit record.
pub const MAX_LINE: usize = 16 * 1024;

/// Parses one line. `None` for anything that is not an audit record.
pub fn parse_line(line: &str) -> Option<Record> {
    if line.len() > MAX_LINE {
        return None;
    }
    let raw = line.split('\u{1d}').next().unwrap_or("");
    let raw = raw.trim_end_matches(['\n', '\r']);
    let mut rest = raw;
    if let Some(after) = rest.strip_prefix("node=") {
        let (_, tail) = after.split_once(' ')?;
        rest = tail;
    }
    let rest = rest.strip_prefix("type=")?;
    let (kind, rest) = rest.split_once(' ')?;
    if kind.is_empty()
        || !kind
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let rest = rest.strip_prefix("msg=audit(")?;
    let (stamp, rest) = rest.split_once("):")?;
    let (time, serial) = stamp.split_once(':')?;
    let (secs, millis) = time.split_once('.')?;
    let secs: i64 = secs.parse().ok()?;
    if millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let millis: i64 = millis.parse().ok()?;
    let serial: u64 = serial.parse().ok()?;
    Some(Record {
        kind: kind.to_string(),
        ts_ms: secs.checked_mul(1000)?.checked_add(millis)?,
        serial,
        fields: parse_fields(rest),
    })
}

/// Splits `k=v k="v w" k=v` into pairs. A quoted value runs to the next
/// quote; an unterminated quote takes the rest of the line. Text that is not
/// `k=v` (USER records carry a `msg='...'` with nested fields) is kept as
/// one value under its key, quotes removed.
fn parse_fields(text: &str) -> Vec<Field> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b' ' {
            i += 1;
        }
        let key = &text[key_start..i];
        if i >= bytes.len() || bytes[i] == b' ' {
            // A bare word with no value: skip it.
            continue;
        }
        i += 1; // '='
        let value;
        let quoted = i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'');
        if quoted {
            let quote = bytes[i];
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            value = &text[start..i];
            if i < bytes.len() {
                i += 1;
            }
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b' ' {
                i += 1;
            }
            value = &text[start..i];
        }
        if !key.is_empty() {
            out.push(Field {
                key: key.to_string(),
                value: value.to_string(),
                quoted,
            });
        }
    }
    out
}

/// Decodes an unquoted value the kernel printed as hex (upper case, even
/// length). Anything else, such as `(null)`, is returned as it is.
pub fn hex_or_text(value: &str) -> String {
    match decode_hex(value) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => value.to_string(),
    }
}

/// Upper-case hex to bytes; `None` when `value` is not even-length hex.
pub fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return None;
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_syscall_line_with_its_enriched_tail() {
        let line = "type=SYSCALL msg=audit(1793114339.775:64): arch=c000003e syscall=257 success=yes exit=3 a0=ffffff9c ppid=1024 pid=1031 comm=\"fakeharness.sh\" exe=\"/usr/bin/bash\" key=\"tk_write\"\u{1d}ARCH=x86_64 SYSCALL=openat";
        let r = parse_line(line).unwrap();
        assert_eq!(r.kind, "SYSCALL");
        assert_eq!(r.ts_ms, 1_793_114_339_775);
        assert_eq!(r.serial, 64);
        assert_eq!(r.get("syscall"), Some("257"));
        assert_eq!(r.get("key"), Some("tk_write"));
        assert_eq!(r.get("comm"), Some("fakeharness.sh"));
        assert!(r.get("ARCH").is_none(), "the enriched part is not read");
    }

    #[test]
    fn hex_names_decode_and_quoted_names_stay() {
        let line = "type=PATH msg=audit(1.000:2): item=1 name=2F686F6D652F7562756E74752F6F7574207369646520C3BC2E747874 nametype=CREATE";
        let r = parse_line(line).unwrap();
        assert_eq!(r.text("name").unwrap(), "/home/ubuntu/out side ü.txt");
        let q = parse_line("type=CWD msg=audit(1.000:2): cwd=\"/home/ubuntu/proj\"").unwrap();
        assert_eq!(q.text("cwd").unwrap(), "/home/ubuntu/proj");
        // A file really named in hex digits is quoted, so it stays as typed.
        let h = parse_line("type=PATH msg=audit(1.000:2): item=0 name=\"CAFE\"").unwrap();
        assert_eq!(h.text("name").unwrap(), "CAFE");
    }

    #[test]
    fn a_node_prefix_is_skipped() {
        let r = parse_line("node=box type=EOE msg=audit(5.123:9): ").unwrap();
        assert_eq!(r.kind, "EOE");
        assert_eq!((r.ts_ms, r.serial), (5123, 9));
    }

    #[test]
    fn not_a_record() {
        for line in [
            "",
            "hello",
            "type=SYSCALL",
            "type=SYSCALL msg=audit(abc:1): x=1",
            "type=SYSCALL msg=audit(1.1:1): x=1",
            "type=sys msg=audit(1.000:1): x=1",
        ] {
            assert!(parse_line(line).is_none(), "{line:?}");
        }
        assert!(parse_line(&format!(
            "type=EOE msg=audit(1.000:1): {}",
            "a".repeat(MAX_LINE)
        ))
        .is_none());
    }

    #[test]
    fn hex_is_only_even_upper_case_runs() {
        assert_eq!(decode_hex("6869"), Some(b"hi".to_vec()));
        assert_eq!(decode_hex("686"), None);
        assert_eq!(decode_hex("6a69"), None);
        assert_eq!(hex_or_text("/bin/ls"), "/bin/ls");
        // Invalid UTF-8 is replaced, not dropped.
        assert_eq!(hex_or_text("FF41"), "\u{fffd}A");
    }

    #[test]
    fn an_unterminated_quote_takes_the_rest() {
        let r = parse_line("type=EXECVE msg=audit(1.000:1): argc=2 a0=\"ls a1=b").unwrap();
        assert_eq!(r.get("a0"), Some("ls a1=b"));
    }
}

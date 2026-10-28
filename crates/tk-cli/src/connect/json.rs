//! Order- and byte-preserving JSON (and JSONC) for config edits.
//!
//! `tk connect` changes one entry in a file a harness owns and leaves every
//! other byte alone: other keys keep their order, numbers keep their
//! spelling, and in JSONC files comments stay where they were. The parser
//! records where each value sits; an edit splices new text into the
//! original at those offsets, then the caller re-parses the result and
//! checks that only the intended member changed (`connect::edit`).
//!
//! The serializer writes the way JavaScript's `JSON.stringify(v, null, n)`
//! does, because every harness recorded so far (Claude Code 2.1.311, Pi
//! 1.4.0, Gemini CLI 0.65.0, Qwen Code 0.26.1, Copilot CLI 1.0.104) writes
//! its JSON with it: `"key": value`, one member per line, `{}` and `[]` when
//! empty, and only `"`, `\` and control characters escaped.

/// A JSON value with object members in file order. Numbers keep their
/// source spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    pub fn object(members: Vec<(&str, Value)>) -> Value {
        Value::Object(
            members
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn strings(items: &[String]) -> Value {
        Value::Array(items.iter().map(|s| Value::Str(s.clone())).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_path(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = self;
        for seg in path {
            cur = cur.get(seg)?;
        }
        Some(cur)
    }

    /// Sets `path` + `key` to `value`, creating missing objects on the way,
    /// appending a new key at the end of its object. Used to compute what an
    /// edit must produce, independently of the text splice.
    pub fn set_path(&mut self, path: &[&str], key: &str, value: Value) -> Result<(), String> {
        let mut cur = self;
        for seg in path {
            let Value::Object(members) = cur else {
                return Err(format!("`{seg}` sits inside a value that is not an object"));
            };
            let idx = match members.iter().position(|(k, _)| k == seg) {
                Some(i) => i,
                None => {
                    members.push((seg.to_string(), Value::Object(Vec::new())));
                    members.len() - 1
                }
            };
            cur = &mut members[idx].1;
        }
        let Value::Object(members) = cur else {
            return Err(format!("`{}` is not an object", path.join(".")));
        };
        match members.iter().position(|(k, _)| k == key) {
            Some(i) => members[i].1 = value,
            None => members.push((key.to_string(), value)),
        }
        Ok(())
    }

    /// Removes `path` + `key`. Returns whether it was there.
    pub fn remove_path(&mut self, path: &[&str], key: &str) -> bool {
        let mut cur = self;
        for seg in path {
            let Value::Object(members) = cur else {
                return false;
            };
            let Some(i) = members.iter().position(|(k, _)| k == seg) else {
                return false;
            };
            cur = &mut members[i].1;
        }
        let Value::Object(members) = cur else {
            return false;
        };
        match members.iter().position(|(k, _)| k == key) {
            Some(i) => {
                members.remove(i);
                true
            }
            None => false,
        }
    }
}

/// A parsed value with its byte span in the source (`start..end`).
#[derive(Clone, Debug)]
pub struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Clone, Debug)]
pub enum Kind {
    Scalar(Value),
    Array(Vec<Node>),
    Object(Vec<Member>),
}

#[derive(Clone, Debug)]
pub struct Member {
    pub key: String,
    pub key_start: usize,
    pub value: Node,
    /// Offset of the comma after this member's value, if there is one.
    pub comma: Option<usize>,
}

impl Node {
    pub fn to_value(&self) -> Value {
        match &self.kind {
            Kind::Scalar(v) => v.clone(),
            Kind::Array(items) => Value::Array(items.iter().map(Node::to_value).collect()),
            Kind::Object(members) => Value::Object(
                members
                    .iter()
                    .map(|m| (m.key.clone(), m.value.to_value()))
                    .collect(),
            ),
        }
    }

    fn member(&self, key: &str) -> Option<&Member> {
        match &self.kind {
            Kind::Object(members) => members.iter().find(|m| m.key == key),
            _ => None,
        }
    }
}

/// A parsed document.
#[derive(Debug)]
pub struct Doc<'a> {
    pub text: &'a str,
    /// `None` for an empty or whitespace-only file.
    pub root: Option<Node>,
    pub has_comments: bool,
    /// One level of indentation as the file writes it (`"  "` if unknown).
    pub unit: String,
}

const MAX_DEPTH: usize = 128;

/// Parses `text`. `jsonc` allows `//` and `/* */` comments and trailing
/// commas (Zed, Gemini, Qwen); plain JSON refuses both, as Claude Code's
/// and Pi's own parsers do.
pub fn parse(text: &str, jsonc: bool) -> Result<Doc<'_>, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        jsonc,
        comments: false,
    };
    p.skip_ws()?;
    let root = if p.i >= p.s.len() {
        None
    } else {
        let node = p.value(0)?;
        p.skip_ws()?;
        if p.i < p.s.len() {
            return Err(p.err("text after the end of the JSON value"));
        }
        if !matches!(node.kind, Kind::Object(_)) {
            return Err("the file's top level is not a JSON object".to_string());
        }
        Some(node)
    };
    Ok(Doc {
        text,
        root,
        has_comments: p.comments,
        unit: detect_unit(text),
    })
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    jsonc: bool,
    comments: bool,
}

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        let line = self.s[..self.i.min(self.s.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1;
        format!("invalid JSON at line {line}: {what}")
    }

    fn skip_ws(&mut self) -> Result<(), String> {
        loop {
            match self.s.get(self.i) {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.i += 1,
                Some(b'/') if self.jsonc => match self.s.get(self.i + 1) {
                    Some(b'/') => {
                        self.comments = true;
                        while self.i < self.s.len() && self.s[self.i] != b'\n' {
                            self.i += 1;
                        }
                    }
                    Some(b'*') => {
                        self.comments = true;
                        self.i += 2;
                        loop {
                            if self.i + 1 >= self.s.len() {
                                return Err(self.err("unterminated comment"));
                            }
                            if self.s[self.i] == b'*' && self.s[self.i + 1] == b'/' {
                                self.i += 2;
                                break;
                            }
                            self.i += 1;
                        }
                    }
                    _ => return Ok(()),
                },
                _ => return Ok(()),
            }
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        let start = self.i;
        let kind = match self.s.get(self.i) {
            Some(b'{') => self.object(depth)?,
            Some(b'[') => self.array(depth)?,
            Some(b'"') => Kind::Scalar(Value::Str(self.string()?)),
            Some(b't') => self.literal("true", Value::Bool(true))?,
            Some(b'f') => self.literal("false", Value::Bool(false))?,
            Some(b'n') => self.literal("null", Value::Null)?,
            Some(b'-' | b'0'..=b'9') => Kind::Scalar(Value::Number(self.number()?)),
            Some(_) => return Err(self.err("unexpected character")),
            None => return Err(self.err("unexpected end of file")),
        };
        Ok(Node {
            start,
            end: self.i,
            kind,
        })
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Kind, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(Kind::Scalar(v))
        } else {
            Err(self.err("unexpected word"))
        }
    }

    fn number(&mut self) -> Result<String, String> {
        let start = self.i;
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.s.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            _ => return Err(self.err("bad number")),
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            let d = self.i;
            while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == d {
                return Err(self.err("bad number"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let d = self.i;
            while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            if self.i == d {
                return Err(self.err("bad number"));
            }
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.err("short \\u escape"))?;
        let h = std::str::from_utf8(h).map_err(|_| self.err("bad \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err(self.err("unterminated string"));
            };
            match b {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else {
                        return Err(self.err("unterminated string"));
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.s.get(self.i) == Some(&b'\\')
                                    && self.s.get(self.i + 1) == Some(&b'u')
                                {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return Err(self.err("unpaired surrogate"));
                                    }
                                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                } else {
                                    return Err(self.err("unpaired surrogate"));
                                }
                            } else if (0xDC00..0xE000).contains(&cp) {
                                return Err(self.err("unpaired surrogate"));
                            }
                            let c = char::from_u32(cp).ok_or_else(|| self.err("bad code point"))?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                0x00..=0x1f => return Err(self.err("control character in a string")),
                _ => {
                    out.push(b);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.err("string is not UTF-8"))
    }

    fn array(&mut self, depth: usize) -> Result<Kind, String> {
        self.i += 1;
        let mut items = Vec::new();
        self.skip_ws()?;
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Kind::Array(items));
        }
        loop {
            self.skip_ws()?;
            if self.jsonc && self.s.get(self.i) == Some(&b']') && !items.is_empty() {
                self.i += 1;
                return Ok(Kind::Array(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip_ws()?;
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Kind::Array(items));
                }
                _ => return Err(self.err("expected `,` or `]`")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Kind, String> {
        self.i += 1;
        let mut members: Vec<Member> = Vec::new();
        self.skip_ws()?;
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Kind::Object(members));
        }
        loop {
            self.skip_ws()?;
            if self.jsonc && self.s.get(self.i) == Some(&b'}') && !members.is_empty() {
                self.i += 1;
                return Ok(Kind::Object(members));
            }
            if self.s.get(self.i) != Some(&b'"') {
                return Err(self.err("expected a key"));
            }
            let key_start = self.i;
            let key = self.string()?;
            if members.iter().any(|m| m.key == key) {
                return Err(self.err(&format!("duplicate key `{key}`")));
            }
            self.skip_ws()?;
            if self.s.get(self.i) != Some(&b':') {
                return Err(self.err("expected `:`"));
            }
            self.i += 1;
            self.skip_ws()?;
            let value = self.value(depth + 1)?;
            self.skip_ws()?;
            match self.s.get(self.i) {
                Some(b',') => {
                    members.push(Member {
                        key,
                        key_start,
                        value,
                        comma: Some(self.i),
                    });
                    self.i += 1;
                }
                Some(b'}') => {
                    members.push(Member {
                        key,
                        key_start,
                        value,
                        comma: None,
                    });
                    self.i += 1;
                    return Ok(Kind::Object(members));
                }
                _ => return Err(self.err("expected `,` or `}`")),
            }
        }
    }
}

/// The file's indentation unit: the leading whitespace of the first
/// indented line. Two spaces when the file has none.
pub fn detect_unit(text: &str) -> String {
    for line in text.lines() {
        let ws: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if !ws.is_empty() && line.len() > ws.len() {
            if ws.starts_with('\t') {
                return "\t".to_string();
            }
            return ws;
        }
    }
    "  ".to_string()
}

/// `JSON.stringify(s)`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `JSON.stringify(v, null, unit)` with every line after the first
/// prefixed by `indent`.
pub fn pretty(v: &Value, unit: &str, indent: &str) -> String {
    let mut out = String::new();
    write_pretty(v, unit, indent, &mut out);
    out
}

fn write_pretty(v: &Value, unit: &str, indent: &str, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(n),
        Value::Str(s) => out.push_str(&quote(s)),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(members) if members.is_empty() => out.push_str("{}"),
        Value::Array(items) => {
            let inner = format!("{indent}{unit}");
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&inner);
                write_pretty(item, unit, &inner, out);
            }
            out.push('\n');
            out.push_str(indent);
            out.push(']');
        }
        Value::Object(members) => {
            let inner = format!("{indent}{unit}");
            out.push_str("{\n");
            for (i, (k, val)) in members.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&inner);
                out.push_str(&quote(k));
                out.push_str(": ");
                write_pretty(val, unit, &inner, out);
            }
            out.push('\n');
            out.push_str(indent);
            out.push('}');
        }
    }
}

/// One line: `{"a": 1, "b": [2]}`. For an object written on one line in
/// the file.
pub fn compact(v: &Value) -> String {
    match v {
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(compact).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(members) => format!(
            "{{{}}}",
            members
                .iter()
                .map(|(k, v)| format!("{}: {}", quote(k), compact(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => pretty(other, "", ""),
    }
}

/// Why an edit did not happen.
#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// The key is already there. `same` when its value already equals the
    /// one asked for.
    Exists { same: bool },
    /// The key to remove is not there.
    Missing,
    /// The path runs through something that is not an object.
    NotObject(String),
}

fn line_start(text: &str, pos: usize) -> usize {
    text[..pos].rfind('\n').map_or(0, |i| i + 1)
}

fn indent_of_line(text: &str, pos: usize) -> &str {
    let ls = line_start(text, pos);
    let rest = &text[ls..];
    let n = rest
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count();
    &rest[..n]
}

/// True when only whitespace precedes `pos` on its line.
fn first_on_line(text: &str, pos: usize) -> bool {
    text[line_start(text, pos)..pos]
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
}

/// End of the line `pos` is on (offset of its `\n`, or the text's end),
/// when the rest of the line after `pos` is only whitespace or a comment.
fn quiet_eol(text: &str, pos: usize) -> Option<usize> {
    let eol = text[pos..].find('\n').map_or(text.len(), |i| pos + i);
    let rest = text[pos..eol].trim();
    if rest.is_empty() || rest.starts_with("//") || (rest.starts_with("/*") && rest.ends_with("*/"))
    {
        Some(eol)
    } else {
        None
    }
}

impl Doc<'_> {
    fn object_at(&self, path: &[&str]) -> Result<Option<&Node>, EditError> {
        let Some(mut cur) = self.root.as_ref() else {
            return Ok(None);
        };
        for seg in path {
            match cur.member(seg) {
                Some(m) => cur = &m.value,
                None => return Ok(None),
            }
        }
        if matches!(cur.kind, Kind::Object(_)) {
            Ok(Some(cur))
        } else {
            Err(EditError::NotObject(path.join(".")))
        }
    }

    /// The value at `path` + `key`, if there is one.
    pub fn lookup(&self, path: &[&str], key: &str) -> Option<Value> {
        let root = self.root.as_ref()?.to_value();
        root.get_path(path)?.get(key).cloned()
    }

    /// The new text with `path` + `key` set to `value`. Missing objects on
    /// `path` are created. An existing key is replaced only when `replace`.
    /// `trailing_newline` applies only when the file is created from
    /// nothing; an existing file keeps its own ending.
    pub fn set_member(
        &self,
        path: &[&str],
        key: &str,
        value: &Value,
        replace: bool,
        trailing_newline: bool,
    ) -> Result<String, EditError> {
        let text = self.text;
        let unit = self.unit.as_str();
        let Some(root) = self.root.as_ref() else {
            // A new file: `{ path: { key: value } }`.
            let mut whole = Value::Object(Vec::new());
            whole
                .set_path(path, key, value.clone())
                .map_err(EditError::NotObject)?;
            let mut out = pretty(&whole, unit, "");
            if trailing_newline {
                out.push('\n');
            }
            return Ok(out);
        };
        // Walk as deep as the file goes.
        let mut cur = root;
        let mut depth = 0;
        while depth < path.len() {
            match cur.member(path[depth]) {
                Some(m) => {
                    if !matches!(m.value.kind, Kind::Object(_)) {
                        return Err(EditError::NotObject(path[..=depth].join(".")));
                    }
                    cur = &m.value;
                    depth += 1;
                }
                None => break,
            }
        }
        if depth < path.len() {
            // Build the rest of the path as one new member of `cur`.
            let mut nested = Value::Object(Vec::new());
            nested
                .set_path(&path[depth + 1..], key, value.clone())
                .map_err(EditError::NotObject)?;
            return Ok(insert_member(text, cur, unit, path[depth], &nested));
        }
        if let Some(m) = cur.member(key) {
            let same = m.value.to_value() == *value;
            if !replace || same {
                return Err(EditError::Exists { same });
            }
            let indent = if first_on_line(text, m.key_start) {
                indent_of_line(text, m.key_start).to_string()
            } else {
                format!("{}{unit}", indent_of_line(text, cur.start))
            };
            let mut out = String::with_capacity(text.len() + 64);
            out.push_str(&text[..m.value.start]);
            out.push_str(&pretty(value, unit, &indent));
            out.push_str(&text[m.value.end..]);
            return Ok(out);
        }
        Ok(insert_member(text, cur, unit, key, value))
    }

    /// The new text without `path` + `key`.
    pub fn remove_member(&self, path: &[&str], key: &str) -> Result<String, EditError> {
        let text = self.text;
        let obj = self.object_at(path)?.ok_or(EditError::Missing)?;
        let Kind::Object(members) = &obj.kind else {
            return Err(EditError::NotObject(path.join(".")));
        };
        let i = members
            .iter()
            .position(|m| m.key == key)
            .ok_or(EditError::Missing)?;
        let m = &members[i];
        let m_end = m.comma.map_or(m.value.end, |c| c + 1);
        let (del_start, del_end) = if members.len() == 1 {
            let interior = &text[obj.start + 1..obj.end - 1];
            let only_this = interior.trim() == text[m.key_start..m_end].trim();
            if only_this {
                // `{}` as JSON.stringify writes an empty object.
                let mut out = String::with_capacity(text.len());
                out.push_str(&text[..obj.start]);
                out.push_str("{}");
                out.push_str(&text[obj.end..]);
                return Ok(out);
            }
            let s = if first_on_line(text, m.key_start) {
                line_start(text, m.key_start)
            } else {
                m.key_start
            };
            let e = match quiet_eol(text, m_end) {
                Some(eol) if eol < text.len() && s == line_start(text, m.key_start) => eol + 1,
                Some(eol) => eol,
                None => m_end,
            };
            (s, e)
        } else if i + 1 < members.len() {
            let s = if first_on_line(text, m.key_start) {
                line_start(text, m.key_start)
            } else {
                m.key_start
            };
            let mut e = m_end;
            let q = quiet_eol(text, m_end);
            if let (Some(eol), true) = (q, s == line_start(text, m.key_start)) {
                e = (eol + 1).min(text.len());
            } else {
                while e < text.len() && (text.as_bytes()[e] == b' ' || text.as_bytes()[e] == b'\t')
                {
                    e += 1;
                }
            }
            (s, e)
        } else {
            // The last of several: drop the previous member's comma too.
            let prev = &members[i - 1];
            let s = prev.comma.unwrap_or(prev.value.end);
            (s, m_end)
        };
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..del_start]);
        out.push_str(&text[del_end..]);
        Ok(out)
    }
}

/// Inserts `"key": value` as the last member of `obj`.
fn insert_member(text: &str, obj: &Node, unit: &str, key: &str, value: &Value) -> String {
    let Kind::Object(members) = &obj.kind else {
        unreachable!("insert_member is only called on objects");
    };
    let owner_indent = indent_of_line(text, obj.start).to_string();
    let one_line = !text[obj.start..obj.end].contains('\n');
    let mut out = String::with_capacity(text.len() + 128);
    match members.last() {
        None => {
            let child = format!("{owner_indent}{unit}");
            let member = format!("{}: {}", quote(key), pretty(value, unit, &child));
            let interior = &text[obj.start + 1..obj.end - 1];
            out.push_str(&text[..obj.start]);
            if interior.trim().is_empty() {
                out.push_str(&format!("{{\n{child}{member}\n{owner_indent}}}"));
                out.push_str(&text[obj.end..]);
            } else {
                // Only comments inside: keep them after the new member.
                out.push_str(&format!("{{\n{child}{member}"));
                out.push_str(&text[obj.start + 1..]);
            }
        }
        Some(last) if one_line => {
            let at = last.comma.map_or(last.value.end, |c| c + 1);
            out.push_str(&text[..at]);
            if last.comma.is_none() {
                out.push(',');
            }
            out.push_str(&format!(" {}: {}", quote(key), compact(value)));
            out.push_str(&text[at..]);
        }
        Some(last) => {
            let child = if first_on_line(text, last.key_start) {
                indent_of_line(text, last.key_start).to_string()
            } else {
                format!("{owner_indent}{unit}")
            };
            let member = format!("{}: {}", quote(key), pretty(value, unit, &child));
            let after = last.comma.map_or(last.value.end, |c| c + 1);
            let quiet = quiet_eol(text, after);
            let at = quiet.unwrap_or(after);
            out.push_str(&text[..last.value.end]);
            if last.comma.is_none() {
                out.push(',');
            }
            out.push_str(&text[last.value.end..at]);
            out.push('\n');
            out.push_str(&child);
            out.push_str(&member);
            if quiet.is_none() {
                // Something else followed on the same line (`... }`): put
                // it on its own line again.
                out.push('\n');
                out.push_str(&owner_indent);
                out.push_str(text[at..].trim_start_matches([' ', '\t']));
            } else {
                out.push_str(&text[at..]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(text: &str, path: &[&str], key: &str, v: &Value) -> String {
        parse(text, true)
            .unwrap()
            .set_member(path, key, v, false, true)
            .unwrap()
    }

    fn entry() -> Value {
        Value::object(vec![
            ("command", Value::str("uvx")),
            ("args", Value::strings(&["mcp-server-time".to_string()])),
        ])
    }

    #[test]
    fn parses_scalars_strings_and_keeps_number_spelling() {
        let d = parse(
            r#"{"a": 1.50, "b": "x\u00e9\"\n", "c": [true, null, -0e+2]}"#,
            false,
        )
        .unwrap();
        let v = d.root.unwrap().to_value();
        assert_eq!(v.get("a"), Some(&Value::Number("1.50".into())));
        assert_eq!(v.get("b"), Some(&Value::str("x\u{e9}\"\n")));
        assert_eq!(
            v.get("c"),
            Some(&Value::Array(vec![
                Value::Bool(true),
                Value::Null,
                Value::Number("-0e+2".into())
            ]))
        );
    }

    #[test]
    fn refuses_bad_json_and_comments_in_strict_mode() {
        for bad in [
            "{",
            "{\"a\": }",
            "{\"a\": 1,}",
            "[1]",
            "{\"a\": 01}",
            "{\"a\": \"\u{1}\"}",
            "{\"a\": 1} x",
            "{\"a\": 1, \"a\": 2}",
            "{\"a\": \"\\ud800\"}",
        ] {
            assert!(parse(bad, false).is_err(), "{bad:?}");
        }
        assert!(parse("{\n // c\n \"a\": 1\n}", false).is_err());
        let d = parse("{\n // c\n \"a\": 1,\n}", true).unwrap();
        assert!(d.has_comments);
    }

    #[test]
    fn deep_nesting_is_refused_not_overflowed() {
        let deep = format!("{{\"a\": {}{}}}", "[".repeat(5000), "]".repeat(5000));
        assert!(parse(&deep, false).is_err());
    }

    #[test]
    fn quote_matches_json_stringify() {
        assert_eq!(
            quote("a\"b\\c\n\u{1f}é/\u{2028}"),
            "\"a\\\"b\\\\c\\n\\u001fé/\u{2028}\""
        );
    }

    #[test]
    fn pretty_matches_json_stringify() {
        let v = Value::object(vec![
            ("a", Value::Array(vec![])),
            ("b", Value::Object(vec![])),
            ("c", Value::strings(&["x".into(), "y".into()])),
        ]);
        assert_eq!(
            pretty(&v, "  ", ""),
            "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    \"x\",\n    \"y\"\n  ]\n}"
        );
    }

    #[test]
    fn new_file_gets_the_whole_path() {
        let d = parse("", false).unwrap();
        let out = d
            .set_member(&["mcpServers"], "time", &entry(), false, true)
            .unwrap();
        assert_eq!(
            out,
            "{\n  \"mcpServers\": {\n    \"time\": {\n      \"command\": \"uvx\",\n      \"args\": [\n        \"mcp-server-time\"\n      ]\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn appends_after_the_last_member_and_keeps_the_rest() {
        let text = "{\n  \"z\": 1.0,\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"a\"\n    }\n  }\n}";
        let out = set(text, &["mcpServers"], "time", &entry());
        assert_eq!(
            out,
            "{\n  \"z\": 1.0,\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"a\"\n    },\n    \"time\": {\n      \"command\": \"uvx\",\n      \"args\": [\n        \"mcp-server-time\"\n      ]\n    }\n  }\n}"
        );
    }

    #[test]
    fn creates_the_container_at_the_end_of_the_root() {
        let text = "{\n  \"z\": true\n}\n";
        let out = set(
            text,
            &["mcpServers"],
            "t",
            &Value::object(vec![("url", Value::str("u"))]),
        );
        assert_eq!(
            out,
            "{\n  \"z\": true,\n  \"mcpServers\": {\n    \"t\": {\n      \"url\": \"u\"\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn fills_an_empty_object_and_follows_four_space_files() {
        let text = "{\n    \"mcpServers\": {}\n}\n";
        let out = set(
            text,
            &["mcpServers"],
            "b",
            &Value::object(vec![("command", Value::str("b"))]),
        );
        assert_eq!(
            out,
            "{\n    \"mcpServers\": {\n        \"b\": {\n            \"command\": \"b\"\n        }\n    }\n}\n"
        );
    }

    #[test]
    fn keeps_comments_and_trailing_commas_in_jsonc() {
        let text = "{\n  // top\n  \"context_servers\": {\n    \"a\": {} // note\n  },\n}\n";
        let out = set(text, &["context_servers"], "b", &Value::Object(vec![]));
        assert_eq!(
            out,
            "{\n  // top\n  \"context_servers\": {\n    \"a\": {}, // note\n    \"b\": {}\n  },\n}\n"
        );
        let text2 = "{\n  \"context_servers\": {\n    \"a\": 1,\n  }\n}";
        let out2 = set(text2, &["context_servers"], "b", &Value::Number("2".into()));
        assert_eq!(
            out2,
            "{\n  \"context_servers\": {\n    \"a\": 1,\n    \"b\": 2\n  }\n}"
        );
    }

    #[test]
    fn one_line_objects_stay_on_one_line() {
        let out = set(
            "{\"mcpServers\": {\"a\": 1}}",
            &["mcpServers"],
            "b",
            &Value::object(vec![("x", Value::Array(vec![]))]),
        );
        assert_eq!(out, "{\"mcpServers\": {\"a\": 1, \"b\": {\"x\": []}}}");
    }

    #[test]
    fn existing_key_is_refused_unless_replace() {
        let text = "{\n  \"mcpServers\": {\n    \"a\": {\n      \"command\": \"a\"\n    }\n  }\n}";
        let d = parse(text, false).unwrap();
        let v = Value::object(vec![("command", Value::str("b"))]);
        assert_eq!(
            d.set_member(&["mcpServers"], "a", &v, false, false),
            Err(EditError::Exists { same: false })
        );
        let same = Value::object(vec![("command", Value::str("a"))]);
        assert_eq!(
            d.set_member(&["mcpServers"], "a", &same, true, false),
            Err(EditError::Exists { same: true })
        );
        let out = d.set_member(&["mcpServers"], "a", &v, true, false).unwrap();
        assert_eq!(
            out,
            text.replace("\"command\": \"a\"", "\"command\": \"b\"")
        );
    }

    #[test]
    fn path_through_a_non_object_is_refused() {
        let d = parse("{\"mcpServers\": []}", false).unwrap();
        assert!(matches!(
            d.set_member(&["mcpServers"], "a", &Value::Null, false, false),
            Err(EditError::NotObject(_))
        ));
    }

    #[test]
    fn removes_first_middle_last_and_only_members() {
        let text = "{\n  \"s\": {\n    \"a\": 1,\n    \"b\": 2,\n    \"c\": 3\n  }\n}";
        let d = parse(text, false).unwrap();
        assert_eq!(
            d.remove_member(&["s"], "a").unwrap(),
            "{\n  \"s\": {\n    \"b\": 2,\n    \"c\": 3\n  }\n}"
        );
        assert_eq!(
            d.remove_member(&["s"], "b").unwrap(),
            "{\n  \"s\": {\n    \"a\": 1,\n    \"c\": 3\n  }\n}"
        );
        assert_eq!(
            d.remove_member(&["s"], "c").unwrap(),
            "{\n  \"s\": {\n    \"a\": 1,\n    \"b\": 2\n  }\n}"
        );
        assert_eq!(d.remove_member(&["s"], "x"), Err(EditError::Missing));
        let only = parse(
            "{\n  \"s\": {\n    \"a\": {\n      \"k\": 1\n    }\n  }\n}",
            false,
        )
        .unwrap();
        assert_eq!(
            only.remove_member(&["s"], "a").unwrap(),
            "{\n  \"s\": {}\n}"
        );
    }

    #[test]
    fn value_set_and_remove_paths() {
        let mut v = Value::Object(vec![]);
        v.set_path(&["a", "b"], "c", Value::Null).unwrap();
        assert_eq!(v.get_path(&["a", "b", "c"]), Some(&Value::Null));
        assert!(v.remove_path(&["a", "b"], "c"));
        assert!(!v.remove_path(&["a", "b"], "c"));
        let mut s = Value::Str("x".into());
        assert!(s.set_path(&["a"], "b", Value::Null).is_err());
    }
}

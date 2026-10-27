//! OTLP log decoding, JSON and protobuf, with no dependency beyond serde_json.
//!
//! Only the logs signal is decoded: it carries every harness event the
//! activity log uses (Claude Code's `claude_code.*` events, Codex's
//! `codex.*` events). Metrics and traces are accepted by the receiver and
//! dropped without decoding.
//!
//! Both decoders are bounded: at most `MAX_RECORDS` records per request, at
//! most `MAX_DEPTH` levels of nested values, and every length is checked
//! against what is left of the buffer before it is used. Nothing here panics
//! on any input; a malformed body is an `Err`.

use serde_json::{Map, Value};

/// Records kept from one request. The rest are counted and dropped.
pub const MAX_RECORDS: usize = 2000;
/// Nested arrays and key-value lists deeper than this are refused.
pub const MAX_DEPTH: usize = 16;
/// Attributes kept per record or resource. Later ones are dropped.
pub const MAX_ATTRIBUTES: usize = 128;

/// One OTLP log record, with its resource's attributes beside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtelLog {
    /// Milliseconds since the epoch: the record's time, else its observed
    /// time, else 0 (the caller fills in the receive time).
    pub time_ms: i64,
    /// `event.name` from the attributes, else the record's `event_name`
    /// field, else the body when it is a string.
    pub event_name: String,
    pub body: Option<Value>,
    pub attributes: Map<String, Value>,
    pub resource: Map<String, Value>,
}

/// What a request held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decoded {
    pub logs: Vec<OtelLog>,
    /// Records past `MAX_RECORDS`.
    pub dropped: usize,
}

fn nanos_to_ms(nanos: u64) -> i64 {
    i64::try_from(nanos / 1_000_000).unwrap_or(i64::MAX)
}

fn finish(mut log: OtelLog, record_event_name: Option<String>) -> OtelLog {
    let from_attr = log
        .attributes
        .get("event.name")
        .and_then(Value::as_str)
        .map(str::to_string);
    log.event_name = from_attr
        .or(record_event_name.filter(|n| !n.is_empty()))
        .or_else(|| {
            log.body
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    log
}

// ---------------------------------------------------------------- JSON

/// Decodes an OTLP/JSON `ExportLogsServiceRequest`. Accepts the spec's
/// lowerCamelCase names and the snake_case some exporters write.
pub fn decode_json(body: &[u8]) -> Result<Decoded, String> {
    let root: Value = serde_json::from_slice(body).map_err(|_| "body is not JSON".to_string())?;
    let mut out = Decoded::default();
    for resource_logs in array(&root, &["resourceLogs", "resource_logs"]) {
        let resource = resource_logs
            .get("resource")
            .map(|r| json_attributes(r.get("attributes"), 0))
            .transpose()?
            .map(slim_resource)
            .unwrap_or_default();
        for scope_logs in array(resource_logs, &["scopeLogs", "scope_logs"]) {
            for record in array(scope_logs, &["logRecords", "log_records"]) {
                if out.logs.len() >= MAX_RECORDS {
                    out.dropped = out.dropped.saturating_add(1);
                    continue;
                }
                out.logs.push(json_record(record, &resource)?);
            }
        }
    }
    Ok(out)
}

fn array<'a>(value: &'a Value, names: &[&str]) -> impl Iterator<Item = &'a Value> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_array))
        .into_iter()
        .flatten()
}

fn field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| value.get(*name))
}

/// A 64-bit number that OTLP/JSON writes as a string or a number.
fn json_u64(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

fn json_record(record: &Value, resource: &Map<String, Value>) -> Result<OtelLog, String> {
    let time = json_u64(field(record, &["timeUnixNano", "time_unix_nano"]))
        .filter(|t| *t > 0)
        .or_else(|| {
            json_u64(field(
                record,
                &["observedTimeUnixNano", "observed_time_unix_nano"],
            ))
        })
        .map(nanos_to_ms)
        .unwrap_or(0);
    let body = match record.get("body") {
        Some(v) => Some(json_any(v, 0)?),
        None => None,
    };
    let attributes = json_attributes(record.get("attributes"), 0)?;
    let event_name = field(record, &["eventName", "event_name"])
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(finish(
        OtelLog {
            time_ms: time,
            event_name: String::new(),
            body,
            attributes,
            resource: resource.clone(),
        },
        event_name,
    ))
}

fn json_attributes(list: Option<&Value>, depth: usize) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    let Some(list) = list.and_then(Value::as_array) else {
        return Ok(out);
    };
    for kv in list.iter().take(MAX_ATTRIBUTES) {
        let Some(key) = kv.get("key").and_then(Value::as_str) else {
            continue;
        };
        let value = match kv.get("value") {
            Some(v) => json_any(v, depth + 1)?,
            None => Value::Null,
        };
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

/// An OTLP/JSON `AnyValue` as plain JSON.
fn json_any(value: &Value, depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err("values nested too deep".into());
    }
    let Some(obj) = value.as_object() else {
        return Ok(Value::Null);
    };
    if let Some(v) = field(value, &["stringValue", "string_value"]) {
        return Ok(v
            .as_str()
            .map(|s| Value::String(s.to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(v) = field(value, &["boolValue", "bool_value"]) {
        return Ok(v.as_bool().map(Value::Bool).unwrap_or(Value::Null));
    }
    if let Some(v) = field(value, &["intValue", "int_value"]) {
        let n = match v {
            Value::String(s) => s.trim().parse::<i64>().ok(),
            Value::Number(n) => n.as_i64(),
            _ => None,
        };
        return Ok(n.map(Value::from).unwrap_or(Value::Null));
    }
    if let Some(v) = field(value, &["doubleValue", "double_value"]) {
        return Ok(v
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null));
    }
    if let Some(v) = field(value, &["arrayValue", "array_value"]) {
        let mut items = Vec::new();
        for item in array(v, &["values"]).take(MAX_ATTRIBUTES) {
            items.push(json_any(item, depth + 1)?);
        }
        return Ok(Value::Array(items));
    }
    if let Some(v) = field(value, &["kvlistValue", "kvlist_value"]) {
        return Ok(Value::Object(json_attributes(v.get("values"), depth + 1)?));
    }
    if let Some(v) = field(value, &["bytesValue", "bytes_value"]) {
        // Bytes are never decoded or shown, as in the protobuf path.
        let len = v.as_str().map(|s| s.len() / 4 * 3).unwrap_or(0);
        return Ok(Value::String(format!("<{len} bytes>")));
    }
    let _ = obj;
    Ok(Value::Null)
}

// ---------------------------------------------------------------- protobuf

/// A protobuf wire reader over a byte slice. Every read checks the length
/// left first.
struct Wire<'a> {
    buf: &'a [u8],
    at: usize,
}

enum Field<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    /// Read past; no OTLP log field this decoder keeps is a fixed32.
    Fixed32,
}

impl<'a> Wire<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn done(&self) -> bool {
        self.at >= self.buf.len()
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut value: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = *self.buf.get(self.at).ok_or("truncated varint")?;
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("varint too long".into())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(len).ok_or("length overflow")?;
        let slice = self.buf.get(self.at..end).ok_or("truncated field")?;
        self.at = end;
        Ok(slice)
    }

    fn next(&mut self) -> Result<(u64, Field<'a>), String> {
        let key = self.varint()?;
        let number = key >> 3;
        let field = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => {
                let b = self.take(8)?;
                Field::Fixed64(u64::from_le_bytes(b.try_into().map_err(|_| "fixed64")?))
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| "length too large")?;
                Field::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Field::Fixed32
            }
            other => return Err(format!("unsupported wire type {other}")),
        };
        Ok((number, field))
    }
}

fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Decodes a protobuf `ExportLogsServiceRequest`.
pub fn decode_protobuf(body: &[u8]) -> Result<Decoded, String> {
    let mut out = Decoded::default();
    let mut wire = Wire::new(body);
    while !wire.done() {
        if let (1, Field::Bytes(resource_logs)) = wire.next()? {
            pb_resource_logs(resource_logs, &mut out)?;
        }
    }
    Ok(out)
}

fn pb_resource_logs(buf: &[u8], out: &mut Decoded) -> Result<(), String> {
    // The resource may come after its scope logs on the wire, so read it
    // first, then the records.
    let mut resource = Map::new();
    let mut scopes = Vec::new();
    let mut wire = Wire::new(buf);
    while !wire.done() {
        match wire.next()? {
            (1, Field::Bytes(r)) => resource = slim_resource(pb_resource(r)?),
            (2, Field::Bytes(s)) => scopes.push(s),
            _ => {}
        }
    }
    for scope in scopes {
        let mut wire = Wire::new(scope);
        while !wire.done() {
            if let (2, Field::Bytes(record)) = wire.next()? {
                if out.logs.len() >= MAX_RECORDS {
                    out.dropped = out.dropped.saturating_add(1);
                    continue;
                }
                out.logs.push(pb_record(record, &resource)?);
            }
        }
    }
    Ok(())
}

/// Longest `service.name` kept.
const MAX_SERVICE_CHARS: usize = 200;

/// The resource as each record keeps it: `service.name` alone, clipped.
/// Every record carries a copy, so a large resource times many records
/// must not multiply (security review S2: 4 MiB times 2,000 records).
fn slim_resource(resource: Map<String, Value>) -> Map<String, Value> {
    let mut slim = Map::new();
    if let Some(Value::String(service)) = resource.get("service.name") {
        slim.insert(
            "service.name".into(),
            Value::String(service.chars().take(MAX_SERVICE_CHARS).collect()),
        );
    }
    slim
}

fn pb_resource(buf: &[u8]) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    let mut wire = Wire::new(buf);
    while !wire.done() {
        if let (1, Field::Bytes(kv)) = wire.next()? {
            if out.len() < MAX_ATTRIBUTES {
                let (k, v) = pb_key_value(kv, 1)?;
                out.insert(k, v);
            }
        }
    }
    Ok(out)
}

fn pb_record(buf: &[u8], resource: &Map<String, Value>) -> Result<OtelLog, String> {
    let mut time = 0u64;
    let mut observed = 0u64;
    let mut body = None;
    let mut attributes = Map::new();
    let mut event_name = None;
    let mut wire = Wire::new(buf);
    while !wire.done() {
        match wire.next()? {
            (1, Field::Fixed64(t)) => time = t,
            (11, Field::Fixed64(t)) => observed = t,
            (5, Field::Bytes(b)) => body = Some(pb_any(b, 1)?),
            (6, Field::Bytes(kv)) => {
                if attributes.len() < MAX_ATTRIBUTES {
                    let (k, v) = pb_key_value(kv, 1)?;
                    attributes.insert(k, v);
                }
            }
            (12, Field::Bytes(name)) => event_name = Some(utf8(name)),
            _ => {}
        }
    }
    let time = if time > 0 { time } else { observed };
    Ok(finish(
        OtelLog {
            time_ms: if time > 0 { nanos_to_ms(time) } else { 0 },
            event_name: String::new(),
            body,
            attributes,
            resource: resource.clone(),
        },
        event_name,
    ))
}

fn pb_key_value(buf: &[u8], depth: usize) -> Result<(String, Value), String> {
    let mut key = String::new();
    let mut value = Value::Null;
    let mut wire = Wire::new(buf);
    while !wire.done() {
        match wire.next()? {
            (1, Field::Bytes(k)) => key = utf8(k),
            (2, Field::Bytes(v)) => value = pb_any(v, depth + 1)?,
            _ => {}
        }
    }
    Ok((key, value))
}

fn pb_any(buf: &[u8], depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err("values nested too deep".into());
    }
    let mut value = Value::Null;
    let mut wire = Wire::new(buf);
    while !wire.done() {
        value = match wire.next()? {
            (1, Field::Bytes(s)) => Value::String(utf8(s)),
            (2, Field::Varint(b)) => Value::Bool(b != 0),
            // int64 on the wire is the two's complement in a varint.
            (3, Field::Varint(n)) => Value::from(n as i64),
            (4, Field::Fixed64(bits)) => serde_json::Number::from_f64(f64::from_bits(bits))
                .map(Value::Number)
                .unwrap_or(Value::Null),
            (5, Field::Bytes(list)) => {
                let mut items = Vec::new();
                let mut inner = Wire::new(list);
                while !inner.done() {
                    if let (1, Field::Bytes(item)) = inner.next()? {
                        if items.len() < MAX_ATTRIBUTES {
                            items.push(pb_any(item, depth + 1)?);
                        }
                    }
                }
                Value::Array(items)
            }
            (6, Field::Bytes(list)) => {
                let mut map = Map::new();
                let mut inner = Wire::new(list);
                while !inner.done() {
                    if let (1, Field::Bytes(kv)) = inner.next()? {
                        if map.len() < MAX_ATTRIBUTES {
                            let (k, v) = pb_key_value(kv, depth + 1)?;
                            map.insert(k, v);
                        }
                    }
                }
                Value::Object(map)
            }
            // Bytes are never decoded or shown; their length is kept.
            (7, Field::Bytes(b)) => Value::String(format!("<{} bytes>", b.len())),
            _ => value,
        };
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) mod encode {
    //! A small protobuf writer, for the tests only.

    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    pub fn bytes(field: u64, data: &[u8], out: &mut Vec<u8>) {
        varint((field << 3) | 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }

    pub fn fixed64(field: u64, v: u64, out: &mut Vec<u8>) {
        varint((field << 3) | 1, out);
        out.extend_from_slice(&v.to_le_bytes());
    }

    pub fn any_string(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        bytes(1, s.as_bytes(), &mut out);
        out
    }

    pub fn any_int(n: i64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(3 << 3, &mut out);
        varint(n as u64, &mut out);
        out
    }

    pub fn key_value(key: &str, any: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes(1, key.as_bytes(), &mut out);
        bytes(2, any, &mut out);
        out
    }

    /// A request with one resource and one record per (time, attributes).
    pub fn request(resource: &[Vec<u8>], records: &[(u64, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut res = Vec::new();
        for kv in resource {
            bytes(1, kv, &mut res);
        }
        let mut scope = Vec::new();
        for (time, attrs) in records {
            let mut rec = Vec::new();
            fixed64(1, *time, &mut rec);
            for kv in attrs {
                bytes(6, kv, &mut rec);
            }
            bytes(2, &rec, &mut scope);
        }
        let mut rl = Vec::new();
        // Scope logs before the resource, as a writer may order them.
        bytes(2, &scope, &mut rl);
        bytes(1, &res, &mut rl);
        let mut req = Vec::new();
        bytes(1, &rl, &mut req);
        req
    }
}

#[cfg(test)]
mod tests {
    use super::encode::*;
    use super::*;
    use serde_json::json;

    #[test]
    fn json_request_decodes_with_event_name_and_resource() {
        let body = json!({
            "resourceLogs": [{
                "resource": { "attributes": [
                    { "key": "service.name", "value": { "stringValue": "claude-code" } }
                ]},
                "scopeLogs": [{ "logRecords": [{
                    "timeUnixNano": "1760000000123000000",
                    "body": { "stringValue": "claude_code.tool_result" },
                    "attributes": [
                        { "key": "event.name", "value": { "stringValue": "tool_result" } },
                        { "key": "tool_name", "value": { "stringValue": "Bash" } },
                        { "key": "duration_ms", "value": { "intValue": "120" } },
                        { "key": "success", "value": { "boolValue": true } },
                        { "key": "cost", "value": { "doubleValue": 0.5 } },
                        { "key": "list", "value": { "arrayValue": { "values": [ { "stringValue": "a" } ] } } },
                        { "key": "kv", "value": { "kvlistValue": { "values": [ { "key": "x", "value": { "intValue": 2 } } ] } } }
                    ]
                }]}]
            }]
        });
        let decoded = decode_json(body.to_string().as_bytes()).unwrap();
        assert_eq!(decoded.logs.len(), 1);
        let log = &decoded.logs[0];
        assert_eq!(log.time_ms, 1_760_000_000_123);
        assert_eq!(log.event_name, "tool_result");
        assert_eq!(log.attributes["tool_name"], "Bash");
        assert_eq!(log.attributes["duration_ms"], 120);
        assert_eq!(log.attributes["success"], true);
        assert_eq!(log.attributes["cost"], 0.5);
        assert_eq!(log.attributes["list"], json!(["a"]));
        assert_eq!(log.attributes["kv"], json!({ "x": 2 }));
        assert_eq!(log.resource["service.name"], "claude-code");
    }

    #[test]
    fn json_snake_case_and_body_name() {
        let body = json!({ "resource_logs": [{ "scope_logs": [{ "log_records": [{
            "observed_time_unix_nano": 2_000_000,
            "body": { "string_value": "codex.api_request" }
        }]}]}]});
        let decoded = decode_json(body.to_string().as_bytes()).unwrap();
        assert_eq!(decoded.logs[0].event_name, "codex.api_request");
        assert_eq!(decoded.logs[0].time_ms, 2);
    }

    #[test]
    fn json_rejects_garbage_and_deep_nesting() {
        assert!(decode_json(b"not json").is_err());
        let mut v = json!({ "stringValue": "x" });
        for _ in 0..40 {
            v = json!({ "arrayValue": { "values": [v] } });
        }
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{
            "attributes": [{ "key": "deep", "value": v }]
        }]}]}]});
        assert!(decode_json(body.to_string().as_bytes()).is_err());
    }

    #[test]
    fn json_caps_records() {
        let records: Vec<Value> = (0..MAX_RECORDS + 5).map(|_| json!({})).collect();
        let body = json!({ "resourceLogs": [{ "scopeLogs": [{ "logRecords": records }]}]});
        let decoded = decode_json(body.to_string().as_bytes()).unwrap();
        assert_eq!(decoded.logs.len(), MAX_RECORDS);
        assert_eq!(decoded.dropped, 5);
    }

    #[test]
    fn protobuf_request_decodes() {
        let req = request(
            &[key_value("service.name", &any_string("codex_app_server"))],
            &[(
                3_000_000_000,
                vec![
                    key_value("event.name", &any_string("codex.tool_result")),
                    key_value("tool_name", &any_string("shell")),
                    key_value("duration_ms", &any_int(-7)),
                ],
            )],
        );
        let decoded = decode_protobuf(&req).unwrap();
        assert_eq!(decoded.logs.len(), 1);
        let log = &decoded.logs[0];
        assert_eq!(log.time_ms, 3000);
        assert_eq!(log.event_name, "codex.tool_result");
        assert_eq!(log.attributes["tool_name"], "shell");
        assert_eq!(log.attributes["duration_ms"], -7);
        assert_eq!(log.resource["service.name"], "codex_app_server");
    }

    #[test]
    fn protobuf_refuses_truncation_and_huge_lengths_without_panic() {
        let req = request(&[], &[(1, vec![key_value("a", &any_string("b"))])]);
        for cut in 0..req.len() {
            // Every prefix either decodes or errors; none panics.
            let _ = decode_protobuf(&req[..cut]);
        }
        // A length near u64::MAX.
        let mut evil = Vec::new();
        varint((1 << 3) | 2, &mut evil);
        varint(u64::MAX, &mut evil);
        assert!(decode_protobuf(&evil).is_err());
        // A group wire type.
        assert!(decode_protobuf(&[0x0b]).is_err());
        // An over-long varint.
        assert!(decode_protobuf(&[
            0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01
        ])
        .is_err());
    }

    #[test]
    fn protobuf_refuses_deep_nesting() {
        let mut any = any_string("x");
        for _ in 0..40 {
            let mut list = Vec::new();
            bytes(1, &any, &mut list);
            let mut wrapped = Vec::new();
            bytes(5, &list, &mut wrapped);
            any = wrapped;
        }
        let req = request(&[], &[(1, vec![key_value("deep", &any)])]);
        assert!(decode_protobuf(&req).is_err());
    }

    #[test]
    fn protobuf_bytes_are_never_kept() {
        let mut any = Vec::new();
        bytes(7, b"secret", &mut any);
        let req = request(&[], &[(1, vec![key_value("b", &any)])]);
        let decoded = decode_protobuf(&req).unwrap();
        assert_eq!(decoded.logs[0].attributes["b"], "<6 bytes>");
    }

    #[test]
    fn a_large_resource_is_not_copied_into_every_record() {
        // Security review S2: one big resource attribute and many records.
        let big = "x".repeat(1024 * 1024);
        let records: Vec<_> = (0..MAX_RECORDS).map(|_| json!({})).collect();
        let body = json!({ "resourceLogs": [{
            "resource": { "attributes": [
                { "key": "service.name", "value": { "stringValue": "claude-code" } },
                { "key": "huge", "value": { "stringValue": big } }
            ]},
            "scopeLogs": [{ "logRecords": records }]
        }]});
        let decoded = decode_json(&serde_json::to_vec(&body).unwrap()).unwrap();
        assert_eq!(decoded.logs.len(), MAX_RECORDS);
        for log in &decoded.logs {
            assert_eq!(log.resource.len(), 1);
            assert_eq!(log.resource["service.name"], "claude-code");
        }
        // A long service name is clipped.
        let long = json!({ "resourceLogs": [{
            "resource": { "attributes": [
                { "key": "service.name", "value": { "stringValue": "s".repeat(5000) } }
            ]},
            "scopeLogs": [{ "logRecords": [{}] }]
        }]});
        let decoded = decode_json(&serde_json::to_vec(&long).unwrap()).unwrap();
        assert_eq!(
            decoded.logs[0].resource["service.name"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_SERVICE_CHARS
        );
    }
}

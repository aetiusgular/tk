//! MonoCode's side of the helper's socket (see the helper's `server.rs` for
//! the protocol). One connection per helper run; replies are matched to
//! requests by id, and events go to a callback.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

/// The largest line from the helper: a tool result with a screenshot.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the helper is launched on macOS only")
)]
pub const MAX_LINE: usize = 64 * 1024 * 1024;

pub type EventSink = Box<dyn Fn(Value) + Send + Sync>;

pub struct HelperClient {
    writer: Mutex<Box<dyn Write + Send>>,
    waiting: Arc<Mutex<HashMap<u64, Sender<Value>>>>,
    next_id: AtomicU64,
    open: Arc<AtomicBool>,
}

impl HelperClient {
    /// Takes a connected stream's two halves. `on_event` gets each event line
    /// (`record`, `exited`) and, once, `{"event":"closed"}`.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the helper is launched on macOS only")
    )]
    pub fn new(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        on_event: EventSink,
    ) -> Self {
        let waiting: Arc<Mutex<HashMap<u64, Sender<Value>>>> = Arc::default();
        let open = Arc::new(AtomicBool::new(true));
        {
            let waiting = waiting.clone();
            let open = open.clone();
            thread::spawn(move || {
                let mut reader = BufReader::new(reader);
                while let Some(line) = read_line(&mut reader) {
                    let Ok(message) = serde_json::from_slice::<Value>(&line) else {
                        continue;
                    };
                    if message.get("event").is_some() {
                        on_event(message);
                        continue;
                    }
                    let Some(id) = message.get("id").and_then(Value::as_u64) else {
                        continue;
                    };
                    let sender = waiting.lock().ok().and_then(|mut map| map.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(message);
                    }
                }
                open.store(false, Ordering::SeqCst);
                // Every request still waiting fails now, not at its timeout.
                if let Ok(mut map) = waiting.lock() {
                    map.clear();
                }
                on_event(json!({"event": "closed"}));
            });
        }
        Self {
            writer: Mutex::new(Box::new(writer)),
            waiting,
            next_id: AtomicU64::new(1),
            open,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// One request. The reply's fields on success, its error text otherwise.
    pub fn request(&self, op: &str, mut body: Value, timeout: Duration) -> Result<Value, String> {
        if !self.is_open() {
            return Err("The computer-use helper is not running.".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.waiting
            .lock()
            .map_err(|_| "helper state poisoned")?
            .insert(id, tx);
        if let Some(map) = body.as_object_mut() {
            map.insert("id".into(), json!(id));
            map.insert("op".into(), json!(op));
        }
        let mut line = body.to_string();
        line.push('\n');
        let written = self
            .writer
            .lock()
            .map_err(|_| "helper state poisoned".to_string())
            .and_then(|mut writer| {
                writer
                    .write_all(line.as_bytes())
                    .and_then(|_| writer.flush())
                    .map_err(|e| format!("The computer-use helper closed: {e}"))
            });
        if let Err(error) = written {
            if let Ok(mut map) = self.waiting.lock() {
                map.remove(&id);
            }
            return Err(error);
        }
        let reply = rx.recv_timeout(timeout).map_err(|_| {
            if let Ok(mut map) = self.waiting.lock() {
                map.remove(&id);
            }
            if self.is_open() {
                format!("The computer-use helper did not answer {op} in time.")
            } else {
                "The computer-use helper stopped.".to_string()
            }
        })?;
        if reply.get("ok") == Some(&Value::Bool(true)) {
            Ok(reply)
        } else {
            let error = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("The computer-use helper refused the request.");
            Err(error.chars().take(500).collect())
        }
    }
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the helper is launched on macOS only")
)]
fn read_line(reader: &mut BufReader<impl Read>) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().ok()?;
        if available.is_empty() {
            return (!line.is_empty()).then_some(line);
        }
        let (chunk, used, done) = match available.iter().position(|b| *b == b'\n') {
            Some(at) => (&available[..at], at + 1, true),
            None => (available, available.len(), false),
        };
        if line.len() + chunk.len() > MAX_LINE {
            // A line too long ends the connection: the stream is out of step.
            return None;
        }
        line.extend_from_slice(chunk);
        reader.consume(used);
        if done {
            return Some(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// A fake helper: answers `hello` and `call`, refuses the rest, sends one
    /// event first.
    fn fake() -> (HelperClient, Arc<Mutex<Vec<Value>>>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut writer = theirs.try_clone().unwrap();
            writeln!(
                writer,
                "{}",
                json!({"event":"record","runtime":"r","action":{"tool":"click"}})
            )
            .unwrap();
            let reader = BufReader::new(theirs);
            for line in reader.lines() {
                let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
                let id = request["id"].clone();
                let reply = match request["op"].as_str() {
                    Some("hello") => json!({"id": id, "ok": true, "protocol": 1}),
                    Some("call") => json!({"id": id, "ok": true, "result": {"content": []}}),
                    Some("slow") => continue,
                    _ => json!({"id": id, "ok": false, "error": "unknown op"}),
                };
                writeln!(writer, "{reply}").unwrap();
            }
        });
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let sink = events.clone();
        let client = HelperClient::new(
            ours.try_clone().unwrap(),
            ours,
            Box::new(move |event| sink.lock().unwrap().push(event)),
        );
        (client, events)
    }

    #[test]
    fn matches_replies_and_passes_events_on() {
        let (client, events) = fake();
        let hello = client
            .request("hello", json!({"version": 1}), Duration::from_secs(2))
            .unwrap();
        assert_eq!(hello["protocol"], 1);
        let call = client
            .request("call", json!({"runtime": "r"}), Duration::from_secs(2))
            .unwrap();
        assert!(call["result"].is_object());
        let error = client
            .request("nope", json!({}), Duration::from_secs(2))
            .unwrap_err();
        assert_eq!(error, "unknown op");
        let error = client
            .request("slow", json!({}), Duration::from_millis(100))
            .unwrap_err();
        assert!(error.contains("in time"));
        assert_eq!(events.lock().unwrap()[0]["event"], "record");
    }

    #[test]
    fn a_closed_helper_fails_at_once() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let events: Arc<Mutex<Vec<Value>>> = Arc::default();
        let sink = events.clone();
        let client = HelperClient::new(
            ours.try_clone().unwrap(),
            ours,
            Box::new(move |event| sink.lock().unwrap().push(event)),
        );
        drop(theirs);
        for _ in 0..100 {
            if !client.is_open() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!client.is_open());
        assert!(client
            .request("hello", json!({}), Duration::from_secs(5))
            .is_err());
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "closed"));
    }
}

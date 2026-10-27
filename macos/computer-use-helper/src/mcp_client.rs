//! A small MCP client over the driver's stdio.

use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::lines::read_line_capped;

/// The largest line taken from the driver: a window screenshot, base64.
pub const MAX_LINE: usize = 64 * 1024 * 1024;
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// How often a waiting request looks at its cancel flag.
const CANCEL_CHECK: Duration = Duration::from_millis(25);

pub struct McpClient {
    writer: Box<dyn Write + Send>,
    incoming: Receiver<Value>,
    next_id: u64,
    /// Set by Stop. A request waiting on the driver returns at once, since
    /// its answer may never come: anything Cua started can hold its stdout
    /// open, so no EOF ends the wait either (security review X9).
    cancel: Arc<AtomicBool>,
}

impl McpClient {
    pub fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static) -> Self {
        let (tx, incoming) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                match read_line_capped(&mut reader, MAX_LINE) {
                    Ok(Some(line)) => {
                        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                            if tx.send(value).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    // A line too long is dropped; its request times out.
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            writer: Box::new(writer),
            incoming,
            next_id: 1,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .and_then(|_| self.writer.flush())
            .map_err(|e| format!("The computer-use runtime closed: {e}"))
    }

    /// Closes the driver's stdin. Cua's direct loop then ends this
    /// transport's sessions and the process (`cua-driver/src/proxy.rs:66-84`).
    pub fn close(&mut self) {
        self.writer = Box::new(std::io::sink());
    }

    /// The flag that cancels this client's requests, for Stop to set
    /// without the lock a call holds.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// One request; the `result` of its response. Requests from the driver
    /// in between are refused, notifications are dropped.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err("The computer-use runtime was stopped.".into());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("The computer-use runtime was stopped.".into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let message = match self.incoming.recv_timeout(left.min(CANCEL_CHECK)) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) if left > CANCEL_CHECK => continue,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!(
                        "The computer-use runtime did not answer {method} in time."
                    ))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("The computer-use runtime ended.".into())
                }
            };
            let has_method = message.get("method").is_some();
            match (message.get("id"), has_method) {
                (Some(their), true) => {
                    let their = their.clone();
                    self.send(&json!({"jsonrpc":"2.0","id":their,"error":{"code":-32601,"message":"Not supported"}}))?;
                }
                (Some(their), false) if their.as_u64() == Some(id) => {
                    if let Some(error) = message.get("error") {
                        let text = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("error");
                        return Err(format!(
                            "The runtime refused {method}: {}",
                            text.chars().take(300).collect::<String>()
                        ));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                _ => {}
            }
        }
    }

    pub fn initialize(&mut self, timeout: Duration) -> Result<Value, String> {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "monocode-computer-use-helper", "version": env!("CARGO_PKG_VERSION")}
            }),
            timeout,
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// A fake MCP server on the other end of a socket pair.
    fn fake(script: impl Fn(&Value) -> Vec<Value> + Send + 'static) -> McpClient {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let reader = theirs.try_clone().unwrap();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            let mut writer = theirs;
            while let Ok(Some(line)) = read_line_capped(&mut reader, MAX_LINE) {
                let message: Value = serde_json::from_slice(&line).unwrap();
                for reply in script(&message) {
                    writeln!(writer, "{reply}").unwrap();
                }
            }
        });
        McpClient::new(ours.try_clone().unwrap(), ours)
    }

    #[test]
    fn initializes_and_matches_replies_by_id() {
        let mut client = fake(|message| {
            let Some(id) = message.get("id").cloned() else {
                return vec![];
            };
            vec![
                json!({"jsonrpc":"2.0","method":"notifications/message","params":{}}),
                json!({"jsonrpc":"2.0","id":"server-ask","method":"sampling/createMessage"}),
                json!({"jsonrpc":"2.0","id":999,"result":{"stray":true}}),
                json!({"jsonrpc":"2.0","id":id,"result":{"method":message["method"]}}),
            ]
        });
        client.initialize(Duration::from_secs(2)).unwrap();
        let result = client
            .request("tools/list", json!({}), Duration::from_secs(2))
            .unwrap();
        assert_eq!(result["method"], "tools/list");
    }

    #[test]
    fn errors_and_timeouts_are_plain_messages() {
        let mut client = fake(|message| match message.get("id") {
            Some(id) if message["method"] == "tools/call" => {
                vec![
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"permission_denied"}}),
                ]
            }
            _ => vec![],
        });
        let error = client
            .request("tools/call", json!({}), Duration::from_secs(2))
            .unwrap_err();
        assert!(error.contains("permission_denied"));
        let error = client
            .request("tools/list", json!({}), Duration::from_millis(150))
            .unwrap_err();
        assert!(error.contains("in time"));
    }

    #[test]
    fn a_cancelled_request_returns_at_once() {
        let mut client = fake(|_| vec![]);
        let cancel = client.canceller();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let error = client
            .request("tools/call", json!({}), Duration::from_secs(30))
            .unwrap_err();
        assert!(error.contains("stopped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
        setter.join().unwrap();
        assert!(client
            .request("tools/list", json!({}), Duration::from_secs(30))
            .unwrap_err()
            .contains("stopped"));
    }
}

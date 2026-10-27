//! The loopback endpoint each chat's computer-use server connects to.
//!
//! The harness starts `<MonoCode> computer-use-mcp` as a stdio MCP server
//! (`bridge_cli.rs`). That process reads the endpoint and the chat's token
//! from the environment MonoCode gave the harness, connects here, sends the
//! token as its first line, and then relays MCP lines both ways. The token
//! picks the chat; a wrong token gets one error line and the connection ends.
//! A chat with a token that is off (a spawn before the user's switch-on,
//! 4.12) connects and is offered nothing until it is on. Stop keeps the
//! connection (4.7). Switching off replaces the token, so a connection made
//! with the old one ends at its next line or poll (security review X2).

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::core::Core;
use super::facade::Facade;

/// The longest first line: `{"token":"..."}`.
const MAX_HELLO: usize = 4096;
/// The longest MCP line from the harness. Tool calls are small.
const MAX_REQUEST: usize = 1024 * 1024;
const HELLO_DEADLINE: Duration = Duration::from_secs(2);
/// A reader that stops reading cannot hold a connection slot forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 32;
/// How often an idle connection checks whether the tool list changed.
const POLL: Duration = Duration::from_millis(750);

pub fn start(core: Arc<Core>) -> std::io::Result<std::net::SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let live = Arc::new(AtomicUsize::new(0));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if !stream
                .peer_addr()
                .map(|a| a.ip().is_loopback())
                .unwrap_or(false)
            {
                continue;
            }
            if live.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                live.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let core = core.clone();
            let live = live.clone();
            thread::spawn(move || {
                serve(&core, stream);
                live.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok(address)
}

enum Line {
    Text(Vec<u8>),
    TooLong,
    Idle,
    Closed,
}

/// Reads one line, waiting at most until the stream's read timeout. Bytes of
/// a line not finished stay in `pending` for the next call.
fn next_line(reader: &mut BufReader<TcpStream>, pending: &mut Vec<u8>, max: usize) -> Line {
    loop {
        let available = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Line::Idle
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return Line::Closed,
        };
        if available.is_empty() {
            return Line::Closed;
        }
        match available.iter().position(|b| *b == b'\n') {
            Some(at) => {
                let too_long = pending.len() + at > max;
                if !too_long {
                    pending.extend_from_slice(&available[..at]);
                }
                reader.consume(at + 1);
                let line = std::mem::take(pending);
                return if too_long {
                    Line::TooLong
                } else {
                    Line::Text(line)
                };
            }
            None => {
                let len = available.len();
                if pending.len() + len > max {
                    pending.clear();
                    reader.consume(len);
                    // Drop the rest of this line; its end resets the reader.
                    return Line::TooLong;
                }
                pending.extend_from_slice(available);
                reader.consume(len);
            }
        }
    }
}

fn write_line(stream: &mut TcpStream, value: &Value) -> bool {
    let mut line = value.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.flush())
        .is_ok()
}

fn serve(core: &Arc<Core>, stream: TcpStream) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let _ = writer.set_write_timeout(Some(WRITE_TIMEOUT));
    let _ = stream.set_read_timeout(Some(HELLO_DEADLINE));
    let mut reader = BufReader::new(stream);
    let mut pending = Vec::new();
    let started = Instant::now();
    let hello = loop {
        match next_line(&mut reader, &mut pending, MAX_HELLO) {
            Line::Text(line) => break line,
            Line::Idle if started.elapsed() < HELLO_DEADLINE => continue,
            _ => return,
        }
    };
    let token = serde_json::from_slice::<Value>(&hello)
        .ok()
        .and_then(|v| v.get("token").and_then(Value::as_str).map(str::to_string));
    let token = token.unwrap_or_default();
    let Some(chat) = core.chat_for_token(&token) else {
        let _ = write_line(
            &mut writer,
            &json!({"monocode": "refused", "message": "Computer use is not on for this chat."}),
        );
        return;
    };
    if !write_line(&mut writer, &json!({"monocode": "ok"})) {
        return;
    }
    let _ = reader.get_ref().set_read_timeout(Some(POLL));
    let mut facade = Facade::new(chat.clone());
    loop {
        let line = next_line(&mut reader, &mut pending, MAX_REQUEST);
        // Switching the chat off replaces its token (security review X2):
        // a connection made with the old one ends here.
        if core.chat_for_token(&token).as_deref() != Some(chat.as_str()) {
            return;
        }
        match line {
            Line::Text(line) => {
                let reply = match serde_json::from_slice::<Value>(&line) {
                    Ok(message) => facade.handle(core.as_ref(), &message),
                    Err(_) => Some(
                        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                    ),
                };
                if let Some(reply) = reply {
                    if !write_line(&mut writer, &reply) {
                        return;
                    }
                }
            }
            Line::TooLong => {
                let reply = json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Request too large"}});
                if !write_line(&mut writer, &reply) {
                    return;
                }
            }
            Line::Idle => {
                if let Some(note) = facade.list_changed(core.as_ref()) {
                    if !write_line(&mut writer, &note) {
                        return;
                    }
                }
            }
            Line::Closed => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::core::tests::{core_with, FakeLauncher};
    use super::*;

    fn connect(address: std::net::SocketAddr, token: &str) -> (TcpStream, BufReader<TcpStream>) {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{}", json!({"token": token})).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (stream, reader)
    }

    fn read(reader: &mut BufReader<TcpStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (core, _, _) = core_with(FakeLauncher::new());
        let address = start(core.clone()).unwrap();
        let (_stream, mut reader) = connect(address, "not-a-token");
        assert_eq!(read(&mut reader)["monocode"], "refused");
        let mut rest = String::new();
        assert_eq!(
            reader.read_line(&mut rest).unwrap(),
            0,
            "closed after refusing"
        );
    }

    #[test]
    fn a_chat_token_reaches_its_chat_and_hears_of_new_grants() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        let address = start(core.clone()).unwrap();
        let (mut stream, mut reader) = connect(address, &core.token("c1").unwrap());
        assert_eq!(read(&mut reader)["monocode"], "ok");
        writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})
        )
        .unwrap();
        assert!(read(&mut reader)["result"]["serverInfo"].is_object());
        writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        )
        .unwrap();
        assert_eq!(
            read(&mut reader)["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        assert_eq!(
            read(&mut reader)["method"],
            "notifications/tools/list_changed"
        );
        writeln!(stream, "{}", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"click","arguments":{}}})).unwrap();
        assert_eq!(read(&mut reader)["result"]["content"][0]["text"], "done");
        writeln!(stream, "not json").unwrap();
        assert_eq!(read(&mut reader)["error"]["code"], -32700);
    }

    fn send(stream: &mut TcpStream, message: Value) {
        writeln!(stream, "{message}").unwrap();
    }

    /// The next reply with an id, skipping notifications.
    fn reply(reader: &mut BufReader<TcpStream>) -> Value {
        loop {
            let message = read(reader);
            if message.get("id").is_some() {
                return message;
            }
        }
    }

    fn ready(core: &Arc<Core>, chat: &str) -> (TcpStream, BufReader<TcpStream>) {
        let address = start(core.clone()).unwrap();
        let (mut stream, mut reader) = connect(address, &core.token(chat).unwrap());
        assert_eq!(read(&mut reader)["monocode"], "ok");
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        );
        assert!(read(&mut reader)["result"]["serverInfo"].is_object());
        (stream, reader)
    }

    #[test]
    fn switching_off_closes_the_connection_and_refuses_the_old_token() {
        // Security review X2: off replaces the chat's token, so a token an
        // agent's shell saw ends with the switch, and a connection made with
        // it ends too. (Before X2 the connection stayed and offered nothing.)
        // The harness restarts without the server on off; its relay, if one
        // is left, dials again, is refused and answers "off" itself.
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        let old = core.token("c1").unwrap();
        let (mut stream, mut reader) = ready(&core, "c1");
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        assert_eq!(
            read(&mut reader)["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        core.set_chat("c1", false);
        let mut rest = String::new();
        assert_eq!(
            reader.read_line(&mut rest).unwrap_or(0),
            0,
            "closed after the switch-off, got {rest:?}"
        );
        let address = start(core.clone()).unwrap();
        let (_stream, mut reader) = connect(address, &old);
        assert_eq!(read(&mut reader)["monocode"], "refused");

        // On again: the new token (the restarted harness's) gets the tools.
        core.set_chat("c1", true);
        assert_ne!(core.token("c1").unwrap(), old);
        let (mut stream, mut reader) = ready(&core, "c1");
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}),
        );
        assert_eq!(
            read(&mut reader)["result"]["tools"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn stop_keeps_the_connection_and_the_next_action_asks_for_access_again() {
        // 4.7's pass line.
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        let (mut stream, mut reader) = ready(&core, "c1");
        core.stop("c1");
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"click","arguments":{}}}),
        );
        let next = reply(&mut reader);
        assert_eq!(next["result"]["isError"], true);
        assert_eq!(
            next["result"]["content"][0]["text"],
            "No app is approved for this chat. Call request_app_access first."
        );
    }

    #[test]
    fn a_chat_with_a_token_but_off_connects_and_hears_when_it_is_switched_on() {
        // 4.12 and X1: a spawn before the switch-on gets a token for a chat
        // that is still off. Its relay connects and is offered nothing.
        let (core, _, _) = core_with(FakeLauncher::new());
        let token = core.mint_token("c1").unwrap();
        assert!(!core.is_on("c1"));
        let address = start(core.clone()).unwrap();
        let (mut stream, mut reader) = connect(address, &token);
        assert_eq!(read(&mut reader)["monocode"], "ok");
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        );
        read(&mut reader);
        send(
            &mut stream,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        assert_eq!(read(&mut reader)["result"]["tools"], json!([]));
        core.set_chat("c1", true);
        assert_eq!(
            read(&mut reader)["method"],
            "notifications/tools/list_changed"
        );
    }

    #[test]
    fn a_silent_client_is_dropped_at_the_hello_deadline() {
        let (core, _, _) = core_with(FakeLauncher::new());
        let address = start(core).unwrap();
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let started = Instant::now();
        let mut rest = String::new();
        assert_eq!(reader.read_line(&mut rest).unwrap_or(0), 0);
        assert!(started.elapsed() < Duration::from_secs(9));
    }
}

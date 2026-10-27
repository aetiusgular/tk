//! The Unix socket server. STUB (session 0): binds the path with mode 0600,
//! answers `hello` and refuses every other op with `not implemented`. Lane B
//! fills it (contract sections 1, 2, 4): the token check, `event`, `tail`,
//! `report`, `approve`, `approver`, `token.mint`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

use crate::protocol::{DaemonInfo, Reply, Request, Scope, PROTOCOL_VERSION};

/// Binds `path`, replacing a stale socket file, and sets it to 0600. The
/// parent folder is created 0700 when missing.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serves one connection: reads lines, answers each. The stub accepts any
/// `hello` whose token has the right shape and refuses the rest.
pub fn serve_one(stream: UnixStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();
    let mut greeted = false;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let reply = match serde_json::from_str::<Request>(line.trim_end()) {
            Err(e) => error(format!("bad line: {e}")),
            Ok(Request::Hello { token, .. }) if !greeted => {
                if tk_core::token::well_formed(&token) {
                    greeted = true;
                    Reply {
                        scope: Some(Scope::Daemon),
                        daemon: Some(DaemonInfo {
                            version: env!("CARGO_PKG_VERSION").to_string(),
                            pid: std::process::id(),
                            protocol: PROTOCOL_VERSION,
                        }),
                        ..ok()
                    }
                } else {
                    let r = error("bad token".to_string());
                    write_line(&mut writer, &r)?;
                    return Ok(());
                }
            }
            Ok(_) if !greeted => {
                let r = error("hello first".to_string());
                write_line(&mut writer, &r)?;
                return Ok(());
            }
            Ok(_) => error("not implemented (session 0 stub; lane B)".to_string()),
        };
        write_line(&mut writer, &reply)?;
    }
}

fn ok() -> Reply {
    Reply {
        ok: true,
        error: None,
        scope: None,
        chat: None,
        daemon: None,
        id: None,
        token: None,
        answer: None,
        by: None,
        report: None,
    }
}

fn error(message: String) -> Reply {
    Reply {
        ok: false,
        error: Some(message),
        ..ok()
    }
}

fn write_line(w: &mut UnixStream, reply: &Reply) -> std::io::Result<()> {
    let mut text = serde_json::to_string(reply).map_err(std::io::Error::other)?;
    text.push('\n');
    w.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;

    fn temp_socket() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tk-sock-test-{}-{}",
            std::process::id(),
            tk_core::token::mint().unwrap()
        ));
        dir.join("tk/daemon.sock")
    }

    #[test]
    fn bind_sets_0600_and_a_0700_parent() {
        let path = temp_socket();
        let _l = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let parent = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent, 0o700);
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn hello_then_other_ops_get_a_reply_each_and_bad_token_closes() {
        let path = temp_socket();
        let listener = bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                serve_one(stream.unwrap()).unwrap();
            }
        });
        // good token
        let client = UnixStream::connect(&path).unwrap();
        let mut w = client.try_clone().unwrap();
        let mut r = BufReader::new(client);
        let tok = tk_core::token::mint().unwrap();
        writeln!(w, r#"{{"op":"hello","token":"{tok}","client":"test"}}"#).unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.scope, Some(Scope::Daemon));
        writeln!(w, r#"{{"op":"approver"}}"#).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok);
        assert!(reply.error.unwrap().contains("not implemented"));
        drop(w);
        drop(r);
        // bad token
        let client = UnixStream::connect(&path).unwrap();
        let mut w = client.try_clone().unwrap();
        let mut r = BufReader::new(client);
        writeln!(w, r#"{{"op":"hello","token":"short","client":"test"}}"#).unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert_eq!(reply.error.as_deref(), Some("bad token"));
        line.clear();
        assert_eq!(
            r.read_line(&mut line).unwrap(),
            0,
            "the daemon closes after a bad token"
        );
        server.join().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
    }
}

//! The socket client against the contract: lane B's `socket_path` and
//! request types (`crates/tk-daemon/src/protocol.rs`), and a scripted fake
//! daemon for the replies, timeouts and refusals.

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use tk_audit_linux::client::{self, fake, Client, ClientError};
use tk_core::event::{Event, Source};

#[test]
fn socket_path_is_lane_bs() {
    assert_eq!(client::socket_path(), tk_daemon::socket_path());
}

fn run_start() -> Event {
    Event {
        id: "run-1".into(),
        at: 0,
        chat_id: "chat-b".into(),
        harness: "pi".into(),
        source: Source::Run,
        kind: "run.start".into(),
        summary: "pi under sbx".into(),
        detail: serde_json::json!({"boundary": "sbx", "argv": ["sbx", "run"], "host": "box"}),
        pid: Some(4242),
        approval_id: None,
    }
}

#[test]
fn hello_mint_and_event_as_the_recorded_exchange() {
    let f = fake::start("mint", fake::Mode::Normal);
    let token = client::read_daemon_token(&f.dir).unwrap();
    let (mut c, hello) = Client::connect(f.path(), &token, "tk run").unwrap();
    assert_eq!(hello.scope, "daemon");
    assert_eq!(c.mint("chat-b", "pi").unwrap(), fake::CHAT_TOKEN);
    assert_eq!(c.post(&run_start()).unwrap(), "run-1");
    // Every line the client sent is a request lane B's types accept.
    let lines = f.lines();
    assert_eq!(lines.len(), 3);
    for l in &lines {
        serde_json::from_value::<tk_daemon::protocol::Request>(l.clone())
            .unwrap_or_else(|e| panic!("{e}: {l}"));
    }
    assert_eq!(lines[1]["op"], "token.mint");
    assert_eq!(
        client::read_otlp_endpoint(&f.dir).as_deref(),
        Some("http://127.0.0.1:4318")
    );
}

#[test]
fn a_bad_token_is_refused_and_no_daemon_is_not_running() {
    let f = fake::start("bad", fake::Mode::Normal);
    match Client::connect(f.path(), &"3".repeat(64), "x") {
        Err(ClientError::Refused(e)) => assert_eq!(e, "bad token"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("accepted"),
    }
    let gone = f.dir.join("nothing.sock");
    assert!(matches!(
        Client::connect(&gone, fake::DAEMON_TOKEN, "x"),
        Err(ClientError::NotRunning(_))
    ));
}

#[test]
fn a_silent_daemon_times_out_and_never_hangs() {
    let f = fake::start("silent", fake::Mode::Silent);
    let t = Instant::now();
    let r = Client::connect_timeout(
        f.path(),
        fake::DAEMON_TOKEN,
        "x",
        Duration::from_millis(300),
    );
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
}

#[test]
fn a_refused_mint_is_an_error() {
    let f = fake::start("refuse", fake::Mode::RefuseMint);
    let (mut c, _) = Client::connect(f.path(), fake::DAEMON_TOKEN, "x").unwrap();
    assert!(matches!(c.mint("c", "pi"), Err(ClientError::Refused(_))));
}

#[test]
fn the_token_file_must_be_ours_and_private() {
    let f = fake::start("perm", fake::Mode::Normal);
    let tok = f.dir.join("daemon.token");
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        client::read_daemon_token(&f.dir),
        Err(ClientError::Token(_))
    ));
    std::fs::set_permissions(&tok, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&tok, "not a token").unwrap();
    assert!(matches!(
        client::read_daemon_token(&f.dir),
        Err(ClientError::Token(_))
    ));
    // A link to a token elsewhere is not the daemon's file.
    let real = f.dir.join("real.token");
    std::fs::write(&real, fake::DAEMON_TOKEN).unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::remove_file(&tok).unwrap();
    std::os::unix::fs::symlink(&real, &tok).unwrap();
    assert!(matches!(
        client::read_daemon_token(&f.dir),
        Err(ClientError::Token(_))
    ));
}

#[test]
fn a_remote_otlp_endpoint_is_ignored() {
    let f = fake::start("otlp", fake::Mode::Normal);
    std::fs::write(f.dir.join("otlp.endpoint"), "http://203.0.113.5:4318\n").unwrap();
    assert_eq!(client::read_otlp_endpoint(&f.dir), None);
}

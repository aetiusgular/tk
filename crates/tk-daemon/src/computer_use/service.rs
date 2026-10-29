//! The daemon's side of computer use (lane K, toolkit plan T1.5): what the
//! fork's Tauri layer did, done with the daemon's own channels.
//!
//! - **Which chat.** The bridge takes a tk chat token (`relay.rs`) and asks
//!   `place`: a live chat token minted by `tk run` switches computer use on
//!   for its chat and gives the connection the chat's bridge token. The
//!   daemon's own token, and a token revoked at the chat's `run.stop`, are
//!   refused. The fork switched a chat on from the window; in tk, starting
//!   the harness under `tk run` with `tk-daemon cu-mcp` in its config is the
//!   switch, and every app still needs its own yes.
//! - **The user's yes.** A card the agent opens (`request_app_access`)
//!   becomes an `approve` request on the daemon, in the chat's name, with
//!   the tool `computer-use/grant-app`; the first approver's answer goes to
//!   the card. No approver connected, or none in time: no (case 63). This
//!   inherits the approver's open problem (integration w1 finding S2):
//!   under `--boundary none` the agent can read `daemon.token` and answer
//!   its own asks. Wave 2 lane J and integration w2 own that fix.
//! - **The end of a chat.** Every `WATCH` the service checks each placed
//!   chat's token. Once the daemon refuses it (the chat's `run.stop`, or a
//!   daemon restart), the chat is switched off: its grants, cards and
//!   runtime end, and its bridge token is replaced, so its connection closes.
//! - **Stop.** `stop` and `stop_all` are here for an op or a `tk cu stop`
//!   to call; neither exists yet (a request for integration). The helper
//!   ends every runtime when the daemon exits.
//!
//! Nothing here makes an outbound connection (rules, section 5, item 8): the
//! bridge listens on 127.0.0.1 and the helper is a local socket.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde_json::Value;

use super::bridge::{self, Resolver};
use super::core::{Core, Notice, PendingView, Status, StopReport};

/// How often placed chats' tokens are checked.
pub const WATCH: Duration = Duration::from_secs(2);
/// How long an app card waits on the approver. The card itself lives 15
/// minutes in the engine; a model asks again while it is open.
pub const ASK_SECONDS: u32 = 120;
/// The tool name approvers see for a computer-use grant.
pub const GRANT_TOOL: &str = "computer-use/grant-app";
/// Chats placed at once. Beyond this a new chat is refused, not queued.
const MAX_CHATS: usize = 256;

/// What the service needs from the daemon. `DaemonHost` is the real one;
/// the tests use a fake.
pub trait Host: Send + Sync {
    /// The chat and harness a live tk chat token belongs to. `None` for any
    /// other token, the daemon's own included.
    fn chat_of(&self, token: &str) -> Option<(String, String)>;
    /// Asks the daemon's approvers in the chat's name. True only on yes.
    fn ask(&self, chat: &str, harness: &str, id: &str, summary: &str) -> bool;
}

#[derive(Clone, Debug)]
struct Placed {
    harness: String,
    token: String,
}

pub struct Service {
    core: Arc<Core>,
    host: Box<dyn Host>,
    placed: Mutex<HashMap<String, Placed>>,
    /// Card ids already sent to the approver.
    asked: Mutex<HashSet<String>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

impl Service {
    /// Builds the service around an engine that tells `notices` of cards.
    /// `make_core` gets the notice sink to build the engine with.
    pub(crate) fn new(
        host: Box<dyn Host>,
        make_core: impl FnOnce(Box<dyn Fn(Notice) + Send + Sync>) -> Arc<Core>,
    ) -> Arc<Service> {
        let cell: Arc<OnceLock<Weak<Service>>> = Arc::new(OnceLock::new());
        let sink = cell.clone();
        let core = make_core(Box::new(move |notice| {
            if let Notice::Request { chat } = notice {
                if let Some(service) = sink.get().and_then(Weak::upgrade) {
                    // Off the engine's thread: an answer waits on a person.
                    std::thread::spawn(move || service.forward_cards(&chat));
                }
            }
        }));
        let service = Arc::new(Service {
            core,
            host,
            placed: Mutex::new(HashMap::new()),
            asked: Mutex::new(HashSet::new()),
        });
        let _ = cell.set(Arc::downgrade(&service));
        service
    }

    /// The bridge's resolver: a live tk chat token switches its chat on.
    pub fn place(&self, token: &str) -> Option<String> {
        let (chat, harness) = self.host.chat_of(token)?;
        {
            let mut placed = lock(&self.placed);
            if !placed.contains_key(&chat) && placed.len() >= MAX_CHATS {
                return None;
            }
            placed.insert(
                chat.clone(),
                Placed {
                    harness,
                    token: token.to_string(),
                },
            );
        }
        self.core.set_chat(&chat, true);
        Some(chat)
    }

    /// Switches off every placed chat whose token the daemon no longer takes.
    /// Returns the chats switched off.
    pub fn sweep(&self) -> Vec<String> {
        let placed: Vec<(String, Placed)> = lock(&self.placed)
            .iter()
            .map(|(c, p)| (c.clone(), p.clone()))
            .collect();
        let mut ended = Vec::new();
        for (chat, link) in placed {
            if self.host.chat_of(&link.token).map(|(c, _)| c) == Some(chat.clone()) {
                continue;
            }
            lock(&self.placed).remove(&chat);
            self.core.set_chat(&chat, false);
            ended.push(chat);
        }
        ended
    }

    fn harness_of(&self, chat: &str) -> String {
        lock(&self.placed)
            .get(chat)
            .map(|p| p.harness.clone())
            .unwrap_or_default()
    }

    /// Sends each open card of `chat` the approver has not seen to the
    /// approver, and gives the card its answer. Blocks until each answer.
    pub fn forward_cards(&self, chat: &str) {
        let cards: Vec<PendingView> = self
            .core
            .status()
            .pending
            .into_iter()
            .filter(|p| p.chat_id == chat)
            .collect();
        for card in cards {
            if !lock(&self.asked).insert(card.request_id.clone()) {
                continue;
            }
            let harness = self.harness_of(chat);
            let yes = self
                .host
                .ask(chat, &harness, &card.request_id, &summary(&card));
            // A yes the engine cannot carry out (the app is not known) is a
            // no, so the card never stays open after its answer.
            if self.core.answer(&card.request_id, yes, &harness).is_err() && yes {
                let _ = self.core.answer(&card.request_id, false, &harness);
            }
            lock(&self.asked).remove(&card.request_id);
        }
    }

    pub fn status(&self) -> Status {
        self.core.status()
    }

    pub fn stop(&self, chat: &str) -> StopReport {
        self.core.stop(chat)
    }

    pub fn stop_all(&self) {
        self.core.stop_all()
    }

    /// The user takes one app back from a chat; its runtime ends at once.
    pub fn revoke(&self, chat: &str, bundle_id: &str) -> bool {
        self.core.revoke(chat, bundle_id)
    }

    /// Marks a chat's turn as a plan turn (only look-only tools run, case
    /// 36), or ends it. tk has no plan-turn signal yet, so nothing calls
    /// this; an op that a harness adapter can reach would (a request for
    /// integration).
    pub fn set_plan(&self, chat: &str, plan: bool) -> Result<(), String> {
        self.core.set_plan(chat, plan)
    }

    /// The bridge's resolver over this service.
    fn resolver(self: &Arc<Self>) -> Resolver {
        let me = Arc::downgrade(self);
        Arc::new(move |token: &str| me.upgrade()?.place(token))
    }
}

/// What the approver reads. The app as the helper resolved it, then the
/// agent's own reason, marked as the agent's words.
pub fn summary(card: &PendingView) -> String {
    let app = match &card.resolved {
        Some(app) => {
            let mut text = match &app.name {
                Some(name) => format!("{name} ({})", app.bundle_id),
                None => app.bundle_id.clone(),
            };
            if let Some(path) = &app.path {
                text.push_str(&format!(" at {path}"));
            }
            if let Some(warning) = &app.warning {
                if let Ok(Value::String(w)) = serde_json::to_value(warning) {
                    text.push_str(&format!(" [{w}]"));
                }
            }
            text
        }
        None => format!("\"{}\" (not found on this Mac)", card.app),
    };
    let reason: String = card.reason.chars().map(shown).collect();
    format!("Let this chat control {app} until the chat ends. The agent's reason: \"{reason}\"")
}

/// One character of the agent's text as the approver sees it: controls as
/// a space, and the marks that reorder or hide text (bidi overrides and
/// isolates, zero-width marks) as a visible `\u{..}`, as tk-pi's cards do
/// (`packages/tk-pi/src/ask.ts`, `cardText`).
fn shown(c: char) -> String {
    match c {
        '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2069}'
        | '\u{feff}' => {
            format!("\\u{{{:x}}}", c as u32)
        }
        c if c.is_control() => " ".to_string(),
        c => c.to_string(),
    }
}

/// The daemon as the service's host.
pub struct DaemonHost(pub Arc<crate::daemon::Daemon>);

impl Host for DaemonHost {
    fn chat_of(&self, token: &str) -> Option<(String, String)> {
        match self.0.hello(token)? {
            crate::daemon::Who::Chat { chat, harness } => Some((chat, harness)),
            crate::daemon::Who::Daemon => None,
        }
    }

    fn ask(&self, chat: &str, harness: &str, id: &str, summary: &str) -> bool {
        use crate::protocol::{ApprovalRequest, Request};
        let (out, rx, _) = crate::daemon::Outbox::new(self.0.next_conn());
        let who = crate::daemon::Who::Chat {
            chat: chat.to_string(),
            harness: harness.to_string(),
        };
        let request = Request::Approve {
            request: ApprovalRequest {
                id: format!("cu-{id}"),
                chat: chat.to_string(),
                harness: harness.to_string(),
                tool: GRANT_TOOL.to_string(),
                summary: summary.to_string(),
                timeout_seconds: ASK_SECONDS,
            },
        };
        // `approve` answers on `out` before `handle` returns.
        self.0.handle(&who, request, &out);
        rx.try_iter()
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .any(|reply| {
                reply.get("ok").and_then(Value::as_bool) == Some(true)
                    && reply.get("answer").and_then(Value::as_str) == Some("yes")
            })
    }
}

static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();

/// The running service, once `start` ran on macOS.
pub fn running() -> Option<&'static Arc<Service>> {
    SERVICE.get()
}

/// Starts computer use in a running daemon: the engine, the bridge on
/// 127.0.0.1, its endpoint in `<runtime dir>/cu.endpoint` (0600), and the
/// watch thread. macOS only: elsewhere nothing starts and no port opens.
pub fn start(daemon: &Arc<crate::daemon::Daemon>, paths: &crate::daemon::Paths) {
    if !cfg!(target_os = "macos") || SERVICE.get().is_some() {
        return;
    }
    if let Err(e) = start_here(daemon, paths) {
        eprintln!("tk-daemon: computer use did not start: {e}");
    }
}

/// `start` without the OS check, for tests on any system.
pub(crate) fn start_here(
    daemon: &Arc<crate::daemon::Daemon>,
    paths: &crate::daemon::Paths,
) -> Result<Arc<Service>, String> {
    let data = paths.data.clone();
    let service = Service::new(Box::new(DaemonHost(daemon.clone())), |notify| {
        super::system_core(Some(data), notify)
    });
    let address = bridge::start_with(service.core.clone(), Some(service.resolver()))
        .map_err(|e| format!("bridge: {e}"))?;
    write_endpoint(paths.runtime_dir(), &address.to_string())?;
    let watcher = Arc::downgrade(&service);
    let _ = std::thread::Builder::new()
        .name("tk-daemon-cu-watch".into())
        .spawn(move || loop {
            std::thread::sleep(WATCH);
            match watcher.upgrade() {
                Some(service) => {
                    service.sweep();
                }
                None => return,
            }
        });
    let _ = SERVICE.set(service.clone());
    Ok(service)
}

fn write_endpoint(dir: &Path, address: &str) -> Result<(), String> {
    crate::service::write_private(&dir.join(super::ENDPOINT_FILE), &format!("{address}\n"))
        .map_err(|e| format!("endpoint file: {e}"))
}

#[cfg(test)]
mod tests {
    use super::super::core::tests::FakeLauncher;
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    const CHAT_TOKEN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const DAEMON_TOKEN: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    type Asks = Arc<Mutex<Vec<(String, String, String)>>>;
    type Live = Arc<Mutex<HashMap<String, (String, String)>>>;

    /// A daemon stand-in: one live chat token, answers asks as told.
    struct FakeHost {
        live: Live,
        answer: bool,
        asks: Asks,
    }

    impl Host for FakeHost {
        fn chat_of(&self, token: &str) -> Option<(String, String)> {
            lock(&self.live).get(token).cloned()
        }
        fn ask(&self, chat: &str, harness: &str, _id: &str, summary: &str) -> bool {
            lock(&self.asks).push((chat.into(), harness.into(), summary.into()));
            self.answer
        }
    }

    fn service(yes: bool) -> (Arc<Service>, Asks, Live) {
        let asks: Asks = Arc::default();
        let live: Live = Arc::default();
        lock(&live).insert(CHAT_TOKEN.to_string(), ("c1".to_string(), "pi".to_string()));
        let host = Box::new(FakeHost {
            live: live.clone(),
            answer: yes,
            asks: asks.clone(),
        });
        let service = Service::new(host, |notify| {
            Core::new(Box::new(FakeLauncher::new()), notify, true)
        });
        (service, asks, live)
    }

    fn revoke(live: &Live, token: &str) {
        lock(live).remove(token);
    }

    #[test]
    fn a_live_chat_token_places_its_chat_and_switches_it_on() {
        let (service, _, _) = service(true);
        assert_eq!(service.place(CHAT_TOKEN).as_deref(), Some("c1"));
        assert!(service.core.is_on("c1"));
        // The daemon's own token is no chat; nor is a stranger's string.
        assert_eq!(service.place(DAEMON_TOKEN), None);
        assert_eq!(service.place("not-a-token"), None);
        assert!(!service.core.is_on(""));
    }

    #[test]
    fn a_revoked_chat_is_switched_off_and_its_grants_end() {
        let (service, _, live) = service(true);
        service.place(CHAT_TOKEN).unwrap();
        service
            .core
            .grant("c1", "pi", "com.apple.calculator", None, None)
            .unwrap();
        assert_eq!(service.core.grants("c1").len(), 1);
        assert!(service.sweep().is_empty(), "still live");
        revoke(&live, CHAT_TOKEN);
        assert_eq!(service.sweep(), vec!["c1".to_string()]);
        assert!(!service.core.is_on("c1"));
        assert!(service.core.grants("c1").is_empty());
        // A relay dialing again with the old token is refused.
        assert_eq!(service.place(CHAT_TOKEN), None);
    }

    #[test]
    fn a_card_goes_to_the_approver_and_a_yes_grants_the_app() {
        use super::super::facade::{AccessAnswer, AccessRequest, Backend};
        let (service, asks, _) = service(true);
        service.place(CHAT_TOKEN).unwrap();
        let answer = service.core.request_access(
            "c1",
            AccessRequest {
                app: "com.apple.calculator".into(),
                reason: "add two numbers\u{7}".into(),
            },
            Duration::from_secs(10),
        );
        assert_eq!(
            answer,
            AccessAnswer::Granted {
                bundle_id: "com.apple.calculator".into()
            }
        );
        let asks = lock(&asks).clone();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].0, "c1");
        assert_eq!(asks[0].1, "pi");
        assert!(asks[0].2.contains("com.apple.calculator"), "{}", asks[0].2);
        assert!(
            asks[0]
                .2
                .contains("The agent's reason: \"add two numbers \""),
            "control characters are shown as spaces: {}",
            asks[0].2
        );
    }

    #[test]
    fn a_no_or_no_approver_denies_the_card() {
        use super::super::facade::{AccessAnswer, AccessRequest, Backend};
        let (service, _, _) = service(false);
        service.place(CHAT_TOKEN).unwrap();
        let answer = service.core.request_access(
            "c1",
            AccessRequest {
                app: "com.apple.calculator".into(),
                reason: "x".into(),
            },
            Duration::from_secs(10),
        );
        assert_eq!(answer, AccessAnswer::Denied);
        assert!(service.core.grants("c1").is_empty());
    }

    #[test]
    fn the_bridge_takes_a_chat_token_and_closes_when_the_chat_ends() {
        let (service, _, live) = service(true);
        let address = bridge::start_with(service.core.clone(), Some(service.resolver())).unwrap();
        let dial = |token: &str| {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            writeln!(stream, "{}", serde_json::json!({ "token": token })).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            (stream, reader, reply)
        };
        let (_s, _r, refused) = dial(DAEMON_TOKEN);
        assert_eq!(refused["monocode"], "refused");
        let (mut stream, mut reader, ok) = dial(CHAT_TOKEN);
        assert_eq!(ok["monocode"], "ok");
        let mut line = String::new();
        for message in [
            serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        ] {
            writeln!(stream, "{message}").unwrap();
            if message.get("id").is_some() {
                line.clear();
                reader.read_line(&mut line).unwrap();
            }
        }
        let tools: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            tools["result"]["tools"][0]["name"], "request_app_access",
            "{tools}"
        );
        // The chat's run stops: the next sweep ends the connection.
        revoke(&live, CHAT_TOKEN);
        service.sweep();
        let mut rest = String::new();
        let _ = writeln!(
            stream,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
        );
        let n = reader.read_line(&mut rest).unwrap_or(0);
        assert_eq!(n, 0, "closed after the chat ended, got {rest:?}");
    }

    #[test]
    fn the_summary_names_the_app_and_quotes_the_agent() {
        use super::super::core::AppInfo;
        let card = PendingView {
            request_id: "r".into(),
            chat_id: "c1".into(),
            app: "Calc".into(),
            reason: "line\nbreak".into(),
            resolved: Some(AppInfo {
                bundle_id: "com.apple.calculator".into(),
                name: Some("Calculator".into()),
                path: Some("/System/Applications/Calculator.app".into()),
                warning: None,
            }),
            refusal: None,
            asked_at: 0,
        };
        assert_eq!(
            summary(&card),
            "Let this chat control Calculator (com.apple.calculator) at /System/Applications/Calculator.app until the chat ends. The agent's reason: \"line break\""
        );
        let unknown = PendingView {
            resolved: None,
            ..card
        };
        assert!(summary(&unknown).contains("\"Calc\" (not found on this Mac)"));
        let hidden = PendingView {
            reason: "fine\u{202e}txt.exe\u{200b}".into(),
            ..unknown
        };
        assert!(
            summary(&hidden).ends_with("\"fine\\u{202e}txt.exe\\u{200b}\""),
            "{}",
            summary(&hidden)
        );
    }
}

/// The service's host against a real daemon: chat tokens it mints, its own
/// token, revocation at `run.stop`, and its approver channel.
#[cfg(test)]
mod daemon_tests {
    use super::*;
    use crate::activity::store::ActivityLog;
    use crate::daemon::{Daemon, Outbox, Who};
    use crate::protocol::{Answer, Request};
    use tk_core::event::{Event, Source};

    const DAEMON_TOKEN: &str = "abababababababababababababababababababababababababababababababab";

    fn daemon() -> (Arc<Daemon>, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("tk-cu-daemon-{}", uuid::Uuid::new_v4().simple()));
        let data = root.join("data");
        let log: &'static ActivityLog =
            Box::leak(Box::new(ActivityLog::start(data.join("activity"))));
        (Daemon::new(DAEMON_TOKEN, log, &data, false).unwrap(), root)
    }

    fn reply(rx: &std::sync::mpsc::Receiver<String>) -> Value {
        serde_json::from_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap()
    }

    fn mint(daemon: &Arc<Daemon>, chat: &str) -> String {
        let (out, rx, _) = Outbox::new(daemon.next_conn());
        daemon.handle(
            &Who::Daemon,
            Request::TokenMint {
                chat: chat.into(),
                harness: "pi".into(),
            },
            &out,
        );
        reply(&rx)["token"].as_str().unwrap().to_string()
    }

    #[test]
    fn the_daemon_places_its_chat_tokens_only() {
        let (daemon, root) = daemon();
        let host = DaemonHost(daemon.clone());
        let token = mint(&daemon, "chat-7");
        assert_eq!(
            host.chat_of(&token),
            Some(("chat-7".to_string(), "pi".to_string()))
        );
        assert_eq!(host.chat_of(DAEMON_TOKEN), None, "the daemon's own token");
        assert_eq!(host.chat_of("nope"), None);
        // `run.stop` revokes the chat's tokens (integration w1).
        let (out, rx, _) = Outbox::new(daemon.next_conn());
        daemon.handle(
            &Who::Daemon,
            Request::Event {
                event: Event {
                    id: "stop-1".into(),
                    at: 0,
                    chat_id: "chat-7".into(),
                    harness: "pi".into(),
                    source: Source::Run,
                    kind: "run.stop".into(),
                    summary: String::new(),
                    detail: serde_json::json!({}),
                    pid: Some(4242),
                    approval_id: None,
                },
            },
            &out,
        );
        assert_eq!(reply(&rx)["ok"], true);
        assert_eq!(host.chat_of(&token), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn with_no_approver_the_answer_is_no_at_once() {
        let (daemon, root) = daemon();
        let host = DaemonHost(daemon);
        let started = std::time::Instant::now();
        assert!(!host.ask("chat-1", "pi", "r1", "Let this chat control x"));
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_approvers_yes_reaches_the_card_and_its_no_does_not() {
        for (said, expect) in [(Answer::Yes, true), (Answer::No, false)] {
            let (daemon, root) = daemon();
            let (approver, pushes, _) = Outbox::new(daemon.next_conn());
            daemon.handle(&Who::Daemon, Request::Approver {}, &approver);
            assert_eq!(reply(&pushes)["ok"], true);
            let answering = daemon.clone();
            let approver_conn = approver.clone();
            let answered = std::thread::spawn(move || {
                let push = reply(&pushes);
                let request = &push["approve"];
                assert_eq!(request["tool"], GRANT_TOOL);
                assert_eq!(request["chat"], "chat-1");
                let id = request["id"].as_str().unwrap().to_string();
                answering.handle(
                    &Who::Daemon,
                    Request::Answer {
                        id,
                        answer: said,
                        chat: None,
                    },
                    &approver_conn,
                );
            });
            let host = DaemonHost(daemon.clone());
            assert_eq!(
                host.ask("chat-1", "pi", "r1", "Let this chat control x"),
                expect
            );
            answered.join().unwrap();
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn start_here_writes_a_private_endpoint_the_relay_reads() {
        use std::os::unix::fs::PermissionsExt;
        let (daemon, root) = daemon();
        let run = root.join("run/tk");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = crate::daemon::Paths {
            socket: run.join("daemon.sock"),
            data: root.join("data"),
        };
        let service = start_here(&daemon, &paths).unwrap();
        let file = run.join(super::super::ENDPOINT_FILE);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // SAFETY: getuid has no preconditions.
        let me = unsafe { libc::getuid() };
        let endpoint = super::super::relay::endpoint(&run, me).unwrap();
        assert!(endpoint.starts_with("127.0.0.1:"), "{endpoint}");

        // The whole chain: the relay dials with a chat token the daemon
        // minted and lists the chat's tools through the bridge.
        let token = mint(&daemon, "chat-9");
        let (stdin_read, mut stdin) = std::os::unix::net::UnixStream::pair().unwrap();
        let (stdout, stdout_read) = std::os::unix::net::UnixStream::pair().unwrap();
        let relay = std::thread::spawn(move || {
            super::super::bridge_cli::serve_stdio(
                Some(&endpoint),
                Some(&token),
                Duration::from_millis(50),
                stdin_read,
                stdout,
            )
        });
        use std::io::{BufRead, BufReader, Write};
        let mut lines = BufReader::new(stdout_read);
        let mut line = String::new();
        writeln!(stdin, "{}", serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}})).unwrap();
        lines.read_line(&mut line).unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        )
        .unwrap();
        let mut listed = Value::Null;
        for _ in 0..4 {
            line.clear();
            lines.read_line(&mut line).unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == 2 {
                listed = v;
                break;
            }
        }
        assert!(service.core.is_on("chat-9"), "the relay placed its chat");
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .map(|t| t.iter().filter_map(|t| t["name"].as_str()).collect())
            .unwrap_or_default();
        assert!(names.contains(&"request_app_access"), "{listed}");
        drop(stdin);
        assert_eq!(relay.join().unwrap(), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}

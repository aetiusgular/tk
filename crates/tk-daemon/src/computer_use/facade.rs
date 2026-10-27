//! The MCP server a chat's harness talks to for computer use (C3).
//!
//! MonoCode answers the harness itself and keeps its own MCP session with the
//! chat's Cua runtime behind it, so the runtime can start, stop and restart
//! with a new manifest without the harness noticing. Before any app is
//! granted the only tool is `request_app_access`, which raises the approval
//! card. After a grant, the runtime's own tools are offered, filtered again
//! to the allowed list.
//!
//! Every reply is built here from typed data. Text from the runtime is passed
//! through as MCP content and never parsed for instructions.

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};

pub use super::bridge_cli::OFF_TEXT;
use super::grants::Grant;
use super::manifest::tool_allowed;

pub const REQUEST_TOOL: &str = "request_app_access";
pub const STATUS_TOOL: &str = "computer_use_status";
/// The protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// How long one `request_app_access` call waits for the card. Short enough
/// for every harness's own tool timeout (Codex's default is 60 seconds);
/// the model calls again to keep waiting.
pub const ACCESS_WAIT: Duration = Duration::from_secs(45);
/// The longest reason shown on the card.
const MAX_REASON: usize = 400;
/// The longest app name or bundle id taken from the model.
const MAX_APP: usize = 255;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    /// What the model named: a bundle id or an app name, as given.
    pub app: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessAnswer {
    Granted {
        bundle_id: String,
    },
    Denied,
    /// The card is still open after the wait.
    Pending,
    /// No card could be shown: the window is closed, or the request was bad.
    Refused(String),
}

/// What the facade needs from the rest of MonoCode.
pub trait Backend {
    /// The grants that hold for the chat now.
    fn grants(&self, chat: &str) -> Vec<Grant>;
    /// Shows the approval card and waits up to `wait` for the answer.
    fn request_access(&self, chat: &str, request: AccessRequest, wait: Duration) -> AccessAnswer;
    /// The runtime's tool list for the chat's current grants, starting or
    /// restarting the runtime as needed.
    fn runtime_tools(&self, chat: &str) -> Result<Vec<Value>, String>;
    /// One `tools/call` on the runtime; the result as the runtime sent it.
    fn runtime_call(&self, chat: &str, name: &str, arguments: Value) -> Result<Value, String>;
    /// Whether the chat's current turn is a plan turn. Then only look-only
    /// tools run, on every harness (security review S6).
    fn plan_turn(&self, _chat: &str) -> bool {
        false
    }
    /// Whether computer use is switched on for the chat. While off, the
    /// connection stays and offers no tools (Mac test 4.7).
    fn enabled(&self, _chat: &str) -> bool {
        true
    }
}

const PLAN_REFUSAL: &str =
    "This is a plan turn: only look-only computer-use tools run. Describe the action in the plan instead.";

/// One harness connection's MCP session.
pub struct Facade {
    chat: String,
    initialized: bool,
    /// The switch and grant set the last `tools/list` answered for, to know
    /// when to tell the harness the list changed.
    listed_for: Option<(bool, BTreeSet<String>)>,
}

impl Facade {
    pub fn new(chat: impl Into<String>) -> Self {
        Self {
            chat: chat.into(),
            initialized: false,
            listed_for: None,
        }
    }

    /// A `notifications/tools/list_changed` when the grants differ from the
    /// set the harness last listed tools for.
    pub fn list_changed(&mut self, backend: &dyn Backend) -> Option<Value> {
        let listed = self.listed_for.as_ref()?;
        let now = (
            backend.enabled(&self.chat),
            grant_set(&backend.grants(&self.chat)),
        );
        if &now == listed {
            return None;
        }
        // Answered once: the next tools/list sets it again.
        self.listed_for = None;
        Some(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}))
    }

    /// Handles one message from the harness. Returns the reply for a request,
    /// None for a notification or a response.
    pub fn handle(&mut self, backend: &dyn Backend, message: &Value) -> Option<Value> {
        let object = message.as_object()?;
        let method = object.get("method").and_then(Value::as_str);
        let id = object.get("id").cloned();
        let (Some(method), Some(id)) = (method, id) else {
            // A notification (initialized, cancelled) or a stray response.
            return None;
        };
        if !(id.is_string() || id.is_number()) {
            return Some(error(Value::Null, -32600, "Invalid request id"));
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                self.initialized = true;
                result(id, initialize_result(&params))
            }
            "ping" => result(id, json!({})),
            "tools/list" if self.initialized => {
                let on = backend.enabled(&self.chat);
                let grants = backend.grants(&self.chat);
                let tools = if on {
                    self.tools(backend, &grants)
                } else {
                    Vec::new()
                };
                self.listed_for = Some((on, grant_set(&grants)));
                result(id, json!({ "tools": tools }))
            }
            "tools/call" if self.initialized && !backend.enabled(&self.chat) => {
                result(id, tool_error(OFF_TEXT))
            }
            "tools/call" if self.initialized => result(id, self.call(backend, &params)),
            "tools/list" | "tools/call" => error(id, -32002, "Server not initialized"),
            "resources/list" => result(id, json!({ "resources": [] })),
            "prompts/list" => result(id, json!({ "prompts": [] })),
            _ => error(id, -32601, "Method not found"),
        })
    }

    fn tools(&self, backend: &dyn Backend, grants: &[Grant]) -> Vec<Value> {
        let mut tools = vec![request_tool(), status_tool()];
        if grants.is_empty() {
            return tools;
        }
        // A runtime that did not start leaves only MonoCode's own tools; the
        // status tool says what is approved.
        if let Ok(list) = backend.runtime_tools(&self.chat) {
            tools.extend(
                list.into_iter()
                    .filter(|tool| {
                        tool.get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|name| {
                                tool_allowed(name) && name != REQUEST_TOOL && name != STATUS_TOOL
                            })
                    })
                    .map(with_annotations),
            );
        }
        tools
    }

    fn call(&self, backend: &dyn Backend, params: &Value) -> Value {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if backend.plan_turn(&self.chat) && name != STATUS_TOOL && !look_only(name) {
            return tool_error(PLAN_REFUSAL);
        }
        match name {
            REQUEST_TOOL => self.request(backend, &arguments),
            STATUS_TOOL => self.status(backend),
            _ if !tool_allowed(name) => tool_error(&format!(
                "{} is not offered in MonoCode's computer use.",
                printable(name, 64)
            )),
            _ => {
                if backend.grants(&self.chat).is_empty() {
                    return tool_error(
                        "No app is approved for this chat. Call request_app_access first.",
                    );
                }
                match backend.runtime_call(&self.chat, name, arguments) {
                    Ok(result) if result.is_object() => result,
                    Ok(_) => tool_error("The computer-use runtime sent a malformed result."),
                    Err(message) => tool_error(&message),
                }
            }
        }
    }

    fn request(&self, backend: &dyn Backend, arguments: &Value) -> Value {
        let app = arguments
            .get("app")
            .and_then(Value::as_str)
            .map(|app| printable(app, MAX_APP))
            .unwrap_or_default();
        if app.trim().is_empty() {
            return tool_error("Give the app's bundle id or name in `app`.");
        }
        let reason = arguments
            .get("reason")
            .and_then(Value::as_str)
            .map(|reason| printable(reason, MAX_REASON))
            .unwrap_or_default();
        let request = AccessRequest {
            app: app.trim().to_string(),
            reason: reason.trim().to_string(),
        };
        match backend.request_access(&self.chat, request, ACCESS_WAIT) {
            AccessAnswer::Granted { bundle_id } => text_result(&format!(
                "The user approved {bundle_id} for this chat. Call tools/list again if your tool list has not changed; then use list_windows or get_window_state on that app."
            )),
            AccessAnswer::Denied => tool_error(
                "The user denied access to that app. Do not ask for it again in this turn.",
            ),
            AccessAnswer::Pending => text_result(
                "The approval card is still open in MonoCode. Call request_app_access again with the same app to keep waiting.",
            ),
            AccessAnswer::Refused(message) => tool_error(&message),
        }
    }

    fn status(&self, backend: &dyn Backend) -> Value {
        let grants = backend.grants(&self.chat);
        if grants.is_empty() {
            return text_result("No app is approved for this chat yet.");
        }
        let list: Vec<String> = grants
            .iter()
            .map(|grant| match &grant.app_name {
                Some(name) => format!("{name} ({})", grant.bundle_id),
                None => grant.bundle_id.clone(),
            })
            .collect();
        text_result(&format!("Approved for this chat: {}.", list.join(", ")))
    }
}

/// Tools that only look. Every other tool acts on an app.
const LOOK_ONLY: &[&str] = &[
    "list_apps",
    "list_windows",
    "get_window_state",
    "get_accessibility_tree",
    "verify_state",
    "parse_visual_regions",
    "get_screen_size",
    "get_cursor_position",
    "zoom",
    "get_session",
];

/// Whether a tool only looks at an app.
pub fn look_only(name: &str) -> bool {
    LOOK_ONLY.contains(&name)
}

/// MonoCode's own MCP annotations on a runtime tool, replacing any the
/// runtime sent, so a harness that keeps a plan turn read only (Claude Code's
/// plan mode) can hold back every tool that acts.
fn with_annotations(mut tool: Value) -> Value {
    let look = tool
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| LOOK_ONLY.contains(&name));
    if let Some(map) = tool.as_object_mut() {
        map.insert(
            "annotations".into(),
            json!({ "readOnlyHint": look, "destructiveHint": !look, "openWorldHint": false }),
        );
    }
    tool
}

fn grant_set(grants: &[Grant]) -> BTreeSet<String> {
    grants
        .iter()
        .map(|grant| grant.bundle_id.to_ascii_lowercase())
        .collect()
}

fn initialize_result(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|known| **known == asked)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": "monocode-computer-use", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Computer use on this Mac, through MonoCode. Each app needs the user's approval for this chat: call request_app_access with the app's bundle id or name first. MonoCode itself is never available. The user can stop computer use at any time."
    })
}

fn request_tool() -> Value {
    json!({
        "name": REQUEST_TOOL,
        "description": "Ask the user to let you control one Mac app in this chat. Give the app's bundle id (for example com.apple.calculator) or its name, and one sentence on why. The user approves or denies on a card in MonoCode. No other computer-use tool works on an app until it is approved.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "app": { "type": "string", "description": "Bundle id or app name." },
                "reason": { "type": "string", "description": "Why you need this app, in one sentence." }
            },
            "required": ["app"],
            "additionalProperties": false
        }
    })
}

fn status_tool() -> Value {
    json!({
        "name": STATUS_TOOL,
        "description": "List the apps the user approved for this chat.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
    })
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_result(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn tool_error(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": true})
}

/// At most `max` characters, with control characters other than a plain
/// space removed, so model text cannot draw lines on the card.
pub fn printable(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .take(max)
        .collect()
}

fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        grants: RefCell<Vec<Grant>>,
        answer: RefCell<AccessAnswer>,
        asked: RefCell<Vec<AccessRequest>>,
        calls: RefCell<Vec<(String, Value)>>,
        runtime_ok: bool,
        plan: RefCell<bool>,
        on: RefCell<bool>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                grants: RefCell::new(Vec::new()),
                answer: RefCell::new(AccessAnswer::Denied),
                asked: RefCell::new(Vec::new()),
                calls: RefCell::new(Vec::new()),
                runtime_ok: true,
                plan: RefCell::new(false),
                on: RefCell::new(true),
            }
        }
        fn grant(&self, bundle: &str) {
            self.grants.borrow_mut().push(Grant {
                chat_id: "c1".into(),
                bundle_id: bundle.into(),
                app_name: Some("Calculator".into()),
                granted_at: 1,
                ended_at: None,
                approval_id: "a1".into(),
            });
        }
    }

    impl Backend for Fake {
        fn grants(&self, _chat: &str) -> Vec<Grant> {
            self.grants.borrow().clone()
        }
        fn request_access(
            &self,
            _chat: &str,
            request: AccessRequest,
            _wait: Duration,
        ) -> AccessAnswer {
            self.asked.borrow_mut().push(request);
            self.answer.borrow().clone()
        }
        fn runtime_tools(&self, _chat: &str) -> Result<Vec<Value>, String> {
            if !self.runtime_ok {
                return Err("helper not running".into());
            }
            Ok(vec![
                json!({"name": "click", "inputSchema": {"type": "object"}}),
                json!({"name": "get_window_state", "inputSchema": {"type": "object"}}),
                json!({"name": "start_recording", "inputSchema": {"type": "object"}}),
                json!({"name": "get_desktop_state", "inputSchema": {"type": "object"}}),
                json!({"name": "run_script", "inputSchema": {"type": "object"}}),
                json!({"name": "request_app_access", "inputSchema": {"type": "object"}}),
            ])
        }
        fn runtime_call(&self, _chat: &str, name: &str, arguments: Value) -> Result<Value, String> {
            self.calls.borrow_mut().push((name.into(), arguments));
            Ok(json!({"content": [{"type": "text", "text": "ok"}]}))
        }
        fn plan_turn(&self, _chat: &str) -> bool {
            *self.plan.borrow()
        }
        fn enabled(&self, _chat: &str) -> bool {
            *self.on.borrow()
        }
    }

    #[test]
    fn while_off_nothing_is_listed_and_every_call_is_refused() {
        let backend = Fake::new();
        backend.grant("com.apple.calculator");
        *backend.on.borrow_mut() = false;
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
        assert_eq!(reply["result"]["tools"], json!([]));
        for name in [REQUEST_TOOL, STATUS_TOOL, "click"] {
            let reply = facade
                .handle(&backend, &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":{"app":"com.apple.calculator"}}}))
                .unwrap();
            assert_eq!(reply["result"]["isError"], true, "{name}");
            assert_eq!(reply["result"]["content"][0]["text"], OFF_TEXT);
        }
        assert!(backend.calls.borrow().is_empty());
        assert!(backend.asked.borrow().is_empty());
    }

    #[test]
    fn every_switch_on_or_off_tells_the_harness() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
        facade.handle(&backend, &list);
        assert!(facade.list_changed(&backend).is_none());
        *backend.on.borrow_mut() = false;
        assert_eq!(
            facade.list_changed(&backend).unwrap()["method"],
            "notifications/tools/list_changed"
        );
        assert!(facade.list_changed(&backend).is_none(), "once per change");
        facade.handle(&backend, &list);
        *backend.on.borrow_mut() = true;
        assert!(facade.list_changed(&backend).is_some());
    }

    fn init(facade: &mut Facade, backend: &Fake) {
        let reply = facade
            .handle(
                backend,
                &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}),
            )
            .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            reply["result"]["capabilities"]["tools"]["listChanged"],
            true
        );
    }

    fn names(reply: &Value) -> Vec<String> {
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn before_a_grant_only_the_request_and_status_tools() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
        assert_eq!(names(&reply), vec![REQUEST_TOOL, STATUS_TOOL]);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"click","arguments":{"x":1}}}),
            )
            .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            backend.calls.borrow().is_empty(),
            "nothing reached the runtime"
        );
    }

    #[test]
    fn after_a_grant_only_allowed_runtime_tools_are_listed() {
        let backend = Fake::new();
        backend.grant("com.apple.calculator");
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
        let listed = names(&reply);
        assert!(listed.contains(&"click".to_string()));
        assert!(listed.contains(&"get_window_state".to_string()));
        for never in ["start_recording", "get_desktop_state", "run_script"] {
            assert!(!listed.contains(&never.to_string()), "{never} listed");
        }
        let tool = |name: &str| {
            reply["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap()
                .clone()
        };
        assert_eq!(tool("click")["annotations"]["readOnlyHint"], false);
        assert_eq!(tool("click")["annotations"]["destructiveHint"], true);
        assert_eq!(
            tool("get_window_state")["annotations"]["readOnlyHint"],
            true
        );
        // The runtime's own copy of a MonoCode tool name is never listed twice.
        assert_eq!(listed.iter().filter(|n| *n == REQUEST_TOOL).count(), 1);
    }

    #[test]
    fn a_plan_turn_runs_only_look_only_tools() {
        // Security review S6: Codex's plan turn does not hold MCP tools back.
        let backend = Fake::new();
        backend.grant("com.apple.calculator");
        *backend.plan.borrow_mut() = true;
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let call = |facade: &mut Facade, name: &str| {
            facade
                .handle(
                    &backend,
                    &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":name,"arguments":{"app":"com.apple.TextEdit"}}}),
                )
                .unwrap()
        };
        let click = call(&mut facade, "click");
        assert_eq!(click["result"]["isError"], true);
        let ask = call(&mut facade, REQUEST_TOOL);
        assert_eq!(ask["result"]["isError"], true);
        assert!(backend.asked.borrow().is_empty(), "no card in a plan turn");
        assert!(backend.calls.borrow().is_empty());
        let look = call(&mut facade, "get_window_state");
        assert_eq!(look["result"]["content"][0]["text"], "ok");
        let status = call(&mut facade, STATUS_TOOL);
        assert!(status["result"]["isError"] != true);
        // The plan turn ends: actions run again.
        *backend.plan.borrow_mut() = false;
        let click = call(&mut facade, "click");
        assert_eq!(click["result"]["content"][0]["text"], "ok");
    }

    #[test]
    fn a_call_to_a_tool_off_the_list_never_reaches_the_runtime() {
        let backend = Fake::new();
        backend.grant("com.apple.calculator");
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        for name in [
            "start_recording",
            "get_desktop_state",
            "run_script",
            "check_for_update",
            "",
        ] {
            let reply = facade
                .handle(
                    &backend,
                    &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name}}),
                )
                .unwrap();
            assert_eq!(reply["result"]["isError"], true, "{name}");
        }
        assert!(backend.calls.borrow().is_empty());
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"click","arguments":{"x":1}}}),
            )
            .unwrap();
        assert_eq!(reply["result"]["content"][0]["text"], "ok");
        assert_eq!(
            backend.calls.borrow()[0],
            ("click".to_string(), json!({"x":1}))
        );
    }

    #[test]
    fn request_tool_raises_the_card_with_clean_text() {
        let backend = Fake::new();
        *backend.answer.borrow_mut() = AccessAnswer::Granted {
            bundle_id: "com.apple.calculator".into(),
        };
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":REQUEST_TOOL,"arguments":{"app":"Calculator\n\u{202E}x","reason":"add\u{0}numbers"}}}),
            )
            .unwrap();
        assert!(reply["result"]["isError"].is_null());
        let asked = backend.asked.borrow();
        assert_eq!(asked[0].app, "Calculator x");
        assert_eq!(asked[0].reason, "addnumbers");
    }

    #[test]
    fn request_without_an_app_asks_nothing() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":REQUEST_TOOL,"arguments":{"app":"  "}}}),
            )
            .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(backend.asked.borrow().is_empty());
    }

    #[test]
    fn pending_and_denied_answers_are_plain_results() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let call = json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":REQUEST_TOOL,"arguments":{"app":"com.apple.calculator"}}});
        *backend.answer.borrow_mut() = AccessAnswer::Pending;
        let reply = facade.handle(&backend, &call).unwrap();
        assert!(reply["result"]["isError"].is_null());
        *backend.answer.borrow_mut() = AccessAnswer::Denied;
        let reply = facade.handle(&backend, &call).unwrap();
        assert_eq!(reply["result"]["isError"], true);
    }

    #[test]
    fn tells_the_harness_when_the_grants_change() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        assert!(
            facade.list_changed(&backend).is_none(),
            "nothing listed yet"
        );
        facade.handle(
            &backend,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        assert!(facade.list_changed(&backend).is_none());
        backend.grant("com.apple.calculator");
        let note = facade.list_changed(&backend).unwrap();
        assert_eq!(note["method"], "notifications/tools/list_changed");
        assert!(facade.list_changed(&backend).is_none(), "once per change");
    }

    #[test]
    fn a_runtime_that_does_not_start_leaves_monocodes_tools() {
        let mut backend = Fake::new();
        backend.runtime_ok = false;
        backend.grant("com.apple.calculator");
        let mut facade = Facade::new("c1");
        init(&mut facade, &backend);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
        assert_eq!(names(&reply), vec![REQUEST_TOOL, STATUS_TOOL]);
    }

    #[test]
    fn protocol_basics() {
        let backend = Fake::new();
        let mut facade = Facade::new("c1");
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .unwrap();
        assert_eq!(reply["error"]["code"], -32002, "list before initialize");
        let reply = facade
            .handle(&backend, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}))
            .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
        assert!(facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .is_none());
        let reply = facade
            .handle(&backend, &json!({"jsonrpc":"2.0","id":"p","method":"ping"}))
            .unwrap();
        assert_eq!(reply["result"], json!({}));
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":2,"method":"sampling/createMessage"}),
            )
            .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
        let reply = facade
            .handle(
                &backend,
                &json!({"jsonrpc":"2.0","id":{"x":1},"method":"ping"}),
            )
            .unwrap();
        assert_eq!(reply["error"]["code"], -32600);
    }
}

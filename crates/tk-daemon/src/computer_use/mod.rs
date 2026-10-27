//! Computer use (wave 3, lane C): MonoCode's side.
//!
//! - The macOS grants sit with a separate helper app, MonoCode Computer Use,
//!   which embeds Cua Driver (`macos/computer-use-helper/`). MonoCode starts
//!   it through Launch Services and talks to it over one Unix socket; the
//!   helper takes MonoCode only, checked by code signature.
//! - Each chat with computer use on gets a token. The harness's MCP server
//!   (`<MonoCode> computer-use-mcp`) reaches MonoCode's loopback bridge with
//!   it, and MonoCode answers as the MCP server (`facade.rs`).
//! - Apps are granted per chat, only from the approval card or the chat's
//!   popover, and held in memory for this run (`grants.rs`). Each grant set
//!   becomes a bounded manifest (`manifest.rs`) for that chat's runtime.
//! - Stop ends the runtime and the grants, and releases a held mouse button.

#[path = "apps.rs"]
mod apps;
#[path = "bridge.rs"]
mod bridge;
#[path = "bridge_cli.rs"]
pub mod bridge_cli;
#[path = "core.rs"]
mod core;
#[path = "facade.rs"]
mod facade;
#[path = "grants.rs"]
mod grants;
#[path = "helper_client.rs"]
mod helper_client;
#[path = "manifest.rs"]
mod manifest;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use tauri::{AppHandle, Emitter, Manager, State};

use self::core::{AppInfo, Core, HelperLauncher, Notice, Status, StopReport};
use self::grants::Grant;
use self::helper_client::{EventSink, HelperClient};

pub const REQUEST_EVENT: &str = "computer-use://request";
pub const CHANGED_EVENT: &str = "computer-use://changed";
/// The helper's bundle, inside MonoCode's own (`Contents/Helpers`).
pub const HELPER_APP: &str = "MonoCode Computer Use.app";

pub struct ComputerUse {
    core: Arc<Core>,
    endpoint: OnceLock<String>,
}

impl ComputerUse {
    pub fn new(data: Option<PathBuf>, notify: Box<dyn Fn(Notice) + Send + Sync>) -> Self {
        let launcher = Box::new(SystemLauncher {
            app: helper_location(),
            data,
        });
        Self {
            core: Core::new(launcher, notify, cfg!(target_os = "macos")),
            endpoint: OnceLock::new(),
        }
    }
}

/// Where the helper is looked for. A debug build also takes
/// `MONOCODE_COMPUTER_USE_HELPER`, for a helper built beside the repository.
/// A release build looks only inside its own bundle, a folder an installer
/// writes, not one an agent is expected to plant files in.
fn helper_location() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("MONOCODE_COMPUTER_USE_HELPER") {
        return Some(PathBuf::from(path));
    }
    let exe = std::env::current_exe().ok()?;
    // Contents/MacOS/<exe> -> Contents/Helpers/<helper>
    Some(exe.parent()?.parent()?.join("Helpers").join(HELPER_APP))
}

struct SystemLauncher {
    app: Option<PathBuf>,
    data: Option<PathBuf>,
}

impl HelperLauncher for SystemLauncher {
    fn location(&self) -> Option<String> {
        cfg!(target_os = "macos")
            .then(|| self.app.as_ref().map(|p| p.to_string_lossy().into_owned()))
            .flatten()
    }

    fn installed(&self) -> bool {
        self.app
            .as_ref()
            .is_some_and(|app| app.join("Contents").join("MacOS").is_dir())
    }

    /// The helper writes each runtime's recording under
    /// `<app data>/computer-use/recordings/<runtime>/`.
    fn recordings_root(&self) -> Option<PathBuf> {
        Some(self.data.as_ref()?.join("computer-use").join("recordings"))
    }

    #[cfg(not(target_os = "macos"))]
    fn launch(&self, _events: EventSink) -> Result<HelperClient, String> {
        Err("Computer use runs on macOS only.".into())
    }

    /// Runs `/usr/bin/open -n -g -a <helper> --args serve ...` (rules
    /// section 5, item 2: a process MonoCode starts outside any sandbox).
    /// Launch Services makes the helper its own app, so macOS credits the
    /// Accessibility and Screen Recording grants to it, not to MonoCode.
    #[cfg(target_os = "macos")]
    fn launch(&self, events: EventSink) -> Result<HelperClient, String> {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};

        let app = self
            .app
            .as_ref()
            .filter(|_| self.installed())
            .ok_or("MonoCode Computer Use is not installed beside MonoCode.")?;
        let data = self
            .data
            .as_ref()
            .ok_or("No app data folder.")?
            .join("computer-use");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&data)
            .map_err(|e| e.to_string())?;
        // A fresh private folder per launch for the socket. $TMPDIR is this
        // user's own; the random name keeps a stale or planted socket out.
        let dir = std::env::temp_dir().join(format!(
            "mcu-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..10]
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("Cannot make the helper's folder: {e}"))?;
        let socket = dir.join("h.sock");
        let status = Command::new("/usr/bin/open")
            .args(["-n", "-g", "-a"])
            .arg(app)
            .arg("--args")
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--monocode-pid")
            .arg(std::process::id().to_string())
            .arg("--data")
            .arg(&data)
            .status()
            .map_err(|e| format!("Cannot start MonoCode Computer Use: {e}"))?;
        if !status.success() {
            return Err("macOS did not open MonoCode Computer Use.".into());
        }
        let started = Instant::now();
        let stream = loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(_) if started.elapsed() < Duration::from_secs(15) => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(error) => return Err(format!("MonoCode Computer Use did not answer: {error}")),
            }
        };
        let reader = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(HelperClient::new(reader, stream, events))
    }
}

/// Starts the bridge and the menu bar item. Called once from `setup`.
pub fn init(app: &AppHandle) {
    let Some(state) = app.try_state::<ComputerUse>() else {
        return;
    };
    match bridge::start(state.core.clone()) {
        Ok(address) => {
            let _ = state.endpoint.set(address.to_string());
        }
        Err(error) => eprintln!("Computer use bridge did not start: {error}"),
    }
}

/// Builds the state, with notices turned into window events and the menu bar
/// item kept in step.
pub fn state(app: &AppHandle) -> ComputerUse {
    let data = app.path().app_data_dir().ok();
    let handle = app.clone();
    ComputerUse::new(
        data,
        Box::new(move |notice| {
            let (event, chat) = match &notice {
                Notice::Request { chat } => (REQUEST_EVENT, chat.clone()),
                Notice::Changed { chat } => (CHANGED_EVENT, chat.clone()),
            };
            let _ = handle.emit(event, serde_json::json!({ "chatId": chat }));
            let tray_handle = handle.clone();
            let _ = handle.run_on_main_thread(move || sync_tray(&tray_handle));
        }),
    )
}

/// Sets the chat's bridge endpoint and token on a harness child, or removes
/// them when computer use is off for the chat. Nothing else reads them.
///
/// `args` are the harness's spawn arguments. When they name the chat's
/// computer-use server and the chat has no token yet, one is made here: a
/// chat restored at reopen can spawn before the window's own switch-on
/// reaches the app, and its relay would then start with no endpoint (Mac
/// test 4.12). The chat stays off: the arguments come from the webview, and
/// only `computer_use_set_chat` switches it on (security review X1). Until
/// then the relay connects and is offered nothing.
pub fn configure_child(app: &AppHandle, session_id: &str, args: &[String], cmd: &mut Command) {
    cmd.env_remove(bridge_cli::ENDPOINT_ENV)
        .env_remove(bridge_cli::TOKEN_ENV);
    let Some(state) = app.try_state::<ComputerUse>() else {
        return;
    };
    if let Some((endpoint, token)) = state.child_env(session_id, args) {
        cmd.env(bridge_cli::ENDPOINT_ENV, endpoint)
            .env(bridge_cli::TOKEN_ENV, token);
        if names_codex_server(args) && !has_cu_exclude(cmd) {
            // Telemetry adds these when it is on; without it Codex's shell
            // commands would see the token (security review X2).
            let config = codex_home(cmd)
                .and_then(|home| std::fs::read_to_string(home.join("config.toml")).ok());
            cmd.args(crate::otel_receiver::harness_env::codex_shell_env_args(
                config.as_deref(),
            ));
        }
    }
}

/// Whether the spawn already keeps the computer-use token out of Codex's
/// shell (telemetry adds the same flags when it is on). Only our own
/// pattern counts: any other shell policy flag, which the webview could add,
/// does not stand in for it (review Y6).
fn has_cu_exclude(cmd: &Command) -> bool {
    cmd.get_args().any(|arg| {
        let arg = arg.to_string_lossy().to_ascii_lowercase();
        arg.starts_with("shell_environment_policy.") && arg.contains("monocode_cu_*")
    })
}

/// Codex's home as the child will see it.
fn codex_home(cmd: &Command) -> Option<PathBuf> {
    let set = cmd
        .get_envs()
        .find(|(k, _)| *k == "CODEX_HOME")
        .and_then(|(_, v)| v.map(PathBuf::from));
    set.or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))
}

impl ComputerUse {
    /// The endpoint and token a chat's harness child gets, if any.
    fn child_env(&self, session_id: &str, args: &[String]) -> Option<(String, String)> {
        // Only a harness that starts the chat's server needs them. One
        // restarted without it (switched off) never holds the token the
        // chat gets back at its next switch-on.
        if !names_server(args) {
            return None;
        }
        let endpoint = self.endpoint.get()?.clone();
        let token = self.core.mint_token(session_id)?;
        Some((endpoint, token))
    }
}

/// The MCP server name MonoCode gives the chat's bridge (`offer.ts`).
const SERVER_NAME: &str = "monocode_computer_use";

/// Whether spawn arguments add the computer-use server: Claude Code's
/// `--mcp-config <json>` with it under `mcpServers`, or Codex's
/// `-c mcp_servers.monocode_computer_use.<key>=...`.
fn names_server(args: &[String]) -> bool {
    names_codex_server(args) || names_claude_server(args)
}

fn names_codex_server(args: &[String]) -> bool {
    let codex_key = format!("mcp_servers.{SERVER_NAME}.");
    args.windows(2)
        .any(|pair| pair[0] == "-c" && pair[1].starts_with(&codex_key))
}

fn names_claude_server(args: &[String]) -> bool {
    args.iter().enumerate().any(|(at, arg)| {
        let next = args.get(at + 1).map(String::as_str);
        let config = match arg.strip_prefix("--mcp-config=") {
            Some(value) => Some(value),
            None if arg == "--mcp-config" => next,
            None => None,
        };
        config.is_some_and(|config| {
            serde_json::from_str::<serde_json::Value>(config)
                .ok()
                .and_then(|value| value.get("mcpServers")?.get(SERVER_NAME).cloned())
                .is_some()
        })
    })
}

// --- Menu bar: a stop item while any chat's runtime runs (C4). ---

#[cfg(target_os = "macos")]
const TRAY_ID: &str = "computer-use";
#[cfg(target_os = "macos")]
const TRAY_STOP: &str = "computer_use_stop_all";

#[cfg(not(target_os = "macos"))]
fn sync_tray(_app: &AppHandle) {}

#[cfg(target_os = "macos")]
fn sync_tray(app: &AppHandle) {
    use tauri::menu::{MenuBuilder, MenuItemBuilder};
    use tauri::tray::TrayIconBuilder;

    let Some(state) = app.try_state::<ComputerUse>() else {
        return;
    };
    let status = state.core.status();
    let active = !status.running_chats.is_empty();
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_visible(active);
        return;
    }
    if !active {
        return;
    }
    let Ok(stop) = MenuItemBuilder::with_id(TRAY_STOP, "Stop computer use").build(app) else {
        return;
    };
    let Ok(menu) = MenuBuilder::new(app).items(&[&stop]).build() else {
        return;
    };
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("MonoCode computer use is running")
        .menu(&menu)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == TRAY_STOP {
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Some(state) = app.try_state::<ComputerUse>() {
                        state.core.stop_all();
                    }
                });
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let _ = builder.build(app);
}

// --- Commands. Each is a user's click in MonoCode's own window. ---

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn computer_use_set_chat(state: State<'_, ComputerUse>, chat_id: String, on: bool) {
    let core = state.core.clone();
    if on {
        // At once, so the token is there before the harness restarts.
        core.set_chat(&chat_id, true);
    } else {
        // Off may stop a runtime, which waits on the helper.
        std::thread::spawn(move || core.set_chat(&chat_id, false));
    }
}

/// Marks a chat's turn as a plan turn, or ends it. While it holds, only
/// look-only tools run (security review S6). Called around every turn.
#[tauri::command]
pub fn computer_use_set_plan(
    state: State<'_, ComputerUse>,
    chat_id: String,
    plan: bool,
) -> Result<(), String> {
    state.core.set_plan(&chat_id, plan)
}

#[tauri::command]
pub fn computer_use_status(state: State<'_, ComputerUse>) -> Status {
    state.core.status()
}

#[tauri::command]
pub fn computer_use_grants(state: State<'_, ComputerUse>, chat_id: String) -> Vec<Grant> {
    state.core.grants(&chat_id)
}

#[tauri::command]
pub async fn computer_use_grant(
    state: State<'_, ComputerUse>,
    chat_id: String,
    harness: String,
    bundle_id: String,
    app_name: Option<String>,
) -> Result<Grant, String> {
    let core = state.core.clone();
    blocking(move || {
        // The app's folder, for the collector to follow (B2). Looked up here,
        // never taken from the webview.
        let app_path = core
            .resolve(&bundle_id)
            .filter(|app| app.bundle_id.eq_ignore_ascii_case(&bundle_id))
            .and_then(|app| app.path);
        core.grant(
            &chat_id,
            &harness,
            &bundle_id,
            app_name.as_deref(),
            app_path.as_deref(),
        )
    })
    .await?
}

#[tauri::command]
pub async fn computer_use_revoke(
    state: State<'_, ComputerUse>,
    chat_id: String,
    bundle_id: String,
) -> Result<bool, String> {
    let core = state.core.clone();
    blocking(move || core.revoke(&chat_id, &bundle_id)).await
}

#[tauri::command]
pub async fn computer_use_answer(
    state: State<'_, ComputerUse>,
    request_id: String,
    allow: bool,
    harness: String,
) -> Result<(), String> {
    let core = state.core.clone();
    blocking(move || core.answer(&request_id, allow, &harness)).await?
}

#[tauri::command]
pub async fn computer_use_stop(
    state: State<'_, ComputerUse>,
    chat_id: String,
) -> Result<StopReport, String> {
    let core = state.core.clone();
    blocking(move || core.stop(&chat_id)).await
}

#[tauri::command]
pub async fn computer_use_resolve_app(
    state: State<'_, ComputerUse>,
    query: String,
) -> Result<Option<AppInfo>, String> {
    let core = state.core.clone();
    blocking(move || core.resolve(&query)).await
}

#[cfg(test)]
mod tests {

    #[test]
    fn another_shell_policy_flag_does_not_skip_the_token_exclude() {
        // Review Y6: any `shell_environment_policy.*` flag made the exclude
        // skipped, so `-c shell_environment_policy.inherit="all"` left the
        // token in Codex's shell. Only our own pattern counts as done.
        let mut cmd = Command::new("codex");
        cmd.args(["-c", "shell_environment_policy.inherit=\"all\""]);
        assert!(!has_cu_exclude(&cmd));
        cmd.args(["-c", "shell_environment_policy.exclude=[\"MONOCODE_CU_*\"]"]);
        assert!(has_cu_exclude(&cmd));
        let mut keyed = Command::new("codex");
        keyed.args([
            "-c",
            "shell_environment_policy.filters.monocode_cu_*=\"exclude\"",
        ]);
        assert!(has_cu_exclude(&keyed));
    }

    use super::*;

    /// The helper keeps its own copy of the relayed tool list. Both must say
    /// the same, name for name.
    #[test]
    fn the_helpers_tool_list_matches_monocodes() {
        let source = include_str!("../../macos/computer-use-helper/src/tools.rs");
        let section = |name: &str| -> Vec<String> {
            let start = source.find(name).expect("list in helper");
            let body = &source[start..];
            let body = &body[body.find('[').unwrap() + 1..body.find("];").unwrap()];
            body.split('"')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        };
        let allowed: Vec<String> = manifest::ALLOWED_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(section("pub const ALLOWED_TOOLS"), allowed);
        let helper_only: Vec<String> = manifest::HELPER_ONLY_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(section("pub const HELPER_ONLY_TOOLS"), helper_only);
    }

    /// A state on the fake helper, so grants work off the Mac too.
    fn with_endpoint() -> ComputerUse {
        let state = ComputerUse {
            core: core::tests::core_with(core::tests::FakeLauncher::new()).0,
            endpoint: OnceLock::new(),
        };
        state.endpoint.set("127.0.0.1:5000".into()).unwrap();
        state
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn rpc(
        facade: &mut facade::Facade,
        core: &core::Core,
        id: i64,
        method: &str,
    ) -> serde_json::Value {
        facade
            .handle(
                core,
                &serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}}),
            )
            .expect("a reply")
    }

    #[test]
    fn a_spawn_naming_the_server_gets_a_token_but_the_chat_stays_off_until_set_chat() {
        // 4.12: a chat restored at reopen can spawn before the webview's
        // switch-on reaches the app, so the spawn gets a token. Security
        // review X1: the switch itself is only `computer_use_set_chat`; the
        // spawn arguments come from the webview.
        let claude = strings(&[
            "-p",
            "--mcp-config",
            r#"{"mcpServers":{"monocode_computer_use":{"type":"stdio","command":"/A/MonoCode","args":["computer-use-mcp"]}}}"#,
            "--setting-sources=user",
        ]);
        let state = with_endpoint();
        let (endpoint, token) = state.child_env("c1", &claude).expect("env for c1");
        assert_eq!(endpoint, "127.0.0.1:5000");
        assert_eq!(Some(token.clone()), state.core.token("c1"));
        assert!(
            !state.core.is_on("c1"),
            "a spawn does not switch the chat on"
        );

        // The relay connects and the facade answers "off".
        let mut facade = facade::Facade::new("c1");
        rpc(&mut facade, &state.core, 1, "initialize");
        assert_eq!(
            rpc(&mut facade, &state.core, 2, "tools/list")["result"]["tools"],
            serde_json::json!([])
        );
        assert!(state
            .core
            .grant("c1", "claude", "com.apple.calculator", None, None)
            .is_err());

        // The user's own switch-on keeps the spawn's token.
        state.core.set_chat("c1", true);
        assert_eq!(state.core.token("c1"), Some(token));
        assert!(state.core.is_on("c1"));
        assert!(state
            .core
            .grant("c1", "claude", "com.apple.calculator", None, None)
            .is_ok());

        let codex = strings(&[
            "app-server",
            "-c",
            r#"mcp_servers.monocode_computer_use.command="/A/MonoCode""#,
            "-c",
            r#"mcp_servers.monocode_computer_use.args=["computer-use-mcp"]"#,
        ]);
        assert!(state.child_env("c2", &codex).is_some());
        assert!(!state.core.is_on("c2"));
    }

    #[test]
    fn a_spawn_without_the_server_switches_nothing_on() {
        let state = with_endpoint();
        for args in [
            strings(&["-p"]),
            strings(&["--mcp-config", r#"{"mcpServers":{}}"#]),
            strings(&[
                "--mcp-config",
                r#"{"mcpServers":{"other":{"command":"x"}}}"#,
            ]),
            // The name inside some other value is not the server.
            strings(&["--append-system-prompt", "monocode_computer_use"]),
            strings(&["-c", "mcp_servers.monocode_computer_use_x.command=\"x\""]),
            strings(&["-c", "model=\"mcp_servers.monocode_computer_use.\""]),
        ] {
            assert!(state.child_env("c1", &args).is_none(), "{args:?}");
        }
        assert!(!state.core.is_on("c1"));
    }

    #[test]
    fn a_child_without_computer_use_gets_no_endpoint_or_token() {
        let state = ComputerUse::new(None, Box::new(|_| {}));
        state.core.set_chat("c1", true);
        assert!(state.core.token("c1").is_some());
        assert!(state.core.token("c2").is_none());
    }

    #[test]
    fn a_harness_restarted_without_the_server_never_gets_the_chats_token() {
        // X2: switched off, the harness restarts without the server. The
        // token it would see is the one the chat gets back at switch-on.
        let state = with_endpoint();
        state.core.set_chat("c1", true);
        state.core.set_chat("c1", false);
        assert!(state.core.token("c1").is_some());
        assert!(state.child_env("c1", &strings(&["app-server"])).is_none());
    }

    #[test]
    fn a_codex_child_with_the_server_keeps_the_token_out_of_its_shell() {
        let codex = strings(&[
            "app-server",
            "-c",
            r#"mcp_servers.monocode_computer_use.command="/A/MonoCode""#,
        ]);
        assert!(names_codex_server(&codex));
        assert!(!names_codex_server(&strings(&[
            "--mcp-config",
            r#"{"mcpServers":{"monocode_computer_use":{"command":"x"}}}"#
        ])));
        let mut cmd = Command::new("codex");
        cmd.env("CODEX_HOME", "/nonexistent-monocode-test");
        assert!(!has_cu_exclude(&cmd));
        cmd.args(crate::otel_receiver::harness_env::codex_shell_env_args(
            None,
        ));
        assert!(has_cu_exclude(&cmd));
        assert!(cmd
            .get_args()
            .any(|a| a.to_string_lossy().contains("MONOCODE_CU_*")));
    }
}

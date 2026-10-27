//! Computer use's state for one app run: the chats it is on for, their
//! grants, the open approval cards, the runtimes, and the helper connection.
//! Kept apart from Tauri so the cloud tests run it whole, with a fake helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::apps::{parse_bundle_id, warning_for, AppRefusal, AppWarning};
use super::facade::{look_only, printable, AccessAnswer, AccessRequest, Backend};
use super::grants::{Grant, GrantTable};
use super::helper_client::{EventSink, HelperClient};
use super::manifest::manifest_yaml;
use crate::activity::types::{ActivitySource, ApprovalScope, ApprovalSubject};
use crate::activity::{ActivityEvent, ApprovalRecord, ProcessKind};

/// The most chats that may hold a bridge token at once (review Y8): the same
/// cap the plan marks have.
pub const MAX_TOKENS: usize = 4096;
/// Open cards per chat; a model that asks for more is told to wait.
const MAX_PENDING_PER_CHAT: usize = 4;
/// A card nobody answered closes after this.
const PENDING_TTL: Duration = Duration::from_secs(15 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const TOOLS_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(130);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(20);

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A bridge token: 256 random bits as hex.
fn new_token() -> String {
    format!("{}{}", new_id(), new_id())
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// What the UI is told. The Tauri layer turns these into window events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// An approval card opened.
    Request { chat: String },
    /// Grants, runtimes or cards changed for a chat.
    Changed { chat: String },
}

/// Starts the helper app and returns a connection to it.
pub trait HelperLauncher: Send + Sync {
    fn launch(&self, events: EventSink) -> Result<HelperClient, String>;
    /// Where the helper is looked for, for Settings; None off macOS.
    fn location(&self) -> Option<String>;
    /// Whether the helper is installed there.
    fn installed(&self) -> bool;
    /// The folder the helper's recordings sit under. A screenshot path an
    /// action record names is copied into the activity log only from inside
    /// it. None: no screenshot is copied.
    fn recordings_root(&self) -> Option<PathBuf> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub bundle_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<AppWarning>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingView {
    pub request_id: String,
    pub chat_id: String,
    pub app: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<AppInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    pub asked_at: i64,
}

struct Pending {
    view: PendingView,
    opened: Instant,
    answer: Option<AccessAnswer>,
    /// When the user answered. The answer waits this long for a model that
    /// stopped waiting to ask again.
    answered: Option<Instant>,
}

/// How long an answer is kept for a model to collect.
const ANSWER_TTL: Duration = Duration::from_secs(5 * 60);

struct RuntimeEntry {
    id: String,
    manifest: String,
    /// The runtime's pid, when the helper named one.
    pid: Option<u32>,
}

/// Records that a runtime ended, so the check stops treating that pid's
/// writes as the runtime's (review V3).
fn note_runtime_end(chat: &str, entry: &RuntimeEntry) {
    if let Some(pid) = entry.pid {
        crate::activity::record(runtime_stop_event(chat, &entry.id, pid));
        // The collector stops following that pid (review W3).
        crate::activity::process_exited(pid);
    }
}

#[derive(Default)]
struct Inner {
    /// Each chat's bridge token. Made at the first switch-on, or at a spawn
    /// that names the chat's server (4.12), and kept through Stop so the
    /// harness keeps its connection (Mac test 4.7). Switching off makes a new
    /// one, so a token an agent's shell saw ends with the switch (security
    /// review X2). A token gives nothing while its chat is off.
    tokens: HashMap<String, String>,
    /// Chats computer use is switched on for now.
    on: std::collections::HashSet<String>,
    grants: GrantTable,
    pending: HashMap<String, Pending>,
    runtimes: HashMap<String, RuntimeEntry>,
    /// The harness each chat's last grant named, for the activity log.
    harness: HashMap<String, String>,
    /// Bumped by every stop, revoke and switch-off. A runtime started under
    /// an older number is stopped as soon as its start returns, so a stop
    /// that raced a start still wins (security review, finding 4).
    generation: HashMap<String, u64>,
    /// Chats whose current turn is a plan turn: only look-only tools run
    /// (rules section 5, item 7; security review S6).
    planning: std::collections::HashSet<String>,
}

impl Inner {
    fn bump(&mut self, chat: &str) {
        *self.generation.entry(chat.to_string()).or_insert(0) += 1;
    }

    fn generation(&self, chat: &str) -> u64 {
        self.generation.get(chat).copied().unwrap_or(0)
    }

    /// True while a card is open in any chat. Input tools wait for it, so an
    /// agent cannot click or type its way to its own Allow (review, finding 3).
    fn card_open(&self) -> bool {
        self.pending.values().any(|p| p.answer.is_none())
    }
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopReport {
    pub stopped: bool,
    pub ended: usize,
    pub released_buttons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub helper: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_path: Option<String>,
    pub running_chats: Vec<String>,
    pub pending: Vec<PendingView>,
}

pub struct Core {
    inner: Mutex<Inner>,
    answered: Condvar,
    helper: Mutex<Option<Arc<HelperClient>>>,
    launcher: Box<dyn HelperLauncher>,
    notify: Box<dyn Fn(Notice) + Send + Sync>,
    supported: bool,
    me: Weak<Core>,
}

impl Core {
    pub fn new(
        launcher: Box<dyn HelperLauncher>,
        notify: Box<dyn Fn(Notice) + Send + Sync>,
        supported: bool,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            inner: Mutex::new(Inner::default()),
            answered: Condvar::new(),
            helper: Mutex::new(None),
            launcher,
            notify,
            supported,
            me: me.clone(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // --- Chats and tokens ---

    /// Switches computer use on or off for a chat. On keeps the chat's token.
    /// Off ends its grants, its cards and its runtime, and replaces its token:
    /// its connections close, and a relay that dials again with the old token
    /// is refused and answers "off". The harness restarts without the server
    /// and gets the new token at the spawn after the next switch-on.
    pub fn set_chat(&self, chat: &str, on: bool) {
        if chat.is_empty() || chat.len() > 200 {
            return;
        }
        if on {
            let mut inner = self.lock();
            if !inner.tokens.contains_key(chat) && inner.tokens.len() >= MAX_TOKENS {
                return;
            }
            inner
                .tokens
                .entry(chat.to_string())
                .or_insert_with(new_token);
            inner.on.insert(chat.to_string());
            return;
        }
        let had = {
            let mut inner = self.lock();
            if let Some(token) = inner.tokens.get_mut(chat) {
                *token = new_token();
            }
            inner.on.remove(chat)
        };
        if had {
            self.stop(chat);
        }
    }

    /// The chat's token, made now if it has none, without switching the chat
    /// on. For a harness spawn that names the chat's server: its relay can
    /// connect, and gets nothing until the user's switch-on (security review
    /// X1, Mac test 4.12).
    pub fn mint_token(&self, chat: &str) -> Option<String> {
        if chat.is_empty() || chat.len() > 200 {
            return None;
        }
        let mut inner = self.lock();
        if let Some(token) = inner.tokens.get(chat) {
            return Some(token.clone());
        }
        if inner.tokens.len() >= MAX_TOKENS {
            return None;
        }
        Some(
            inner
                .tokens
                .entry(chat.to_string())
                .or_insert_with(new_token)
                .clone(),
        )
    }

    /// Whether computer use is switched on for a chat now.
    pub fn is_on(&self, chat: &str) -> bool {
        self.lock().on.contains(chat)
    }

    /// The chat's bridge token, if it has one. Spawns take `mint_token`.
    #[cfg(test)]
    pub fn token(&self, chat: &str) -> Option<String> {
        self.lock().tokens.get(chat).cloned()
    }

    /// The chat a token belongs to. Compared in constant time per token.
    pub fn chat_for_token(&self, token: &str) -> Option<String> {
        let inner = self.lock();
        inner
            .tokens
            .iter()
            .find(|(_, known)| constant_eq(known.as_bytes(), token.as_bytes()))
            .map(|(chat, _)| chat.clone())
    }

    // --- Grants, from the user's clicks ---

    pub fn grants(&self, chat: &str) -> Vec<Grant> {
        self.lock().grants.active(chat, now_ms())
    }

    /// The user granted an app from the card or the popover.
    pub fn grant(
        &self,
        chat: &str,
        harness: &str,
        bundle_id: &str,
        app_name: Option<&str>,
        app_path: Option<&str>,
    ) -> Result<Grant, String> {
        if !self.supported {
            return Err("Computer use runs on macOS only.".into());
        }
        let now = now_ms();
        let (grant, evicted) = {
            let mut inner = self.lock();
            if !inner.on.contains(chat) {
                return Err("Switch computer use on for this chat first.".into());
            }
            inner
                .harness
                .insert(chat.to_string(), harness_name(harness));
            inner
                .grants
                .grant(chat, bundle_id, app_name, now, new_id())
                .map_err(|e| e.message())?
        };
        if grant.granted_at == now {
            self.log_approval(&grant, true, None);
            // B2: the collector follows the granted app for this chat.
            crate::activity::register_grant(chat, &grant.bundle_id, app_path);
        }
        for ended in &evicted {
            self.log_approval(ended, true, ended.ended_at);
        }
        (self.notify)(Notice::Changed {
            chat: chat.to_string(),
        });
        Ok(grant)
    }

    /// The user removed one app. Its runtime ends at once.
    pub fn revoke(&self, chat: &str, bundle_id: &str) -> bool {
        let ended = {
            let mut inner = self.lock();
            inner.bump(chat);
            inner.grants.end(chat, bundle_id, now_ms())
        };
        let Some(ended) = ended else { return false };
        self.log_approval(&ended, true, ended.ended_at);
        self.end_runtime(chat);
        (self.notify)(Notice::Changed {
            chat: chat.to_string(),
        });
        true
    }

    /// C4: ends the chat's runtime, every grant it holds and its open cards.
    pub fn stop(&self, chat: &str) -> StopReport {
        let now = now_ms();
        let (ended, runtime) = {
            let mut inner = self.lock();
            inner.bump(chat);
            let ended = inner.grants.end_chat(chat, now);
            for pending in inner
                .pending
                .values_mut()
                .filter(|p| p.view.chat_id == chat)
            {
                pending.answer.get_or_insert(AccessAnswer::Denied);
            }
            inner.pending.retain(|_, p| p.view.chat_id != chat);
            (ended, inner.runtimes.remove(chat))
        };
        self.answered.notify_all();
        for grant in &ended {
            self.log_approval(grant, true, grant.ended_at);
        }
        let mut report = StopReport {
            ended: ended.len(),
            ..StopReport::default()
        };
        // Ask the helper even with no runtime known: it checks the buttons.
        let runtime_id = runtime
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "none".into());
        match self.connected_helper() {
            Some(helper) => {
                match helper.request("stop", json!({"runtime": runtime_id}), STOP_TIMEOUT) {
                    Ok(reply) => {
                        report.stopped =
                            reply.get("ended").and_then(Value::as_bool).unwrap_or(false);
                        report.released_buttons = reply
                            .get("releasedButtons")
                            .and_then(Value::as_array)
                            .map(|list| {
                                list.iter()
                                    .filter_map(Value::as_str)
                                    .map(|s| s.chars().take(20).collect())
                                    .collect()
                            })
                            .unwrap_or_default();
                        report.warning = reply
                            .get("warning")
                            .and_then(Value::as_str)
                            .map(|text| printable(text, 300));
                    }
                    Err(error) => report.warning = Some(error),
                }
            }
            None => report.stopped = true,
        }
        // Recorded once the helper has answered, so the runtime's last
        // recording writes fall inside its span (review W1).
        if let Some(runtime) = &runtime {
            note_runtime_end(chat, runtime);
        }
        (self.notify)(Notice::Changed {
            chat: chat.to_string(),
        });
        report
    }

    /// Every chat, as at quit.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only the macOS menu bar item calls it")
    )]
    pub fn stop_all(&self) {
        let chats: Vec<String> = {
            let inner = self.lock();
            let mut chats: Vec<String> = inner.runtimes.keys().cloned().collect();
            chats.extend(inner.grants.chats_with_grants(now_ms()));
            chats.sort();
            chats.dedup();
            chats
        };
        for chat in chats {
            self.stop(&chat);
        }
    }

    // --- Cards ---

    /// The user's answer to an agent's card.
    pub fn answer(&self, request_id: &str, allow: bool, harness: &str) -> Result<(), String> {
        let view = {
            let inner = self.lock();
            let pending = inner
                .pending
                .get(request_id)
                .ok_or("That request is no longer open.")?;
            pending.view.clone()
        };
        let answer = if allow {
            let Some(app) = view.resolved.clone().filter(|_| view.refusal.is_none()) else {
                return Err(view
                    .refusal
                    .unwrap_or_else(|| "MonoCode could not find that app.".into()));
            };
            let grant = self.grant(
                &view.chat_id,
                harness,
                &app.bundle_id,
                app.name.as_deref(),
                app.path.as_deref(),
            )?;
            AccessAnswer::Granted {
                bundle_id: grant.bundle_id,
            }
        } else {
            let record = ApprovalRecord {
                id: new_id(),
                chat_id: view.chat_id.clone(),
                harness: harness_name(harness),
                subject: ApprovalSubject::App,
                target: view
                    .resolved
                    .as_ref()
                    .map(|a| a.bundle_id.clone())
                    .unwrap_or_else(|| view.app.clone()),
                scope: ApprovalScope::Chat,
                granted: false,
                granted_at: now_ms(),
                ended_at: None,
            };
            crate::activity::record_approval(record);
            AccessAnswer::Denied
        };
        {
            let mut inner = self.lock();
            if let Some(pending) = inner.pending.get_mut(request_id) {
                pending.answer = Some(answer);
                pending.answered = Some(Instant::now());
            }
        }
        self.answered.notify_all();
        (self.notify)(Notice::Changed { chat: view.chat_id });
        Ok(())
    }

    /// Drops cards nobody answered in time, and answers nobody collected.
    fn open_cards(inner: &mut Inner) {
        inner.pending.retain(|_, p| match p.answered {
            Some(at) => at.elapsed() < ANSWER_TTL,
            None => p.opened.elapsed() < PENDING_TTL,
        });
    }

    /// What the model named, as an app the card can show.
    fn resolve_request(&self, app: &str) -> (Option<AppInfo>, Option<String>) {
        let looked_up = self.resolve(app);
        match parse_bundle_id(app) {
            Ok(id) => (
                Some(
                    looked_up
                        .filter(|found| found.bundle_id.eq_ignore_ascii_case(&id))
                        .unwrap_or(AppInfo {
                            warning: warning_for(&id),
                            bundle_id: id,
                            name: None,
                            path: None,
                        }),
                ),
                None,
            ),
            Err(AppRefusal::Never) => (None, Some(AppRefusal::Never.message().into())),
            Err(AppRefusal::Invalid) => match looked_up {
                Some(found) if parse_bundle_id(&found.bundle_id).is_ok() => (Some(found), None),
                Some(_) => (None, Some(AppRefusal::Never.message().into())),
                None => (
                    None,
                    Some(format!(
                        "No app named \"{}\" was found. Ask again with its bundle id.",
                        printable(app, 80)
                    )),
                ),
            },
        }
    }

    /// Looks an app up through the helper. None when the helper is not
    /// running or does not know it.
    pub fn resolve(&self, query: &str) -> Option<AppInfo> {
        let helper = self.helper_or_launch().ok()?;
        let reply = helper
            .request(
                "resolve",
                json!({"query": printable(query, 255)}),
                RESOLVE_TIMEOUT,
            )
            .ok()?;
        let app = reply.get("app")?;
        let bundle_id = app.get("bundleId")?.as_str()?;
        let bundle_id = parse_bundle_id(bundle_id).ok()?;
        Some(AppInfo {
            warning: warning_for(&bundle_id),
            name: app
                .get("name")
                .and_then(Value::as_str)
                .map(|n| printable(n, 80)),
            path: app
                .get("path")
                .and_then(Value::as_str)
                .map(|p| printable(p, 1024)),
            bundle_id,
        })
    }

    // --- Runtimes ---

    /// The helper when it is connected. Never waits: a launch in progress
    /// holds the lock for up to 25 seconds, and a stop or the menu bar must
    /// not wait on it (review, finding 8). The generation check catches a
    /// runtime that launch then starts.
    fn connected_helper(&self) -> Option<Arc<HelperClient>> {
        let guard = self.helper.try_lock().ok()?;
        guard.as_ref().filter(|helper| helper.is_open()).cloned()
    }

    fn helper_or_launch(&self) -> Result<Arc<HelperClient>, String> {
        if !self.supported {
            return Err("Computer use runs on macOS only.".into());
        }
        let mut guard = self.helper.lock().map_err(|_| "helper state poisoned")?;
        if let Some(helper) = guard.as_ref().filter(|helper| helper.is_open()) {
            return Ok(helper.clone());
        }
        let me = self.me.clone();
        let client = self.launcher.launch(Box::new(move |event| {
            if let Some(core) = me.upgrade() {
                core.on_helper_event(event);
            }
        }))?;
        let client = Arc::new(client);
        client.request("hello", json!({"version": 1}), Duration::from_secs(10))?;
        // The runtimes of a helper that went away are gone with it.
        let gone: Vec<(String, RuntimeEntry)> = self.lock().runtimes.drain().collect();
        for (chat, entry) in &gone {
            note_runtime_end(chat, entry);
        }
        *guard = Some(client.clone());
        Ok(client)
    }

    /// The chat's runtime id, started or restarted to match its grants.
    fn ensure_runtime(&self, chat: &str) -> Result<(Arc<HelperClient>, String), String> {
        let generation = self.lock().generation(chat);
        let grants = self.grants(chat);
        let manifest = manifest_yaml(&grants).ok_or("No app is approved for this chat.")?;
        let helper = self.helper_or_launch()?;
        let current = {
            let inner = self.lock();
            inner
                .runtimes
                .get(chat)
                .map(|r| (r.id.clone(), r.manifest == manifest))
        };
        if let Some((id, true)) = current {
            return Ok((helper, id));
        }
        // A new runtime id for each manifest, so the helper never mixes them.
        let id = new_id()[..12].to_string();
        let reply = helper.request(
            "start",
            json!({"runtime": id, "manifest": manifest}),
            START_TIMEOUT,
        )?;
        let started_pid = reply
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|pid| *pid <= u32::MAX as u64)
            .map(|pid| pid as u32);
        if let Some(pid) = started_pid {
            crate::activity::register_process(pid, chat, ProcessKind::CuaRuntime);
            // The check knows the runtime by this record: its own writes to
            // its recording folder are not flagged (security review T1).
            crate::activity::record(runtime_event(chat, &id, pid));
        }
        if let Some(event) = recording_event(chat, &id, &reply) {
            crate::activity::record(event);
        }
        {
            let mut inner = self.lock();
            if inner.generation(chat) != generation {
                drop(inner);
                let _ = helper.request("stop", json!({"runtime": id}), STOP_TIMEOUT);
                if let Some(pid) = started_pid {
                    crate::activity::record(runtime_stop_event(chat, &id, pid));
                    crate::activity::process_exited(pid);
                }
                return Err("Computer use was stopped for this chat.".into());
            }
            let previous = inner.runtimes.insert(
                chat.to_string(),
                RuntimeEntry {
                    id: id.clone(),
                    manifest,
                    pid: started_pid,
                },
            );
            if let Some(previous) = previous {
                note_runtime_end(chat, &previous);
            }
        }
        (self.notify)(Notice::Changed {
            chat: chat.to_string(),
        });
        Ok((helper, id))
    }

    fn end_runtime(&self, chat: &str) {
        let runtime = self.lock().runtimes.remove(chat);
        if let (Some(runtime), Some(helper)) = (&runtime, self.connected_helper()) {
            let _ = helper.request("stop", json!({"runtime": runtime.id}), STOP_TIMEOUT);
        }
        // After the helper's answer (review W1).
        if let Some(runtime) = &runtime {
            note_runtime_end(chat, runtime);
        }
    }

    fn on_helper_event(&self, event: Value) {
        match event.get("event").and_then(Value::as_str) {
            Some("record") => {
                let Some(runtime) = event.get("runtime").and_then(Value::as_str) else {
                    return;
                };
                let (chat, harness) = {
                    let inner = self.lock();
                    let Some(chat) = inner
                        .runtimes
                        .iter()
                        .find(|(_, entry)| entry.id == runtime)
                        .map(|(chat, _)| chat.clone())
                    else {
                        return;
                    };
                    let harness = inner.harness.get(&chat).cloned().unwrap_or_default();
                    (chat, harness)
                };
                let mut action =
                    action_event(&chat, &harness, event.get("action").unwrap_or(&Value::Null));
                attach_screenshot(
                    &mut action,
                    self.launcher.recordings_root().as_deref(),
                    crate::activity::screenshot_path,
                );
                crate::activity::record(action);
            }
            Some("exited") => {
                let Some(runtime) = event.get("runtime").and_then(Value::as_str) else {
                    return;
                };
                let chat = {
                    let mut inner = self.lock();
                    let chat = inner
                        .runtimes
                        .iter()
                        .find(|(_, entry)| entry.id == runtime)
                        .map(|(chat, _)| chat.clone());
                    if let Some(chat) = &chat {
                        if let Some(entry) = inner.runtimes.remove(chat) {
                            note_runtime_end(chat, &entry);
                        }
                    }
                    chat
                };
                if let Some(chat) = chat {
                    (self.notify)(Notice::Changed { chat });
                }
            }
            Some("closed") => {
                let gone: Vec<(String, RuntimeEntry)> = self.lock().runtimes.drain().collect();
                for (chat, entry) in &gone {
                    note_runtime_end(chat, entry);
                }
                let chats: Vec<String> = gone.into_iter().map(|(chat, _)| chat).collect();
                for chat in chats {
                    (self.notify)(Notice::Changed { chat });
                }
            }
            _ => {}
        }
    }

    pub fn status(&self) -> Status {
        let helper = if !self.supported {
            "unsupported"
        } else if self.connected_helper().is_some() {
            "running"
        } else if self.launcher.installed() {
            "stopped"
        } else {
            "missing"
        };
        let mut inner = self.lock();
        Self::open_cards(&mut inner);
        let mut pending: Vec<PendingView> = inner
            .pending
            .values()
            .filter(|p| p.answer.is_none())
            .map(|p| p.view.clone())
            .collect();
        pending.sort_by_key(|p| p.asked_at);
        let mut running_chats: Vec<String> = inner.runtimes.keys().cloned().collect();
        running_chats.sort();
        Status {
            helper,
            helper_path: self.launcher.location(),
            running_chats,
            pending,
        }
    }

    fn log_approval(&self, grant: &Grant, granted: bool, ended_at: Option<i64>) {
        if ended_at.is_some() {
            crate::activity::end_grant(&grant.chat_id, &grant.bundle_id);
        }
        let harness = self
            .lock()
            .harness
            .get(&grant.chat_id)
            .cloned()
            .unwrap_or_default();
        crate::activity::record_approval(ApprovalRecord {
            id: grant.approval_id.clone(),
            chat_id: grant.chat_id.clone(),
            harness,
            subject: ApprovalSubject::App,
            target: grant.bundle_id.clone(),
            scope: ApprovalScope::Chat,
            granted,
            granted_at: grant.granted_at,
            ended_at,
        });
    }
}

/// The harness id as the log takes it: a short plain name, or nothing.
fn harness_name(harness: &str) -> String {
    if harness.len() <= 32
        && harness
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        harness.to_string()
    } else {
        String::new()
    }
}

/// The record of a runtime's start, with its pid.
pub fn runtime_event(chat: &str, runtime: &str, pid: u32) -> ActivityEvent {
    ActivityEvent {
        id: new_id(),
        at: now_ms(),
        chat_id: chat.to_string(),
        harness: String::new(),
        source: ActivitySource::Cua,
        kind: "cua.runtime".into(),
        summary: "Computer use runtime started".into(),
        detail: json!({ "runtime": printable(runtime, 32) }),
        pid: Some(pid),
        approval_id: None,
    }
}

/// The record of a runtime's end, with its pid (review V3).
pub fn runtime_stop_event(chat: &str, runtime: &str, pid: u32) -> ActivityEvent {
    ActivityEvent {
        id: new_id(),
        at: now_ms(),
        chat_id: chat.to_string(),
        harness: String::new(),
        source: ActivitySource::Cua,
        kind: "cua.runtime.stop".into(),
        summary: "Computer use runtime ended".into(),
        detail: json!({ "runtime": printable(runtime, 32) }),
        pid: Some(pid),
        approval_id: None,
    }
}

/// The record of a runtime whose recording did not start (Mac test 4.9),
/// from the helper's `start` reply: `"recording": {"on": false, "error": ..}`.
/// None when it started, or the reply does not say.
pub fn recording_event(chat: &str, runtime: &str, reply: &Value) -> Option<ActivityEvent> {
    let recording = reply.get("recording")?;
    if recording.get("on").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    let error = recording
        .get("error")
        .and_then(Value::as_str)
        .map(|text| printable(text, 300))
        .unwrap_or_default();
    Some(ActivityEvent {
        id: new_id(),
        at: now_ms(),
        chat_id: chat.to_string(),
        harness: String::new(),
        source: ActivitySource::Cua,
        kind: "cua.recording".into(),
        summary: "Computer use recording did not start".into(),
        detail: json!({ "on": false, "error": error, "runtime": printable(runtime, 32) }),
        pid: None,
        approval_id: None,
    })
}

/// One Cua action as an activity event (C7).
pub fn action_event(chat: &str, harness: &str, action: &Value) -> ActivityEvent {
    let tool = action
        .get("tool")
        .and_then(Value::as_str)
        .map(|t| printable(t, 64))
        .unwrap_or_else(|| "unknown".into());
    let failed = action
        .get("error")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut detail = action.clone();
    if let (Some(bundle), Value::Object(map)) = (action_bundle_id(action), &mut detail) {
        map.insert("bundleId".into(), Value::String(bundle));
    }
    ActivityEvent {
        id: new_id(),
        at: now_ms(),
        chat_id: chat.to_string(),
        harness: harness.to_string(),
        source: ActivitySource::Cua,
        kind: "cua.action".into(),
        summary: if failed {
            format!("{tool} (failed)")
        } else {
            tool
        },
        detail,
        pid: action
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|pid| *pid <= u32::MAX as u64)
            .map(|pid| pid as u32),
        approval_id: None,
    }
}

/// The app an action names in its arguments, as a bundle id, for the check's
/// "app not granted" flag. Only a valid bundle id counts; a name or a pid
/// names nothing here.
fn action_bundle_id(action: &Value) -> Option<String> {
    let args = action.get("arguments")?;
    ["bundle_id", "bundleId", "app"]
        .iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_str))
        .find_map(|raw| parse_bundle_id(raw).ok())
}

/// Largest screenshot copied into the log.
const MAX_SCREENSHOT_BYTES: u64 = 16 * 1024 * 1024;

/// Moves an action's screenshot into the activity log's own folder (lane B's
/// request 5), so the panel can show it. The helper's path is dropped from
/// the record either way. A copy is made only from a regular PNG file inside
/// `recordings`, opened without following a link, under the size cap, to the
/// place `dest_for` names (None: no log, or disk under your floor).
fn attach_screenshot(
    event: &mut ActivityEvent,
    recordings: Option<&Path>,
    dest_for: impl Fn(&str) -> Option<PathBuf>,
) {
    let Value::Object(map) = &mut event.detail else {
        return;
    };
    let Some(Value::String(source)) = map.remove("screenshot") else {
        return;
    };
    let Some(root) = recordings.and_then(|r| r.canonicalize().ok()) else {
        return;
    };
    let source = Path::new(&source);
    let Some(parent) = source.parent().and_then(|p| p.canonicalize().ok()) else {
        return;
    };
    if !parent.starts_with(&root) || source.file_name().is_none() {
        return;
    }
    let Some(bytes) = read_png_nofollow(&parent.join(source.file_name().unwrap())) else {
        return;
    };
    let Some(dest) = dest_for(&event.id) else {
        return;
    };
    if write_new_private(&dest, &bytes).is_ok() {
        map.insert(
            "screenshot".into(),
            Value::String(dest.to_string_lossy().into_owned()),
        );
    }
}

fn read_png_nofollow(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK: a FIFO named like a screenshot opens at once instead of
        // waiting for a writer, then fails the regular-file check below. A
        // blocked open would stall every later action record (review T3).
        // On a regular file it changes nothing.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_SCREENSHOT_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_SCREENSHOT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SCREENSHOT_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    Some(bytes)
}

fn write_new_private(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(dest)?;
    file.write_all(bytes)
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Core {
    /// Marks the chat's turn as a plan turn, or ends that. A mark that cannot
    /// be kept (a bad id, the 4,096 cap) is refused, so the turn does not run
    /// unmarked (security review T7).
    pub fn set_plan(&self, chat: &str, plan: bool) -> Result<(), String> {
        if chat.is_empty() || chat.len() > 256 {
            return Err("Bad chat id.".into());
        }
        let mut inner = self.lock();
        if plan {
            if inner.planning.len() >= 4096 && !inner.planning.contains(chat) {
                return Err("Too many plan turns are marked.".into());
            }
            inner.planning.insert(chat.to_string());
        } else {
            inner.planning.remove(chat);
        }
        Ok(())
    }
}

impl Backend for Core {
    fn enabled(&self, chat: &str) -> bool {
        self.is_on(chat)
    }

    fn plan_turn(&self, chat: &str) -> bool {
        self.lock().planning.contains(chat)
    }

    fn grants(&self, chat: &str) -> Vec<Grant> {
        Core::grants(self, chat)
    }

    fn request_access(&self, chat: &str, request: AccessRequest, wait: Duration) -> AccessAnswer {
        if !self.supported {
            return AccessAnswer::Refused("Computer use runs on macOS only.".into());
        }
        // Already granted: no card.
        if let Ok(id) = parse_bundle_id(&request.app) {
            if self
                .grants(chat)
                .iter()
                .any(|g| g.bundle_id.eq_ignore_ascii_case(&id))
            {
                return AccessAnswer::Granted { bundle_id: id };
            }
        }
        // The same chat asking for the same app again waits on the same card.
        let existing = {
            let mut inner = self.lock();
            Self::open_cards(&mut inner);
            inner
                .pending
                .iter()
                .find(|(_, p)| p.view.chat_id == chat && p.view.app == request.app)
                .map(|(id, _)| id.clone())
        };
        let request_id = match existing {
            Some(id) => id,
            None => {
                let (resolved, refusal) = self.resolve_request(&request.app);
                if let Some(refusal) = refusal {
                    return AccessAnswer::Refused(refusal);
                }
                if let Some(app) = &resolved {
                    if self
                        .grants(chat)
                        .iter()
                        .any(|g| g.bundle_id.eq_ignore_ascii_case(&app.bundle_id))
                    {
                        return AccessAnswer::Granted {
                            bundle_id: app.bundle_id.clone(),
                        };
                    }
                }
                let mut inner = self.lock();
                if inner
                    .pending
                    .values()
                    .filter(|p| p.view.chat_id == chat && p.answer.is_none())
                    .count()
                    >= MAX_PENDING_PER_CHAT
                {
                    return AccessAnswer::Refused(
                        "Wait for the user to answer the cards already open.".into(),
                    );
                }
                let id = new_id();
                inner.pending.insert(
                    id.clone(),
                    Pending {
                        view: PendingView {
                            request_id: id.clone(),
                            chat_id: chat.to_string(),
                            app: request.app.clone(),
                            reason: request.reason.clone(),
                            resolved,
                            refusal: None,
                            asked_at: now_ms(),
                        },
                        opened: Instant::now(),
                        answer: None,
                        answered: None,
                    },
                );
                drop(inner);
                (self.notify)(Notice::Request {
                    chat: chat.to_string(),
                });
                id
            }
        };
        let deadline = Instant::now() + wait;
        let mut inner = self.lock();
        loop {
            match inner.pending.get(&request_id) {
                None => return AccessAnswer::Denied,
                Some(pending) if pending.answer.is_some() => {
                    let answer = pending.answer.clone().unwrap_or(AccessAnswer::Denied);
                    inner.pending.remove(&request_id);
                    return answer;
                }
                Some(_) => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return AccessAnswer::Pending;
            }
            inner = match self.answered.wait_timeout(inner, left) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    fn runtime_tools(&self, chat: &str) -> Result<Vec<Value>, String> {
        let (helper, id) = self.ensure_runtime(chat)?;
        let reply = helper.request("tools", json!({"runtime": id}), TOOLS_TIMEOUT)?;
        Ok(reply
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn runtime_call(&self, chat: &str, name: &str, arguments: Value) -> Result<Value, String> {
        if !look_only(name) && self.lock().card_open() {
            return Err(
                "An approval card is open in MonoCode. Input waits until the user answers it."
                    .into(),
            );
        }
        let (helper, id) = self.ensure_runtime(chat)?;
        let reply = helper.request(
            "call",
            json!({"runtime": id, "name": name, "arguments": arguments}),
            CALL_TIMEOUT,
        );
        match reply {
            Ok(reply) => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            Err(error) => {
                // A runtime that failed mid-call is started fresh next time.
                if !helper.is_open() {
                    let gone = self.lock().runtimes.remove(chat);
                    if let Some(entry) = &gone {
                        note_runtime_end(chat, entry);
                    }
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    /// A helper on a socket pair: logs every request and answers it.
    pub struct FakeLauncher {
        pub log: Arc<Mutex<Vec<Value>>>,
        pub launches: Arc<AtomicUsize>,
        pub working: bool,
        /// How long the fake helper takes to answer `start`.
        pub start_delay: Duration,
    }

    impl FakeLauncher {
        pub fn new() -> Self {
            Self {
                log: Arc::default(),
                launches: Arc::default(),
                working: true,
                start_delay: Duration::ZERO,
            }
        }
    }

    impl HelperLauncher for FakeLauncher {
        fn launch(&self, events: EventSink) -> Result<HelperClient, String> {
            if !self.working {
                return Err("helper missing".into());
            }
            self.launches.fetch_add(1, Ordering::SeqCst);
            let (ours, theirs) = UnixStream::pair().unwrap();
            let log = self.log.clone();
            let start_delay = self.start_delay;
            thread::spawn(move || {
                let mut writer = theirs.try_clone().unwrap();
                let mut pid = 100;
                for line in BufReader::new(theirs).lines() {
                    let Ok(line) = line else { break };
                    let request: Value = serde_json::from_str(&line).unwrap();
                    log.lock().unwrap().push(request.clone());
                    let id = request["id"].clone();
                    let reply = match request["op"].as_str().unwrap() {
                        "hello" => json!({"id": id, "ok": true}),
                        "start" => {
                            thread::sleep(start_delay);
                            pid += 1;
                            json!({"id": id, "ok": true, "pid": pid})
                        }
                        "tools" => json!({"id": id, "ok": true, "tools": [{"name": "click"}]}),
                        "call" => {
                            json!({"id": id, "ok": true, "result": {"content": [{"type": "text", "text": "done"}]}})
                        }
                        "stop" => {
                            json!({"id": id, "ok": true, "ended": true, "releasedButtons": ["left"]})
                        }
                        "resolve" => match request["query"].as_str().unwrap() {
                            "Calculator" | "com.apple.calculator" => {
                                json!({"id": id, "ok": true, "app": {"bundleId": "com.apple.calculator", "name": "Calculator", "path": "/System/Applications/Calculator.app"}})
                            }
                            "MonoCode" => {
                                json!({"id": id, "ok": true, "app": {"bundleId": "com.monocode.desktop", "name": "MonoCode"}})
                            }
                            _ => json!({"id": id, "ok": true, "app": null}),
                        },
                        _ => json!({"id": id, "ok": false, "error": "unknown"}),
                    };
                    writeln!(writer, "{reply}").unwrap();
                }
            });
            Ok(HelperClient::new(ours.try_clone().unwrap(), ours, events))
        }
        fn location(&self) -> Option<String> {
            Some("/fake/MonoCode Computer Use.app".into())
        }
        fn installed(&self) -> bool {
            self.working
        }
    }

    pub type Log = Arc<Mutex<Vec<Value>>>;
    pub type Notices = Arc<Mutex<Vec<Notice>>>;

    pub fn core_with(launcher: FakeLauncher) -> (Arc<Core>, Log, Notices) {
        let log = launcher.log.clone();
        let notices: Arc<Mutex<Vec<Notice>>> = Arc::default();
        let sink = notices.clone();
        let core = Core::new(
            Box::new(launcher),
            Box::new(move |n| sink.lock().unwrap().push(n)),
            true,
        );
        (core, log, notices)
    }

    fn ops(log: &Arc<Mutex<Vec<Value>>>) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .map(|r| r["op"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn minted_tokens_are_capped() {
        // Review Y8: every webview spawn naming the server minted a token for
        // any chat id, with no cap.
        let (core, _, _) = core_with(FakeLauncher::new());
        for n in 0..MAX_TOKENS {
            assert!(core.mint_token(&format!("chat-{n}")).is_some());
        }
        assert!(core.mint_token("one-too-many").is_none());
        assert!(
            core.mint_token("chat-0").is_some(),
            "an existing chat keeps its token"
        );
    }

    #[test]
    fn a_recording_that_did_not_start_is_recorded() {
        // 4.9: the helper's start reply says whether Cua recorded.
        let reply = json!({"id": 1, "ok": true, "pid": 9, "recording": {"on": false, "error": format!("bounded_resource_outside_manifest\n{}", "x".repeat(400))}});
        let event = recording_event("c1", "rt1", &reply).expect("an event");
        assert_eq!(event.source, ActivitySource::Cua);
        assert_eq!(event.kind, "cua.recording");
        assert_eq!(event.summary, "Computer use recording did not start");
        assert_eq!(event.chat_id, "c1");
        assert_eq!(event.detail["on"], false);
        assert_eq!(event.detail["runtime"], "rt1");
        let error = event.detail["error"].as_str().unwrap();
        assert!(error.starts_with("bounded_resource_outside_manifest x"));
        assert_eq!(error.chars().count(), 300);
        let bare = recording_event("c1", "rt1", &json!({"recording": {"on": false}})).unwrap();
        assert_eq!(bare.detail["error"], "");
        assert!(recording_event("c1", "rt1", &json!({"recording": {"on": true}})).is_none());
        assert!(
            recording_event("c1", "rt1", &json!({"pid": 9})).is_none(),
            "an older helper says nothing"
        );
    }

    #[test]
    fn a_helper_record_with_a_turn_and_no_pid_keeps_its_screenshot() {
        // 4.9: the helper's own records, beside Cua's turn folders.
        let root = scratch("root6");
        let calls = root.join("rt1").join("1760000000000").join("calls");
        std::fs::create_dir_all(&calls).unwrap();
        std::fs::write(calls.join("call-000001.png"), PNG).unwrap();
        let mut event = action_event(
            "c1",
            "claude",
            &json!({"source": "helper", "tool": "click", "turn": "call-000001", "error": false,
                    "screenshot": calls.join("call-000001.png").to_string_lossy()}),
        );
        assert_eq!(event.pid, None);
        assert_eq!(event.summary, "click");
        assert_eq!(event.detail["source"], "helper");
        assert_eq!(event.detail["turn"], "call-000001");
        let out = scratch("out6");
        let d = out.join("h.png");
        let d2 = d.clone();
        attach_screenshot(&mut event, Some(&root), move |_| Some(d2.clone()));
        assert_eq!(event.detail["screenshot"], d.to_string_lossy().as_ref());
        assert_eq!(std::fs::read(&d).unwrap(), PNG);
    }

    #[test]
    fn a_runtime_end_is_recorded_with_its_pid() {
        // Review V3.
        let event = runtime_stop_event("c1", "rt1", 77);
        assert_eq!(event.kind, "cua.runtime.stop");
        assert_eq!(event.source, ActivitySource::Cua);
        assert_eq!(event.pid, Some(77));
        assert_eq!(event.chat_id, "c1");
    }

    #[test]
    fn a_chat_must_be_switched_on_before_a_grant() {
        let (core, _, _) = core_with(FakeLauncher::new());
        assert!(core
            .grant("c1", "claude", "com.apple.calculator", None, None)
            .is_err());
        core.set_chat("c1", true);
        assert!(core
            .grant("c1", "claude", "com.apple.calculator", None, None)
            .is_ok());
        assert!(core
            .grant("c1", "claude", "com.monocode.desktop", None, None)
            .is_err());
    }

    #[test]
    fn tokens_are_per_chat_survive_stop_and_are_replaced_when_switched_off() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.set_chat("c2", true);
        let t1 = core.token("c1").unwrap();
        let t2 = core.token("c2").unwrap();
        assert_ne!(t1, t2);
        assert!(t1.len() >= 64);
        assert_eq!(core.chat_for_token(&t1).as_deref(), Some("c1"));
        assert_eq!(core.chat_for_token("guess"), None);
        core.set_chat("c1", true);
        assert_eq!(
            core.token("c1").unwrap(),
            t1,
            "switching on again keeps the token"
        );
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        assert!(core.is_on("c1"));
        core.set_chat("c1", false);
        assert!(!core.is_on("c1"));
        assert!(core.grants("c1").is_empty(), "off ends the grants");
        assert!(
            core.grant("c1", "claude", "com.apple.calculator", None, None)
                .is_err(),
            "nothing is granted while off"
        );
        // X2: off replaces the token, so one an agent's shell saw is dead.
        // Off also takes the server out of the launch config, so the harness
        // restarts and its next spawn with the server gets the new token.
        assert_eq!(core.chat_for_token(&t1), None, "the old token is refused");
        let t3 = core.token("c1").unwrap();
        assert_ne!(t3, t1);
        assert!(t3.len() >= 64);
        assert_eq!(core.chat_for_token(&t2).as_deref(), Some("c2"));
        core.set_chat("c1", true);
        assert_eq!(core.token("c1").unwrap(), t3, "on keeps the current token");
        assert!(core.is_on("c1"));
        // 4.7: Stop keeps the token, so the harness keeps its connection.
        core.stop("c1");
        assert_eq!(core.token("c1").unwrap(), t3);
        assert_eq!(core.chat_for_token(&t3).as_deref(), Some("c1"));
        // A spawn's token never switches a chat on, and keeps one it has.
        assert_eq!(core.mint_token("c9"), core.token("c9"));
        assert!(!core.is_on("c9"));
        assert_eq!(core.mint_token("c1").unwrap(), t3);
    }

    #[test]
    fn a_call_starts_a_runtime_and_a_new_grant_restarts_it() {
        let (core, log, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        assert!(
            core.runtime_call("c1", "click", json!({})).is_err(),
            "no grant, no runtime"
        );
        assert!(ops(&log).is_empty(), "the helper was not even started");
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        let result = core.runtime_call("c1", "click", json!({"x": 1})).unwrap();
        assert_eq!(result["content"][0]["text"], "done");
        core.runtime_call("c1", "click", json!({})).unwrap();
        assert_eq!(ops(&log), vec!["hello", "start", "call", "call"]);
        let manifest = log.lock().unwrap()[1]["manifest"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(manifest.contains("com.apple.calculator"));
        assert!(manifest.contains("display: false"));
        core.grant("c1", "claude", "com.apple.TextEdit", None, None)
            .unwrap();
        core.runtime_call("c1", "click", json!({})).unwrap();
        assert_eq!(&ops(&log)[4..], ["start", "call"]);
        assert!(core.status().running_chats.contains(&"c1".to_string()));
    }

    #[test]
    fn input_waits_while_any_card_is_open() {
        let (core, log, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.set_chat("c2", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        assert_eq!(
            core.request_access(
                "c2",
                AccessRequest {
                    app: "com.apple.TextEdit".into(),
                    reason: String::new(),
                },
                Duration::from_millis(20),
            ),
            AccessAnswer::Pending
        );
        let error = core.runtime_call("c1", "click", json!({})).unwrap_err();
        assert!(error.contains("approval card is open"));
        assert!(!ops(&log).contains(&"call".to_string()));
        // Looking still works.
        assert!(core
            .runtime_call("c1", "get_window_state", json!({}))
            .is_ok());
        let id = core.status().pending[0].request_id.clone();
        core.answer(&id, false, "claude").unwrap();
        assert!(core.runtime_call("c1", "click", json!({})).is_ok());
    }

    #[test]
    fn a_stop_during_a_start_wins() {
        let mut launcher = FakeLauncher::new();
        launcher.start_delay = Duration::from_millis(400);
        let (core, log, _) = core_with(launcher);
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        let caller = {
            let core = core.clone();
            thread::spawn(move || core.runtime_call("c1", "click", json!({})))
        };
        // Wait until the start is in flight, then stop.
        for _ in 0..200 {
            if ops(&log).contains(&"start".to_string()) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        core.stop("c1");
        let result = caller.join().unwrap();
        assert!(
            result.is_err(),
            "the call after a stop must not run: {result:?}"
        );
        let ops = ops(&log);
        assert!(!ops.contains(&"call".to_string()));
        assert_eq!(
            ops.last().unwrap(),
            "stop",
            "the late runtime was stopped: {ops:?}"
        );
        assert!(core.status().running_chats.is_empty());
    }

    #[test]
    fn revoking_an_app_stops_its_runtime() {
        let (core, log, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        core.runtime_call("c1", "click", json!({})).unwrap();
        assert!(core.revoke("c1", "com.apple.calculator"));
        assert_eq!(ops(&log).last().unwrap(), "stop");
        assert!(core.grants("c1").is_empty());
        assert!(core.status().running_chats.is_empty());
    }

    #[test]
    fn stop_ends_everything_and_reports_the_buttons() {
        let (core, log, notices) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        core.runtime_call("c1", "click", json!({})).unwrap();
        let report = core.stop("c1");
        assert!(report.stopped);
        assert_eq!(report.ended, 1);
        assert_eq!(report.released_buttons, vec!["left"]);
        assert_eq!(ops(&log).last().unwrap(), "stop");
        assert!(core.grants("c1").is_empty());
        assert!(notices
            .lock()
            .unwrap()
            .contains(&Notice::Changed { chat: "c1".into() }));
        // The next call asks for a grant again.
        assert!(core.runtime_call("c1", "click", json!({})).is_err());
    }

    #[test]
    fn stop_with_no_helper_running_starts_nothing() {
        let (core, log, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        let report = core.stop("c1");
        assert!(report.stopped);
        assert!(ops(&log).is_empty());
    }

    #[test]
    fn a_card_waits_for_the_user_and_grants_the_resolved_app() {
        let (core, _, notices) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        let asker = {
            let core = core.clone();
            thread::spawn(move || {
                core.request_access(
                    "c1",
                    AccessRequest {
                        app: "Calculator".into(),
                        reason: "sums".into(),
                    },
                    Duration::from_secs(5),
                )
            })
        };
        let mut card = None;
        for _ in 0..200 {
            if let Some(p) = core.status().pending.first().cloned() {
                card = Some(p);
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let card = card.expect("a card opened");
        assert_eq!(
            card.resolved.as_ref().unwrap().bundle_id,
            "com.apple.calculator"
        );
        assert!(notices
            .lock()
            .unwrap()
            .contains(&Notice::Request { chat: "c1".into() }));
        core.answer(&card.request_id, true, "claude").unwrap();
        assert_eq!(
            asker.join().unwrap(),
            AccessAnswer::Granted {
                bundle_id: "com.apple.calculator".into()
            }
        );
        assert_eq!(core.grants("c1").len(), 1);
        assert!(core.status().pending.is_empty());
    }

    #[test]
    fn an_unanswered_card_stays_open_and_a_repeat_waits_on_it() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        let ask = || {
            core.request_access(
                "c1",
                AccessRequest {
                    app: "com.apple.calculator".into(),
                    reason: String::new(),
                },
                Duration::from_millis(50),
            )
        };
        assert_eq!(ask(), AccessAnswer::Pending);
        assert_eq!(ask(), AccessAnswer::Pending);
        assert_eq!(core.status().pending.len(), 1, "one card, not two");
        let id = core.status().pending[0].request_id.clone();
        core.answer(&id, false, "claude").unwrap();
        assert_eq!(ask(), AccessAnswer::Denied);
        assert!(core.grants("c1").is_empty());
    }

    #[test]
    fn monocode_and_unknown_names_never_open_a_card() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        for app in ["MonoCode", "com.monocode.desktop", "No Such App"] {
            let answer = core.request_access(
                "c1",
                AccessRequest {
                    app: app.into(),
                    reason: String::new(),
                },
                Duration::from_millis(50),
            );
            assert!(
                matches!(answer, AccessAnswer::Refused(_)),
                "{app}: {answer:?}"
            );
        }
        assert!(core.status().pending.is_empty());
    }

    #[test]
    fn an_app_already_granted_answers_at_once() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        let answer = core.request_access(
            "c1",
            AccessRequest {
                app: "com.apple.Calculator".into(),
                reason: String::new(),
            },
            Duration::from_millis(50),
        );
        assert!(matches!(answer, AccessAnswer::Granted { .. }));
    }

    #[test]
    fn stop_closes_the_chats_cards() {
        let (core, _, _) = core_with(FakeLauncher::new());
        core.set_chat("c1", true);
        assert_eq!(
            core.request_access(
                "c1",
                AccessRequest {
                    app: "com.apple.calculator".into(),
                    reason: String::new()
                },
                Duration::from_millis(20)
            ),
            AccessAnswer::Pending
        );
        core.stop("c1");
        assert!(core.status().pending.is_empty());
    }

    #[test]
    fn a_missing_helper_is_a_plain_error() {
        let mut launcher = FakeLauncher::new();
        launcher.working = false;
        let (core, _, _) = core_with(launcher);
        core.set_chat("c1", true);
        core.grant("c1", "claude", "com.apple.calculator", None, None)
            .unwrap();
        let error = core.runtime_call("c1", "click", json!({})).unwrap_err();
        assert!(error.contains("missing"));
        assert_eq!(core.status().helper, "missing");
    }

    #[test]
    fn records_from_the_helper_become_cua_events() {
        let event = action_event(
            "c1",
            "claude",
            &json!({"tool": "click", "pid": 7, "error": true}),
        );
        assert_eq!(event.source, ActivitySource::Cua);
        assert_eq!(event.summary, "click (failed)");
        assert_eq!(event.pid, Some(7));
        assert_eq!(event.chat_id, "c1");
    }

    #[test]
    fn off_the_mac_nothing_is_granted() {
        let core = Core::new(Box::new(FakeLauncher::new()), Box::new(|_| {}), false);
        core.set_chat("c1", true);
        assert!(core
            .grant("c1", "claude", "com.apple.calculator", None, None)
            .is_err());
        assert_eq!(core.status().helper, "unsupported");
        assert!(matches!(
            core.request_access(
                "c1",
                AccessRequest {
                    app: "com.apple.calculator".into(),
                    reason: String::new()
                },
                Duration::from_millis(10)
            ),
            AccessAnswer::Refused(_)
        ));
    }

    // Integration (wave 3): lane C's actions in lane B's log.

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cu-int-{name}-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    fn shot_event(path: &Path) -> ActivityEvent {
        action_event(
            "c1",
            "claude",
            &json!({"tool": "click", "screenshot": path.to_string_lossy()}),
        )
    }

    #[test]
    fn an_action_names_its_app_only_by_a_valid_bundle_id() {
        let named = action_event(
            "c1",
            "claude",
            &json!({"tool": "launch_app", "arguments": {"bundle_id": "com.apple.calculator"}}),
        );
        assert_eq!(named.detail["bundleId"], "com.apple.calculator");
        let by_name = action_event(
            "c1",
            "claude",
            &json!({"tool": "launch_app", "arguments": {"app": "Calculator"}}),
        );
        assert!(by_name.detail.get("bundleId").is_none());
        let none = action_event(
            "c1",
            "claude",
            &json!({"tool": "click", "arguments": {"x": 1}}),
        );
        assert!(none.detail.get("bundleId").is_none());
    }

    #[test]
    fn a_screenshot_inside_the_recordings_is_copied_into_the_log() {
        let root = scratch("root");
        let turn = root.join("rt1").join("turn-00001");
        std::fs::create_dir_all(&turn).unwrap();
        std::fs::write(turn.join("after.png"), PNG).unwrap();
        let out = scratch("out");
        let mut event = shot_event(&turn.join("after.png"));
        let dest = out.join("x.png");
        let d = dest.clone();
        attach_screenshot(&mut event, Some(&root), move |_| Some(d.clone()));
        assert_eq!(event.detail["screenshot"], dest.to_string_lossy().as_ref());
        assert_eq!(std::fs::read(&dest).unwrap(), PNG);
    }

    #[test]
    fn a_screenshot_outside_the_recordings_or_not_a_png_is_dropped() {
        let root = scratch("root2");
        let elsewhere = scratch("else");
        std::fs::write(elsewhere.join("a.png"), PNG).unwrap();
        std::fs::write(root.join("notpng.png"), b"hello").unwrap();
        let out = scratch("out2");
        for source in [
            elsewhere.join("a.png"),
            root.join("notpng.png"),
            root.join("..")
                .join(elsewhere.file_name().unwrap())
                .join("a.png"),
            root.join("missing.png"),
        ] {
            let mut event = shot_event(&source);
            let d = out.join("y.png");
            attach_screenshot(&mut event, Some(&root), move |_| Some(d.clone()));
            assert!(event.detail.get("screenshot").is_none(), "{source:?}");
        }
        assert!(!out.join("y.png").exists());
        // No recordings folder known, or no place in the log: dropped too.
        std::fs::write(root.join("ok.png"), PNG).unwrap();
        let mut event = shot_event(&root.join("ok.png"));
        attach_screenshot(&mut event, None, |_| Some(out.join("z.png")));
        assert!(event.detail.get("screenshot").is_none());
        let mut event = shot_event(&root.join("ok.png"));
        attach_screenshot(&mut event, Some(&root), |_| None);
        assert!(event.detail.get("screenshot").is_none());
    }

    #[test]
    fn a_screenshot_link_is_not_followed() {
        let root = scratch("root3");
        let elsewhere = scratch("else3");
        std::fs::write(elsewhere.join("secret.png"), PNG).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("secret.png"), root.join("after.png")).unwrap();
        let out = scratch("out3");
        let mut event = shot_event(&root.join("after.png"));
        let d = out.join("s.png");
        attach_screenshot(&mut event, Some(&root), move |_| Some(d.clone()));
        assert!(event.detail.get("screenshot").is_none());
        assert!(!out.join("s.png").exists());
    }

    #[test]
    fn plan_marks_hold_and_a_mark_that_cannot_be_kept_is_refused() {
        let core = Core::new(Box::new(FakeLauncher::new()), Box::new(|_| {}), true);
        assert!(core.set_plan("c1", true).is_ok());
        assert!(core.plan_turn("c1"));
        assert!(core.set_plan("", true).is_err());
        assert!(core.set_plan(&"x".repeat(300), true).is_err());
        for n in 0..4095 {
            core.set_plan(&format!("chat{n}"), true).unwrap();
        }
        assert!(core.set_plan("one-too-many", true).is_err());
        assert!(core.set_plan("c1", true).is_ok(), "already marked");
        core.set_plan("c1", false).unwrap();
        assert!(!core.plan_turn("c1"));
    }

    #[test]
    fn a_fifo_named_like_a_screenshot_is_refused_without_waiting() {
        // Security review T3: an open of a FIFO would block for a writer.
        let root = scratch("root5");
        let fifo = root.join("after.png");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let out = scratch("out5");
        let (tx, rx) = std::sync::mpsc::channel();
        let root2 = root.clone();
        std::thread::spawn(move || {
            let mut event = shot_event(&root2.join("after.png"));
            let d = out.join("f.png");
            attach_screenshot(&mut event, Some(&root2), move |_| Some(d.clone()));
            let _ = tx.send(event.detail.get("screenshot").is_none());
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
    }

    #[test]
    fn an_existing_file_at_the_destination_is_never_overwritten() {
        let root = scratch("root4");
        std::fs::write(root.join("after.png"), PNG).unwrap();
        let out = scratch("out4");
        std::fs::write(out.join("taken.png"), b"mine").unwrap();
        let mut event = shot_event(&root.join("after.png"));
        let d = out.join("taken.png");
        attach_screenshot(&mut event, Some(&root), move |_| Some(d.clone()));
        assert!(event.detail.get("screenshot").is_none());
        assert_eq!(std::fs::read(out.join("taken.png")).unwrap(), b"mine");
    }
}

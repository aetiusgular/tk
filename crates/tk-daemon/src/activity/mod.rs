//! The activity log (wave 3, lane B). Detection only: nothing here blocks an
//! action. Design: research note X, section 4; plan `parallel_build_w3.md`,
//! lane card B.
//!
//! - `record`, `record_approval`: the write side lanes A and C call. Neither
//!   returns an error or panics; before `init`, or with a full queue, the
//!   record is dropped and counted (rules, section 4, case 9).
//! - `register_process`, `process_exited`, `register_grant`, `end_grant`:
//!   the registry the collector follows (B2).
//! - `commands`: what the panel and the approvals bridge call.

pub mod chain;
pub mod check;
pub mod collector;
pub mod commands;
pub mod registry;
pub mod retention;
pub mod store;
pub mod types;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use tauri::{AppHandle, Manager};

pub use types::{ActivityEvent, ApprovalRecord, ProcessKind};

static LOG: OnceLock<store::ActivityLog> = OnceLock::new();
static CHATS: OnceLock<Mutex<HashMap<String, check::ChatContext>>> = OnceLock::new();
static SCREENSHOT_FLOOR: AtomicU64 = AtomicU64::new(retention::DEFAULT_SCREENSHOT_FLOOR_BYTES);
/// The app's data folder and identifier, read once in `init`. The dev build
/// has its own (`com.monocode.desktop.dev`, decision 1), so nothing here
/// hard-codes the release one.
static DATA_DIR: OnceLock<String> = OnceLock::new();
static IDENTIFIER: OnceLock<String> = OnceLock::new();

/// Starts the log in the app's data folder. Called once from `setup`. A
/// failure leaves the log off; the app runs on.
pub fn init(app: &AppHandle) {
    let _ = IDENTIFIER.set(app.config().identifier.clone());
    let Ok(base) = app.path().app_data_dir() else {
        eprintln!("activity log: no data folder, log off");
        return;
    };
    let _ = DATA_DIR.set(base.to_string_lossy().into_owned());
    let _ = LOG.set(store::ActivityLog::start(base.join("activity")));
}

/// The app's data folder, once `init` ran.
pub(crate) fn data_dir() -> Option<&'static str> {
    DATA_DIR.get().map(String::as_str)
}

/// The app's identifier, or the release one before `init`.
pub(crate) fn identifier() -> &'static str {
    IDENTIFIER
        .get()
        .map(String::as_str)
        .unwrap_or(collector::RELEASE_IDENTIFIER)
}

pub(crate) fn log() -> Option<&'static store::ActivityLog> {
    LOG.get()
}

pub(crate) fn chats() -> &'static Mutex<HashMap<String, check::ChatContext>> {
    CHATS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Adds one event to the log.
pub fn record(event: ActivityEvent) {
    if let Some(log) = LOG.get() {
        log.submit(store::Entry::Event(event));
    }
}

/// Adds what one approval card, rule or grant allowed. A later record with
/// the same id (its `ended_at` set, say) replaces the earlier one on read.
pub fn record_approval(record: ApprovalRecord) {
    if let Some(log) = LOG.get() {
        log.submit(store::Entry::Approval(record));
    }
}

/// Tells the collector which chat a process belongs to, so it can follow the
/// process and its descendants.
pub fn register_process(pid: u32, chat_id: &str, kind: ProcessKind) {
    registry::register_process(pid, chat_id, kind, store::now_ms());
}

/// A process MonoCode started has exited. Nothing goes to the collector,
/// which learns exits on its own; the registry stops replaying it.
pub fn process_exited(pid: u32) {
    registry::process_exited(pid);
}

/// A computer-use grant began: the collector follows the app at `app_path`
/// for this chat until `end_grant`. Lane C calls it beside its
/// `ApprovalRecord` for the grant.
pub fn register_grant(chat_id: &str, bundle_id: &str, app_path: Option<&str>) {
    registry::register_grant(chat_id, bundle_id, app_path, store::now_ms());
}

pub fn end_grant(chat_id: &str, bundle_id: &str) {
    registry::end_grant(chat_id, bundle_id, store::now_ms());
}

/// Where computer use's recording may save a screenshot for `event_id`, or
/// `None`: no log, a bad id, or free disk under your floor (B7). The folder
/// is the day's, so retention keeps screenshots 3 days.
pub fn screenshot_path(event_id: &str) -> Option<PathBuf> {
    let log = LOG.get()?;
    let id: String = event_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(128)
        .collect();
    if id.is_empty() {
        return None;
    }
    let free = retention::free_bytes(log.dir());
    if !retention::screenshots_allowed(free, SCREENSHOT_FLOOR.load(Ordering::Relaxed)) {
        return None;
    }
    let day = retention::day_name(retention::day_of_ms(store::now_ms()));
    let dir = log.dir().join(retention::SCREENSHOT_DIR).join(day);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{id}.png")))
}

pub(crate) fn set_screenshot_floor(bytes: u64) {
    SCREENSHOT_FLOOR.store(bytes, Ordering::Relaxed);
}

pub(crate) fn screenshot_floor() -> u64 {
    SCREENSHOT_FLOOR.load(Ordering::Relaxed)
}

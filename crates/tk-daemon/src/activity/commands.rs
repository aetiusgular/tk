//! The activity log's Tauri commands: the approvals bridge's writes, the
//! panel's reads (B6) and the collector's install (B4).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::check::{self, ChatContext, Flag};
use super::collector::{self, CollectorStatus, InstallPlan};
use super::store::{self, Entry};
use super::types::{ActivityEvent, ActivitySource, ApprovalRecord};

/// At most this many records in one call from the webview.
pub const MAX_BATCH: usize = 500;
const MAX_FOLDERS: usize = 32;
const MAX_FOLDER_LEN: usize = 4_096;
const READ_LIMIT: usize = 20_000;

fn now() -> i64 {
    store::now_ms()
}

#[tauri::command]
pub fn activity_record(events: Vec<ActivityEvent>) {
    let Some(log) = super::log() else { return };
    for event in webview_events(events) {
        log.submit(Entry::Event(event));
    }
}

/// What the webview may write: its own adapter events and the OTel events
/// its feed passes on. Computer use's and the collector's records come from
/// Rust only, so a page with IPC cannot forge a runtime record that exempts
/// a pid, or a collector event (security review U2).
pub fn webview_events(events: Vec<ActivityEvent>) -> impl Iterator<Item = ActivityEvent> {
    events
        .into_iter()
        .take(MAX_BATCH)
        .filter(|e| matches!(e.source, ActivitySource::Adapter | ActivitySource::Otel))
}

#[tauri::command]
pub fn activity_record_approvals(records: Vec<ApprovalRecord>) {
    let Some(log) = super::log() else { return };
    for record in records.into_iter().take(MAX_BATCH) {
        log.submit(Entry::Approval(record));
    }
}

/// Keeps only absolute folders, never the root, never with `..` or NUL.
pub fn clean_context(mut context: ChatContext) -> Option<ChatContext> {
    if !super::registry::valid_chat_id(&context.chat_id) {
        return None;
    }
    context.harness = context
        .harness
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(32)
        .collect();
    let mut folders = Vec::new();
    for folder in context.folders {
        let trimmed = folder.trim_end_matches('/').to_string();
        let ok = trimmed.starts_with('/')
            && trimmed.len() <= MAX_FOLDER_LEN
            && !trimmed.contains('\0')
            && !trimmed.split('/').any(|part| part == "..");
        if ok && !folders.contains(&trimmed) {
            folders.push(trimmed);
        }
        if folders.len() >= MAX_FOLDERS {
            break;
        }
    }
    context.folders = folders;
    Some(context)
}

/// The chat's harness and folders, for the check. Sent by the webview when a
/// chat opens or its folders change.
#[tauri::command]
pub fn activity_set_chat(context: ChatContext) {
    let Some(context) = clean_context(context) else {
        return;
    };
    if let Ok(mut chats) = super::chats().lock() {
        if chats.len() >= 2_048 && !chats.contains_key(&context.chat_id) {
            return;
        }
        chats.insert(context.chat_id.clone(), context);
    }
}

/// Whether the collector answered this read. `Unknown` when nobody asked
/// (not macOS), so the panel never says "not running" for a collector it
/// did not try (Mac test 2.9, 3.11).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reachable {
    #[default]
    Unknown,
    Yes,
    No,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagsReport {
    pub flags: Vec<Flag>,
    /// The events the flags and the expected rows name, by id.
    pub events: Vec<ActivityEvent>,
    /// The app's own log verified.
    pub chain_ok: bool,
    /// The collector's log verified; `None` when it was not read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_chain_ok: Option<bool>,
    pub collector_reachable: Reachable,
    /// Why the collector's reply is missing or partial: `timeout`,
    /// `truncated`, `not running`, `refused`, or `eslogger: <line>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_reason: Option<String>,
    pub truncated: bool,
    /// Records the app dropped because its queue was full.
    pub dropped: u64,
    /// Expected events the check hid, as rows, for the panel's toggle.
    pub expected: Vec<Flag>,
    /// How many events each rule hid.
    pub suppressed: std::collections::BTreeMap<String, u64>,
}

/// Runs the check over one time range, for one chat or every chat.
#[allow(clippy::too_many_arguments)]
pub fn flags_report(
    dir: &Path,
    data_dir: Option<&str>,
    chat_id: Option<&str>,
    since: i64,
    until: i64,
    chats: &HashMap<String, ChatContext>,
    collector_events: Option<collector::EventsReply>,
    now_ms: i64,
) -> FlagsReport {
    let back = store::read(dir, since, until, chat_id, READ_LIMIT);
    // One pass over the 7 days before: known hosts, and the runtime starts
    // and ends the check needs to know a runtime's pid (reviews U1, V3, V5).
    let before = store::lookback(dir, since, chat_id);
    let known_hosts = before.hosts;
    let mut events = back.events;
    events.extend(before.runtimes);
    let mut report = FlagsReport {
        chain_ok: back.chain_ok,
        truncated: back.truncated || before.truncated,
        ..FlagsReport::default()
    };
    if let Some(reply) = collector_events {
        // A reply that never completed still counts for what it holds; the
        // panel says why it is partial (Mac test 3.12b).
        let answered = reply.complete || !reply.events.is_empty();
        report.collector_reachable = if answered {
            Reachable::Yes
        } else {
            Reachable::No
        };
        report.collector_reason = reply.problem.clone();
        if reply.complete {
            report.collector_chain_ok = Some(reply.chain_ok);
        }
        report.truncated |= reply.truncated || !reply.complete;
        for mut event in reply.events {
            if let Some(chat) = chats.get(&event.chat_id) {
                event.harness = chat.harness.clone();
            }
            events.push(event);
        }
    }
    events.sort_by_key(|event| event.at);
    let output = check::run_full(&check::CheckInput {
        events: &events,
        approvals: &back.approvals,
        chats,
        known_hosts: &known_hosts,
        now_ms,
        data_dir,
    });
    let wanted: HashSet<&str> = output
        .flags
        .iter()
        .chain(&output.expected)
        .flat_map(|f| f.context.iter().map(String::as_str))
        .collect();
    report.events = events
        .iter()
        .filter(|e| wanted.contains(e.id.as_str()))
        .cloned()
        .collect();
    report.flags = output.flags;
    report.expected = output.expected;
    report.suppressed = output.suppressed;
    report
}

#[tauri::command]
pub async fn activity_flags(
    chat_id: Option<String>,
    since: i64,
    until: Option<i64>,
) -> Result<FlagsReport, String> {
    let Some(log) = super::log() else {
        return Ok(FlagsReport::default());
    };
    let dir = log.dir().to_path_buf();
    let dropped = log.dropped();
    tauri::async_runtime::spawn_blocking(move || {
        let until = until.unwrap_or_else(now);
        let chats = super::chats().lock().map(|c| c.clone()).unwrap_or_default();
        let collector_events = if cfg!(target_os = "macos") {
            let started = std::time::Instant::now();
            let mut reply =
                collector::fetch_events(since, until, chat_id.as_deref(), collector::MAX_EVENTS);
            if reply.complete || !reply.events.is_empty() {
                // It answered; still say when eslogger itself is down.
                if let Some(problem) = collector::fetch_status_detail()
                    .as_ref()
                    .and_then(collector::eslogger_problem)
                {
                    reply.problem.get_or_insert(problem);
                }
            }
            let took = started.elapsed();
            if took > std::time::Duration::from_secs(2) {
                eprintln!("activity: collector read took {} ms", took.as_millis());
            }
            Some(reply)
        } else {
            None
        };
        let _guard = super::log().map(|l| l.read_lock.lock());
        let mut report = flags_report(
            &dir,
            super::data_dir(),
            chat_id.as_deref(),
            since,
            until,
            &chats,
            collector_events,
            now(),
        );
        report.dropped = dropped;
        report
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn activity_collector_status() -> CollectorStatus {
    tauri::async_runtime::spawn_blocking(collector::status)
        .await
        .unwrap_or_default()
}

/// Where `cargo build --release` in `macos/activity-collector` puts the
/// binary, fixed at compile time from this crate's own folder. The
/// repository is a Cargo workspace, so the app's executable no longer sits
/// a fixed number of folders below it (Mac test 3.4).
const SOURCE_TREE_COLLECTOR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../macos/activity-collector/target/release/monocode-activity-collector"
);

fn regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// The collector binary to install. A release build takes only the one
/// beside its executable. A debug build, in order: `MONOCODE_COLLECTOR_BIN`
/// (an absolute path to a regular file), the source tree's release build,
/// beside the executable, then the old build layouts above it. The install
/// still checks its hash, and the plan names the path and the hash.
fn built_collector_from(
    exe: &Path,
    env_bin: Option<&str>,
    source_tree: Option<&Path>,
    release: bool,
) -> Option<PathBuf> {
    let bundled = exe.parent()?.join(collector::BINARY_NAME);
    // A release build installs only what ships beside its executable: an
    // ancestor such as /Applications/macos/... is writable by any admin
    // user, and its binary would run as root (review X11).
    if release {
        return regular_file(&bundled).then_some(bundled);
    }
    if let Some(bin) = env_bin.map(Path::new) {
        if bin.is_absolute() && regular_file(bin) {
            return Some(bin.to_path_buf());
        }
    }
    if let Some(path) = source_tree.filter(|p| regular_file(p)) {
        return Some(path.to_path_buf());
    }
    let mut candidates = vec![bundled];
    // Old layouts: <repo>/target/<profile>/x, <repo>/src-tauri/target/<profile>/x
    // and <repo>/target/<profile>/MonoCode.app/Contents/MacOS/x.
    for up in [3usize, 4, 6] {
        if let Some(root) = exe.ancestors().nth(up) {
            candidates.push(
                root.join("macos/activity-collector/target/release")
                    .join(collector::BINARY_NAME),
            );
        }
    }
    candidates.into_iter().find(|path| regular_file(path))
}

fn built_collector(exe: &Path) -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        let env_bin = std::env::var("MONOCODE_COLLECTOR_BIN").ok();
        built_collector_from(
            exe,
            env_bin.as_deref(),
            Some(Path::new(SOURCE_TREE_COLLECTOR)),
            false,
        )
    } else {
        built_collector_from(exe, None, None, true)
    }
}

/// What the install will do. Writes nothing outside the app's data folder.
#[tauri::command]
pub async fn activity_collector_install_plan(app: AppHandle) -> Result<InstallPlan, String> {
    if !cfg!(target_os = "macos") {
        return Err("The collector runs only on macOS.".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let names = collector::Names::current();
    if collector::bundled_plist(&exe, &names).is_some() {
        return Ok(InstallPlan {
            route: "smAppService".into(),
            explanation: collector::explanation(false),
            plist: String::new(),
            commands: Vec::new(),
            staged_plist: None,
            daemon_path: names.daemon_path.clone(),
        });
    }
    let built = built_collector(&exe).ok_or_else(|| {
        "No collector binary found. Build it first: cargo build --release, in macos/activity-collector.".to_string()
    })?;
    let hash = collector::file_sha256(&built).map_err(|e| e.to_string())?;
    let data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let stage = data.join("activity").join("install");
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let staged = stage.join(format!("{}.plist", names.label));
    let exe_text = exe.to_string_lossy().to_string();
    let plan = collector::terminal_plan(
        &names,
        &built.to_string_lossy(),
        &hash,
        &staged.to_string_lossy(),
        &exe_text,
    )?;
    std::fs::write(&staged, &plan.plist).map_err(|e| e.to_string())?;
    Ok(plan)
}

/// Registers the bundled collector. Runs only on your click in the panel,
/// after the plan was shown.
#[tauri::command]
pub async fn activity_collector_register() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(collector::register_bundled)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn activity_collector_unregister() -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let names = collector::Names::current();
    if collector::bundled_plist(&exe, &names).is_some() {
        tauri::async_runtime::spawn_blocking(collector::unregister_bundled)
            .await
            .map_err(|e| e.to_string())??;
        return Ok(Vec::new());
    }
    Ok(collector::removal_commands(&names))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionSettings {
    pub screenshot_floor_bytes: u64,
}

#[tauri::command]
pub fn activity_set_retention(settings: RetentionSettings) -> RetentionSettings {
    // A floor of at least 256 MB, so a typo cannot fill the disk.
    let floor = settings.screenshot_floor_bytes.max(256 * 1024 * 1024);
    super::set_screenshot_floor(floor);
    RetentionSettings {
        screenshot_floor_bytes: super::screenshot_floor(),
    }
}

/// A screenshot for the panel, as a data URL. Only a `.png` inside the log's
/// screenshot folder, resolved on disk so a symlink cannot reach outside.
#[tauri::command]
pub async fn activity_screenshot(path: String) -> Result<String, String> {
    let Some(log) = super::log() else {
        return Err("The activity log is off.".into());
    };
    let root = log.dir().join(super::retention::SCREENSHOT_DIR);
    tauri::async_runtime::spawn_blocking(move || read_screenshot(&root, Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

pub fn read_screenshot(root: &Path, path: &Path) -> Result<String, String> {
    use base64::Engine as _;
    const MAX_BYTES: u64 = 20 * 1024 * 1024;
    let root = root
        .canonicalize()
        .map_err(|_| "No screenshots yet.".to_string())?;
    let real = path
        .canonicalize()
        .map_err(|_| "The screenshot is gone.".to_string())?;
    if !real.starts_with(&root) || real.extension().and_then(|e| e.to_str()) != Some("png") {
        return Err("Not a screenshot from the activity log.".into());
    }
    let meta = std::fs::metadata(&real).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Err("Not a screenshot from the activity log.".into());
    }
    let bytes = std::fs::read(&real).map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::store::ChainWriter;
    use crate::activity::types::{ActivitySource, ApprovalScope, ApprovalSubject};
    use serde_json::json;

    const DATA: &str = "/Users/t/Library/Application Support/com.monocode.desktop";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("activity-cmd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn contexts_keep_only_safe_folders() {
        let context = clean_context(ChatContext {
            chat_id: "c1".into(),
            harness: "claude<script>".into(),
            folders: vec![
                "/Users/t/app/".into(),
                "relative".into(),
                "/Users/t/../etc".into(),
                "/Users/t/app".into(),
                "/Users/t/with space".into(),
            ],
        })
        .unwrap();
        assert_eq!(context.folders, vec!["/Users/t/app", "/Users/t/with space"]);
        assert_eq!(context.harness, "claudescript");
        assert!(clean_context(ChatContext {
            chat_id: "bad id".into(),
            ..ChatContext::default()
        })
        .is_none());
    }

    #[test]
    fn a_report_joins_the_app_log_and_the_collector() {
        let dir = temp("report");
        let day = 20_377 * 86_400_000_i64;
        let mut writer = ChainWriter::new(dir.clone());
        writer
            .write(
                &Entry::Approval(ApprovalRecord {
                    id: "g1".into(),
                    chat_id: "c1".into(),
                    harness: "claude".into(),
                    subject: ApprovalSubject::App,
                    target: "com.apple.calculator".into(),
                    scope: ApprovalScope::Chat,
                    granted: true,
                    granted_at: day - 86_400_000 * 2,
                    ended_at: None,
                }),
                day - 86_400_000 * 2,
            )
            .unwrap();
        let chats = HashMap::from([(
            "c1".to_string(),
            ChatContext {
                chat_id: "c1".into(),
                harness: "claude".into(),
                folders: vec!["/Users/t/app".into()],
            },
        )]);
        let collector_events = collector::EventsReply {
            events: vec![
                ActivityEvent {
                    id: "es-1".into(),
                    at: day + 10,
                    chat_id: "c1".into(),
                    harness: String::new(),
                    source: ActivitySource::Eslogger,
                    kind: "unlink".into(),
                    summary: "deleted".into(),
                    detail: json!({ "path": "/Users/t/Desktop/x", "grant": "com.apple.calculator" }),
                    pid: Some(4),
                    approval_id: None,
                },
                ActivityEvent {
                    id: "es-2".into(),
                    at: day + 20,
                    chat_id: "c1".into(),
                    harness: String::new(),
                    source: ActivitySource::Eslogger,
                    kind: "write".into(),
                    summary: "wrote".into(),
                    detail: json!({ "path": "/Users/t/app/ok.txt" }),
                    pid: Some(4),
                    approval_id: None,
                },
            ],
            truncated: false,
            chain_ok: true,
            complete: true,
            problem: None,
        };
        let report = flags_report(
            &dir,
            Some(DATA),
            Some("c1"),
            day,
            day + 1_000,
            &chats,
            Some(collector_events),
            day + 2_000,
        );
        assert!(report.chain_ok);
        assert_eq!(report.collector_chain_ok, Some(true));
        assert_eq!(
            report.flags.len(),
            1,
            "the grant from two days ago still covers the app, not the write"
        );
        assert_eq!(report.flags[0].event_id, "es-1");
        assert!(report.events.iter().all(|e| e.harness == "claude"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_runtime_started_before_the_window_still_exempts_its_recordings() {
        // Security review U1: a runtime lives across turns.
        let dir = temp("runtime-before");
        let day = 20_377 * 86_400_000_i64;
        let mut writer = ChainWriter::new(dir.clone());
        let runtime = ActivityEvent {
            id: "rt".into(),
            at: day - 3_600_000,
            chat_id: "c1".into(),
            harness: String::new(),
            source: ActivitySource::Cua,
            kind: "cua.runtime".into(),
            summary: String::new(),
            detail: json!({ "runtime": "rt1" }),
            pid: Some(77),
            approval_id: None,
        };
        writer
            .write(&Entry::Event(runtime), day - 3_600_000)
            .unwrap();
        let rec = "/Users/t/Library/Application Support/com.monocode.desktop/computer-use/recordings/rt1/turn-00009/after.png";
        let write = |id: &str, pid: u32| ActivityEvent {
            id: id.into(),
            at: day + 10,
            chat_id: "c1".into(),
            harness: String::new(),
            source: ActivitySource::Eslogger,
            kind: "create".into(),
            summary: String::new(),
            detail: json!({ "path": rec }),
            pid: Some(pid),
            approval_id: None,
        };
        let reply = collector::EventsReply {
            events: vec![write("own", 77), write("other", 5)],
            truncated: false,
            chain_ok: true,
            complete: true,
            problem: None,
        };
        let report = flags_report(
            &dir,
            Some(DATA),
            Some("c1"),
            day,
            day + 1_000,
            &HashMap::new(),
            Some(reply),
            day + 2_000,
        );
        let ids: Vec<&str> = report.flags.iter().map(|f| f.event_id.as_str()).collect();
        assert_eq!(ids, vec!["other"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_webview_cannot_write_computer_use_or_collector_records() {
        // Security review U2.
        let make = |source: ActivitySource| ActivityEvent {
            id: "x".into(),
            at: 1,
            chat_id: "c1".into(),
            harness: String::new(),
            source,
            kind: "cua.runtime".into(),
            summary: String::new(),
            detail: json!({}),
            pid: Some(1),
            approval_id: None,
        };
        let kept: Vec<ActivitySource> = webview_events(vec![
            make(ActivitySource::Adapter),
            make(ActivitySource::Otel),
            make(ActivitySource::Cua),
            make(ActivitySource::Eslogger),
            make(ActivitySource::Approval),
        ])
        .map(|e| e.source)
        .collect();
        assert_eq!(kept, vec![ActivitySource::Adapter, ActivitySource::Otel]);
    }

    #[test]
    fn a_report_without_a_collector_still_runs() {
        let dir = temp("nocollector");
        let report = flags_report(&dir, Some(DATA), None, 0, 10, &HashMap::new(), None, 20);
        assert!(report.flags.is_empty());
        assert_eq!(report.collector_reachable, Reachable::Unknown);
        assert_eq!(report.collector_chain_ok, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn screenshots_are_read_only_from_their_folder() {
        let dir = temp("shots");
        let root = dir.join("screenshots");
        std::fs::create_dir_all(root.join("2026-10-16")).unwrap();
        std::fs::write(root.join("2026-10-16/a.png"), b"\x89PNG").unwrap();
        std::fs::write(dir.join("secret.png"), b"no").unwrap();
        std::fs::write(root.join("2026-10-16/notes.txt"), b"no").unwrap();
        assert!(read_screenshot(&root, &root.join("2026-10-16/a.png"))
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(read_screenshot(&root, &dir.join("secret.png")).is_err());
        assert!(read_screenshot(&root, &root.join("2026-10-16/../../secret.png")).is_err());
        assert!(read_screenshot(&root, &root.join("2026-10-16/notes.txt")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret.png"), root.join("2026-10-16/link.png"))
                .unwrap();
            assert!(read_screenshot(&root, &root.join("2026-10-16/link.png")).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_floor_has_a_minimum() {
        let set = activity_set_retention(RetentionSettings {
            screenshot_floor_bytes: 1,
        });
        assert_eq!(set.screenshot_floor_bytes, 256 * 1024 * 1024);
        activity_set_retention(RetentionSettings {
            screenshot_floor_bytes: super::super::retention::DEFAULT_SCREENSHOT_FLOOR_BYTES,
        });
    }

    #[test]
    fn the_built_collector_is_found_from_a_workspace_app_bundle() {
        // Mac test 3.4: a Cargo workspace builds to <repo>/target, and on
        // macOS the dev executable sits inside MonoCode.app.
        let root = temp("built-ws");
        let exe = root.join("target/debug/MonoCode.app/Contents/MacOS/MonoCode");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let bin = root
            .join("macos/activity-collector/target/release")
            .join(collector::BINARY_NAME);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"x").unwrap();
        assert_eq!(built_collector_from(&exe, None, None, false), Some(bin));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_env_override_and_the_source_tree_come_first() {
        let root = temp("built-order");
        let exe = root.join("x/MonoCode");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let env_bin = root.join("env-collector");
        let tree_bin = root.join("tree-collector");
        std::fs::write(&env_bin, b"x").unwrap();
        std::fs::write(&tree_bin, b"x").unwrap();
        std::fs::write(root.join("x").join(collector::BINARY_NAME), b"x").unwrap();
        let env = env_bin.to_string_lossy().to_string();
        assert_eq!(
            built_collector_from(&exe, Some(&env), Some(&tree_bin), false),
            Some(env_bin.clone())
        );
        // A relative or missing override is ignored.
        assert_eq!(
            built_collector_from(&exe, Some("rel/c"), Some(&tree_bin), false),
            Some(tree_bin.clone())
        );
        assert_eq!(
            built_collector_from(&exe, None, None, false),
            Some(root.join("x").join(collector::BINARY_NAME))
        );
        // A directory is not a binary.
        let dir = root.join("adir");
        std::fs::create_dir_all(&dir).unwrap();
        let dir_text = dir.to_string_lossy().to_string();
        assert_eq!(
            built_collector_from(&exe, Some(&dir_text), Some(&tree_bin), false),
            Some(tree_bin)
        );
        assert!(SOURCE_TREE_COLLECTOR
            .ends_with("/../macos/activity-collector/target/release/monocode-activity-collector"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_incomplete_collector_reply_still_reaches_the_check() {
        // Mac test 3.12b: the reply was dropped whole when its end line
        // did not arrive in time.
        let dir = temp("partial");
        let event = ActivityEvent {
            id: "es-9".into(),
            at: 10,
            chat_id: "c1".into(),
            harness: String::new(),
            source: ActivitySource::Eslogger,
            kind: "open".into(),
            summary: String::new(),
            detail: json!({ "path": "/Users/t/.ssh/id_ed25519", "tier": "child" }),
            pid: Some(4),
            approval_id: None,
        };
        let reply = collector::EventsReply {
            events: vec![event],
            truncated: false,
            chain_ok: false,
            complete: false,
            problem: Some("timeout".into()),
        };
        let report = flags_report(
            &dir,
            Some(DATA),
            Some("c1"),
            0,
            100,
            &HashMap::new(),
            Some(reply),
            200,
        );
        assert_eq!(report.flags.len(), 1);
        assert_eq!(report.collector_reachable, Reachable::Yes);
        assert_eq!(report.collector_reason.as_deref(), Some("timeout"));
        assert!(report.truncated);
        assert_eq!(report.collector_chain_ok, None);
        let gone = collector::EventsReply {
            problem: Some("not running".into()),
            ..collector::EventsReply::default()
        };
        let report = flags_report(
            &dir,
            Some(DATA),
            Some("c1"),
            0,
            100,
            &HashMap::new(),
            Some(gone),
            200,
        );
        assert_eq!(report.collector_reachable, Reachable::No);
        assert_eq!(report.collector_reason.as_deref(), Some("not running"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_release_build_takes_only_the_bundled_collector() {
        // Review X11: an ancestor folder such as /Applications/macos/... is
        // writable by an admin user, so a release build must not look there.
        let root = temp("built-release");
        let exe = root.join("target/release/MonoCode.app/Contents/MacOS/MonoCode");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let ancestor = root
            .join("macos/activity-collector/target/release")
            .join(collector::BINARY_NAME);
        std::fs::create_dir_all(ancestor.parent().unwrap()).unwrap();
        std::fs::write(&ancestor, b"x").unwrap();
        assert_eq!(built_collector_from(&exe, None, None, true), None);
        assert_eq!(
            built_collector_from(&exe, None, None, false),
            Some(ancestor)
        );
        let bundled = exe.parent().unwrap().join(collector::BINARY_NAME);
        std::fs::write(&bundled, b"x").unwrap();
        assert_eq!(built_collector_from(&exe, None, None, true), Some(bundled));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_built_collector_is_found_beside_the_repository() {
        let root = temp("built");
        let exe = root.join("src-tauri/target/debug/monocode");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        assert!(built_collector_from(&exe, None, None, false).is_none());
        let bin = root
            .join("macos/activity-collector/target/release")
            .join(collector::BINARY_NAME);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"x").unwrap();
        assert_eq!(built_collector_from(&exe, None, None, false), Some(bin));
        let _ = std::fs::remove_dir_all(&root);
    }
}

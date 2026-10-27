//! B3 and B4 from the app's side: reading the collector over its read-only
//! socket, its status, and its install.
//!
//! The install never runs on its own (rules, section 5, item 2). The app
//! shows what will be installed and the exact commands first. Two routes:
//!
//! 1. `SMAppService`, from a signed app bundle that carries the daemon and
//!    its plist (macOS 13+). macOS then asks you to allow it in System
//!    Settings, Login Items. Not compiled in the cloud.
//! 2. A development build has no bundle, so the app writes the plist and
//!    shows the commands for you to run in Terminal yourself. Nothing here
//!    builds or runs a shell string.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::registry::READ_SOCKET;
use super::types::{ActivityEvent, ActivitySource};

/// The signed release's identifier. The daemon's names follow the running
/// app's identifier (`app.config().identifier`), so a dev build
/// (`com.monocode.desktop.dev`, decision 1) installs its own label.
pub const RELEASE_IDENTIFIER: &str = "com.monocode.desktop";
pub const BINARY_NAME: &str = "monocode-activity-collector";
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// An `events` reply waits while the collector verifies every day file in
/// the range before it answers (`log.rs` `read_events`).
const EVENTS_TIMEOUT: Duration = Duration::from_secs(15);

/// The daemon's launchd label and paths, from the app's identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Names {
    /// The app's identifier, the bundle that may register the daemon.
    pub app_identifier: String,
    /// `<identifier>.activity-collector`.
    pub label: String,
    pub daemon_path: String,
    pub plist_path: String,
}

impl Names {
    /// Names for an identifier, or `None` for one that could not sit in a
    /// path or a shell line unquoted.
    pub fn for_identifier(identifier: &str) -> Option<Names> {
        let ok = !identifier.is_empty()
            && identifier.len() <= 200
            && identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
            && !identifier.starts_with('.')
            && !identifier.contains("..");
        if !ok {
            return None;
        }
        let label = format!("{identifier}.activity-collector");
        Some(Names {
            app_identifier: identifier.to_string(),
            daemon_path: format!("/Library/PrivilegedHelperTools/{label}"),
            plist_path: format!("/Library/LaunchDaemons/{label}.plist"),
            label,
        })
    }

    pub fn release() -> Names {
        Names::for_identifier(RELEASE_IDENTIFIER).unwrap_or_else(|| unreachable!())
    }

    /// The running app's names.
    pub fn current() -> Names {
        Names::for_identifier(super::identifier()).unwrap_or_else(Names::release)
    }
}
const MAX_REPLY_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_EVENTS: usize = 10_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorStatus {
    /// macOS only.
    pub supported: bool,
    /// The plist is in place, or the app bundle registered it.
    pub installed: bool,
    /// The read socket answered.
    pub reachable: bool,
    /// Processes this app registered that are still running.
    pub registered: usize,
    /// The collector's own status line, as data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    /// Why it is not reachable, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// `eslogger: <its last stderr line>` while eslogger is not running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eslogger_problem: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventsReply {
    pub events: Vec<ActivityEvent>,
    pub truncated: bool,
    pub chain_ok: bool,
    /// The reply ended with its closing line.
    pub complete: bool,
    /// Why there is no complete reply: `timeout`, `truncated`, `not running`
    /// or the collector's own error (`refused`). `None` when complete.
    pub problem: Option<String>,
}

/// Reads an `events` reply. Lines that are not an `eslogger` event are
/// skipped, so the collector cannot put another source's record in the log.
pub fn parse_events_reply(reader: impl BufRead, limit: usize) -> EventsReply {
    let mut reply = EventsReply::default();
    let mut lines = reader.lines();
    loop {
        let line = match lines.next() {
            None => break,
            Some(Ok(line)) => line,
            Some(Err(error)) => {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) {
                    reply.problem = Some("timeout".into());
                }
                break;
            }
        };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value.get("ok").and_then(Value::as_bool) == Some(false) {
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("error");
            reply.problem = Some(error.chars().filter(|c| !c.is_control()).take(64).collect());
            break;
        }
        if value.get("end").and_then(Value::as_bool) == Some(true) {
            reply.truncated = value
                .get("truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            reply.chain_ok = value
                .get("chainOk")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            reply.complete = true;
            break;
        }
        let Ok(event) = serde_json::from_value::<ActivityEvent>(value) else {
            continue;
        };
        if event.source != ActivitySource::Eslogger {
            continue;
        }
        if reply.events.len() >= limit {
            reply.truncated = true;
            continue;
        }
        reply.events.push(super::store::bound_event(event));
    }
    if !reply.complete && reply.problem.is_none() {
        reply.problem = Some("truncated".into());
    }
    reply
}

pub fn events_request(since: i64, until: i64, chat_id: Option<&str>, limit: usize) -> String {
    let mut request = json!({
        "v": 1, "op": "events", "since": since, "until": until,
        "limit": limit.min(MAX_EVENTS)
    });
    if let Some(chat) = chat_id {
        request["chatId"] = json!(chat);
    }
    request.to_string()
}

#[cfg(unix)]
fn ask(
    request: &str,
    timeout: Duration,
) -> std::io::Result<BufReader<std::io::Take<std::os::unix::net::UnixStream>>> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(READ_SOCKET)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;
    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    Ok(BufReader::new(stream.take(MAX_REPLY_BYTES)))
}

/// The collector's events between two times. A reply that never completed
/// keeps what arrived and says why (Mac test 3.12b). Never an error to the
/// caller (case 9).
pub fn fetch_events(since: i64, until: i64, chat_id: Option<&str>, limit: usize) -> EventsReply {
    #[cfg(unix)]
    {
        match ask(
            &events_request(since, until, chat_id, limit),
            EVENTS_TIMEOUT,
        ) {
            Ok(reader) => parse_events_reply(reader, limit.min(MAX_EVENTS)),
            Err(_) => EventsReply {
                problem: Some("not running".into()),
                ..EventsReply::default()
            },
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (since, until, chat_id, limit);
        EventsReply {
            problem: Some("not running".into()),
            ..EventsReply::default()
        }
    }
}

/// eslogger's stopped reason from the collector's status, as
/// `eslogger: <line>`, or `None` while it runs. The line is the collector's,
/// capped and printable there; it is cut again here.
pub fn eslogger_problem(detail: &Value) -> Option<String> {
    let state = detail.get("eslogger").and_then(Value::as_str)?;
    if state == "running" {
        return None;
    }
    let line: String = detail
        .get("esloggerError")
        .and_then(Value::as_str)
        .unwrap_or(state)
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    Some(format!("eslogger: {line}"))
}

/// The collector's status line, or `None`. Short timeout: it is cheap.
pub fn fetch_status_detail() -> Option<Value> {
    #[cfg(unix)]
    {
        let mut reader = ask("{\"v\":1,\"op\":\"status\"}", READ_TIMEOUT).ok()?;
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        serde_json::from_str::<Value>(&line)
            .ok()
            .filter(|v| v.get("eslogger").is_some())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

pub fn status() -> CollectorStatus {
    let supported = cfg!(target_os = "macos");
    let names = Names::current();
    let installed = Path::new(&names.plist_path).exists() || bundled_registered();
    let mut status = CollectorStatus {
        supported,
        installed,
        registered: super::registry::live_count(),
        ..CollectorStatus::default()
    };
    #[cfg(unix)]
    {
        match ask("{\"v\":1,\"op\":\"status\"}", READ_TIMEOUT) {
            Ok(mut reader) => {
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        status.reachable = true;
                        // Why eslogger is not running, for the page (3.4).
                        status.eslogger_problem = eslogger_problem(&value);
                        status.detail = Some(value);
                    }
                }
                if !status.reachable {
                    status.problem =
                        Some("The collector answered with something unreadable.".into());
                }
            }
            Err(error) => {
                status.problem = Some(match error.kind() {
                    std::io::ErrorKind::NotFound => "The collector is not running.".to_string(),
                    std::io::ErrorKind::ConnectionRefused => {
                        "The collector refused this app. A development build needs the install that names it.".to_string()
                    }
                    _ => format!("The collector did not answer ({:?}).", error.kind()),
                });
            }
        }
    }
    status
}

/// What the install will do, shown before anything runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    /// `smAppService` from a signed bundle, `terminal` otherwise.
    pub route: String,
    /// Plain sentences: what runs as root, what it can see, what it keeps.
    pub explanation: Vec<String>,
    /// The launchd plist, as text.
    pub plist: String,
    /// Commands for you to run in Terminal (the terminal route), one a line.
    pub commands: Vec<String>,
    /// Where the plist was written for the terminal route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_plist: Option<String>,
    /// The installed daemon, the file to grant Full Disk Access to.
    #[serde(default)]
    pub daemon_path: String,
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The launchd plist. `dev_peer_path` is the unsigned development app the
/// daemon may accept; `None` for a signed build.
pub fn plist(label: &str, program: &str, dev_peer_path: Option<&str>) -> String {
    let mut args = vec![program.to_string(), "run".to_string()];
    if let Some(path) = dev_peer_path {
        args.push("--dev-peer-path".into());
        args.push(path.to_string());
    }
    let args: String = args
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>Label</key>\n\t<string>{label}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>KeepAlive</key>\n\t<true/>\n\
\t<key>ProcessType</key>\n\t<string>Background</string>\n\
</dict>\n</plist>\n"
    )
}

/// The plist a signed bundle carries in `Contents/Library/LaunchDaemons` for
/// `SMAppService` (lane B's request 1). The program is named relative to the
/// bundle, and only MonoCode's own bundle id may register it. The file at
/// `macos/activity-collector/bundle/` must match this text (tested).
#[cfg(test)]
pub fn bundled_plist_text(names: &Names) -> String {
    let label = &names.label;
    let app = &names.app_identifier;
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>Label</key>\n\t<string>{label}</string>\n\
\t<key>BundleProgram</key>\n\t<string>Contents/MacOS/monocode-activity-collector</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>monocode-activity-collector</string>\n\t\t<string>run</string>\n\t</array>\n\
\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t<string>{app}</string>\n\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>KeepAlive</key>\n\t<true/>\n\
\t<key>ProcessType</key>\n\t<string>Background</string>\n\
</dict>\n</plist>\n"
    )
}

/// A path is safe to put on a command line you will paste into a shell only
/// if it needs no quoting beyond single quotes and holds no single quote.
fn shell_quotable(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\'') && !path.contains('\0') && !path.contains('\n')
}

fn quoted(path: &str) -> String {
    format!("'{path}'")
}

/// The terminal route: the commands you run yourself, with the binary's
/// SHA-256 checked after it is copied to a root-owned place and before it is
/// started, so a file swapped in the staging folder never runs as root.
pub fn terminal_plan(
    names: &Names,
    built_binary: &str,
    binary_sha256: &str,
    staged_plist: &str,
    dev_peer_path: &str,
) -> Result<InstallPlan, String> {
    for path in [built_binary, staged_plist, dev_peer_path] {
        if !shell_quotable(path) {
            return Err(format!(
                "A path holds a character the install cannot show safely: {path:?}"
            ));
        }
    }
    if binary_sha256.len() != 64 || !binary_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("The collector binary's hash is not a SHA-256.".into());
    }
    // The plist is staged in a folder you own, so anything running as you
    // could swap it after the plan is shown. It is checked by hash too, once
    // it sits in a root-owned place, and nothing starts unless both checks
    // passed (security review S1).
    let plist_text = plist(&names.label, &names.daemon_path, Some(dev_peer_path));
    let (label, daemon_path, plist_path) = (&names.label, &names.daemon_path, &names.plist_path);
    let plist_sha256 = text_sha256(&plist_text);
    // Both files are installed root-owned, then one root shell checks both
    // hashes and starts the daemon only if both match, else removes both.
    // Every tool by absolute path: a planted `shasum` or `cut` earlier on
    // your PATH cannot vouch for a swapped file (security review T6).
    // Staged next to their final places as `.new`, checked there, and only
    // then moved over the installed ones: a reinstall never leaves an
    // unchecked binary where a running or restarting daemon would start it
    // as root (security review U8). The files move into place before the
    // old daemon stops, and a failed move or start says so (review V4).
    let binary_sha256 = binary_sha256.to_ascii_lowercase();
    let commands = vec![
        format!(
            "/usr/bin/sudo /usr/bin/install -o root -g wheel -m 755 {} {daemon_path}.new",
            quoted(built_binary)
        ),
        format!(
            "/usr/bin/sudo /usr/bin/install -o root -g wheel -m 644 {} {plist_path}.new",
            quoted(staged_plist)
        ),
        format!(
            "/usr/bin/sudo /bin/sh -c 'B=$(/usr/bin/shasum -a 256 {daemon_path}.new | /usr/bin/cut -d \" \" -f 1); P=$(/usr/bin/shasum -a 256 {plist_path}.new | /usr/bin/cut -d \" \" -f 1); if [ \"$B\" = \"{binary_sha256}\" ] && [ \"$P\" = \"{plist_sha256}\" ]; then /bin/mv -f {daemon_path}.new {daemon_path} && /bin/mv -f {plist_path}.new {plist_path} || {{ echo \"could not move the files into place, not installed\"; exit 1; }}; /bin/launchctl bootout system/{label} 2>/dev/null; /bin/launchctl bootstrap system {plist_path} || {{ echo \"collector not running, start it with: sudo /bin/launchctl bootstrap system {plist_path}\"; exit 1; }}; else echo \"hash mismatch, not installed\"; /bin/rm -f {daemon_path}.new {plist_path}.new; exit 1; fi'"
        ),
    ];
    let mut explanation = explanation(true);
    explanation.push(format!(
        "The binary installed is {built_binary}, SHA-256 {binary_sha256}."
    ));
    Ok(InstallPlan {
        route: "terminal".into(),
        explanation,
        plist: plist_text,
        commands,
        staged_plist: Some(staged_plist.to_string()),
        daemon_path: daemon_path.clone(),
    })
}

/// Every tool by absolute path, as in the install (Mac test 3.13).
pub fn removal_commands(names: &Names) -> Vec<String> {
    vec![
        format!(
            "/usr/bin/sudo /bin/launchctl bootout system/{}",
            names.label
        ),
        format!(
            "/usr/bin/sudo /bin/rm -f {} {}",
            names.plist_path, names.daemon_path
        ),
        "/usr/bin/sudo /bin/rm -rf /var/run/monocode-activity".into(),
        "# The log stays in /Library/Application Support/MonoCode/activity until you delete it."
            .into(),
    ]
}

pub fn explanation(dev_build: bool) -> Vec<String> {
    let mut lines = vec![
        "The collector runs as root, outside any sandbox, and starts /usr/bin/eslogger.".to_string(),
        "It needs Full Disk Access, which you grant it in System Settings, Privacy & Security.".to_string(),
        "It records only: processes MonoCode started for a chat and their children, and apps you granted for computer use while the grant holds.".to_string(),
        "It records what they ran, the files they created, wrote, moved or deleted, and opens of sensitive files such as ~/.ssh. Command lines are kept.".to_string(),
        "It blocks nothing. It sends nothing anywhere. Its log is root-owned, kept 7 days, at most 1 GB.".to_string(),
        "Only MonoCode may talk to it, checked by code signature.".to_string(),
    ];
    if dev_build {
        lines.push(
            "This is a development build, which is unsigned: the collector will accept this one app file by its path instead, which is weaker than a signature.".to_string(),
        );
        lines.push(
            "The commands check the collector binary and the plist against the hashes shown here before anything starts. The binary is the one in your build folder: a development install trusts whatever was built there.".to_string(),
        );
    }
    lines
}

/// Where a signed bundle carries the daemon's plist for `SMAppService`.
pub fn bundled_plist(exe: &Path, names: &Names) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    let plist = contents
        .join("Library/LaunchDaemons")
        .join(format!("{}.plist", names.label));
    plist.exists().then_some(plist)
}

fn bundled_registered() -> bool {
    #[cfg(target_os = "macos")]
    {
        sm::status() == Some(sm::ENABLED)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Registers the bundled daemon with `SMAppService`. macOS then shows its own
/// approval in System Settings.
pub fn register_bundled() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        sm::register()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("The collector runs only on macOS.".into())
    }
}

pub fn unregister_bundled() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        sm::unregister()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("The collector runs only on macOS.".into())
    }
}

/// `SMAppService.daemonServiceWithPlistName:`. Not compiled in the cloud:
/// the macOS target cannot be installed there (w3_session_0.md, point 12).
#[cfg(target_os = "macos")]
mod sm {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    pub const ENABLED: isize = 1;
    const REQUIRES_APPROVAL: isize = 2;

    fn service() -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"SMAppService")?;
        let name = NSString::from_str(&format!("{}.plist", super::Names::current().label));
        // SAFETY: a class method that returns an autoreleased SMAppService.
        unsafe { msg_send![class, daemonServiceWithPlistName: &*name] }
    }

    pub fn status() -> Option<isize> {
        let service = service()?;
        // SAFETY: `status` is a plain NSInteger property.
        Some(unsafe { msg_send![&*service, status] })
    }

    pub fn register() -> Result<String, String> {
        let service = service().ok_or("SMAppService is not available")?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: registerAndReturnError: takes an NSError** out-parameter.
        let ok: bool = unsafe { msg_send![&*service, registerAndReturnError: &mut error] };
        let state: isize = unsafe { msg_send![&*service, status] };
        if ok || state == REQUIRES_APPROVAL {
            return Ok(if state == ENABLED {
                "enabled".into()
            } else {
                "requiresApproval".into()
            });
        }
        Err("macOS refused to register the collector.".into())
    }

    pub fn unregister() -> Result<(), String> {
        let service = service().ok_or("SMAppService is not available")?;
        let mut error: *mut AnyObject = std::ptr::null_mut();
        // SAFETY: unregisterAndReturnError: takes an NSError** out-parameter.
        let ok: bool = unsafe { msg_send![&*service, unregisterAndReturnError: &mut error] };
        if ok {
            Ok(())
        } else {
            Err("macOS refused to unregister the collector.".into())
        }
    }
}

/// SHA-256 of a file, in lowercase hex.
pub fn text_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: &str = "com.monocode.desktop.activity-collector";
    const DAEMON_PATH: &str =
        "/Library/PrivilegedHelperTools/com.monocode.desktop.activity-collector";
    const PLIST_PATH: &str = "/Library/LaunchDaemons/com.monocode.desktop.activity-collector.plist";

    fn plan(binary: &str, hash: &str, staged: &str, peer: &str) -> Result<InstallPlan, String> {
        terminal_plan(&Names::release(), binary, hash, staged, peer)
    }

    #[test]
    fn names_follow_the_app_identifier() {
        // Decision 1: the dev build installs its own label and files.
        let release = Names::release();
        assert_eq!(release.label, LABEL);
        assert_eq!(release.daemon_path, DAEMON_PATH);
        assert_eq!(release.plist_path, PLIST_PATH);
        let dev = Names::for_identifier("com.monocode.desktop.dev").unwrap();
        assert_eq!(dev.label, "com.monocode.desktop.dev.activity-collector");
        assert_eq!(
            dev.plist_path,
            "/Library/LaunchDaemons/com.monocode.desktop.dev.activity-collector.plist"
        );
        let plan =
            terminal_plan(&dev, "/a/c", &"a".repeat(64), "/s/c.plist", "/m/MonoCode").unwrap();
        assert!(plan
            .plist
            .contains("<string>com.monocode.desktop.dev.activity-collector</string>"));
        assert!(plan.commands[2].contains("bootstrap system /Library/LaunchDaemons/com.monocode.desktop.dev.activity-collector.plist"));
        assert_eq!(plan.daemon_path, dev.daemon_path);
        assert!(bundled_plist_text(&dev).contains("<string>com.monocode.desktop.dev</string>"));
        for bad in ["", "a b", "a/b", "..x", "a;rm", ".a"] {
            assert!(Names::for_identifier(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn removal_uses_absolute_tools() {
        // Mac test 3.13.
        let commands = removal_commands(&Names::release());
        assert_eq!(
            commands[0],
            format!("/usr/bin/sudo /bin/launchctl bootout system/{LABEL}")
        );
        assert_eq!(
            commands[1],
            format!("/usr/bin/sudo /bin/rm -f {PLIST_PATH} {DAEMON_PATH}")
        );
        for command in commands.iter().filter(|c| !c.starts_with('#')) {
            assert!(command.starts_with("/usr/bin/sudo /bin/"), "{command}");
        }
    }

    #[test]
    fn an_incomplete_reply_keeps_its_events_and_says_why() {
        // Mac test 3.12b: a reply cut short was dropped whole.
        let reply = parse_events_reply(
            &br#"{"id":"es-1","at":5,"chatId":"c1","harness":"","source":"eslogger","kind":"exec","summary":"","detail":{}}
{"id":"es-2""#[..],
            10,
        );
        assert!(!reply.complete);
        assert_eq!(reply.events.len(), 1);
        assert_eq!(reply.problem.as_deref(), Some("truncated"));
        let refused = parse_events_reply(&b"{\"ok\":false,\"error\":\"refused\"}\n"[..], 10);
        assert_eq!(refused.problem.as_deref(), Some("refused"));
        let done = parse_events_reply(
            &b"{\"end\":true,\"truncated\":false,\"chainOk\":true}\n"[..],
            10,
        );
        assert_eq!(done.problem, None);
    }

    #[test]
    fn esloggers_reason_is_passed_on() {
        let stopped = json!({"eslogger": "stopped", "esloggerError": "Caller lacks TCC authorization for Full Disk Access"});
        assert_eq!(
            eslogger_problem(&stopped).as_deref(),
            Some("eslogger: Caller lacks TCC authorization for Full Disk Access")
        );
        assert_eq!(
            eslogger_problem(&json!({"eslogger": "missing"})).as_deref(),
            Some("eslogger: missing")
        );
        assert_eq!(eslogger_problem(&json!({"eslogger": "running"})), None);
    }

    #[test]
    fn an_events_reply_keeps_only_collector_events() {
        let text = [
            r#"{"id":"es-1","at":5,"chatId":"c1","harness":"","source":"eslogger","kind":"exec","summary":"ran ls","detail":{"argv":["ls"]},"pid":9}"#,
            r#"{"id":"x","at":6,"chatId":"c1","harness":"","source":"approval","kind":"k","summary":"","detail":{}}"#,
            "not json",
            r#"{"end":true,"truncated":false,"chainOk":true}"#,
            r#"{"id":"after","at":7,"chatId":"c1","harness":"","source":"eslogger","kind":"exec","summary":"","detail":{}}"#,
        ]
        .join("\n");
        let reply = parse_events_reply(text.as_bytes(), 10);
        assert_eq!(reply.events.len(), 1);
        assert_eq!(reply.events[0].id, "es-1");
        assert!(reply.complete && reply.chain_ok && !reply.truncated);
    }

    #[test]
    fn an_events_reply_cut_short_is_not_complete() {
        let reply = parse_events_reply(&b"{\"id\":\"es-1\""[..], 10);
        assert!(!reply.complete);
        assert!(!reply.chain_ok);
    }

    #[test]
    fn the_limit_holds() {
        let line = r#"{"id":"e","at":1,"chatId":"c","harness":"","source":"eslogger","kind":"exec","summary":"","detail":{}}"#;
        let text = [line; 5].join("\n");
        let reply = parse_events_reply(text.as_bytes(), 2);
        assert_eq!(reply.events.len(), 2);
        assert!(reply.truncated);
    }

    #[test]
    fn the_request_follows_the_protocol() {
        let value: Value = serde_json::from_str(&events_request(1, 2, Some("c1"), 50_000)).unwrap();
        assert_eq!(value["op"], "events");
        assert_eq!(value["limit"], MAX_EVENTS);
        assert_eq!(value["chatId"], "c1");
    }

    #[test]
    fn the_bundled_plist_file_matches_and_names_no_dev_path() {
        let text = bundled_plist_text(&Names::release());
        assert!(text.contains("<key>BundleProgram</key>"));
        assert!(text.contains("<string>com.monocode.desktop</string>"));
        assert!(!text.contains("dev-peer"));
        let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../macos/activity-collector/bundle")
            .join(format!("{LABEL}.plist"));
        assert_eq!(std::fs::read_to_string(file).unwrap(), text);
    }

    #[test]
    fn the_plist_escapes_and_names_the_dev_path() {
        let text = plist(LABEL, "/a/b", Some("/Users/t/My & <App>/monocode"));
        assert!(text.contains("<string>--dev-peer-path</string>"));
        assert!(text.contains("My &amp; &lt;App&gt;"));
        assert!(text.contains(LABEL));
        assert!(!plist(LABEL, "/a/b", None).contains("dev-peer"));
    }

    #[test]
    fn the_terminal_plan_checks_the_hash_before_it_starts_anything() {
        let hash = "a".repeat(64);
        let plan = plan(
            "/Users/t/build/collector",
            &hash,
            "/Users/t/stage/c.plist",
            "/Users/t/mono/MonoCode",
        )
        .unwrap();
        assert_eq!(plan.commands.len(), 3);
        assert!(plan.commands[0].contains(DAEMON_PATH));
        assert!(plan.commands[2].contains(&hash));
        assert!(plan.commands[2].contains("launchctl bootstrap system"));
        assert!(plan.explanation.iter().any(|l| l.contains("unsigned")));
    }

    #[test]
    fn the_plan_names_the_binary_it_installs_and_its_hash() {
        // Review X11: the explanation names the path as well as the hash.
        let hash = "b".repeat(64);
        let plan = plan(
            "/Users/t/build/collector",
            &hash,
            "/Users/t/stage/c.plist",
            "/Users/t/mono/MonoCode",
        )
        .unwrap();
        assert!(
            plan.explanation
                .iter()
                .any(|l| l.contains("/Users/t/build/collector") && l.contains(&hash)),
            "{:?}",
            plan.explanation
        );
    }

    #[test]
    fn the_staged_plist_is_checked_by_hash_before_anything_starts() {
        // Security review S1 and T6.
        let binary = "b".repeat(64);
        let plan = plan(
            "/Users/t/build/collector",
            &binary,
            "/Users/t/stage/c.plist",
            "/Users/t/mono/MonoCode",
        )
        .unwrap();
        let plist_hash = text_sha256(&plan.plist);
        assert!(plan.commands[1].contains(PLIST_PATH));
        let step = &plan.commands[2];
        assert!(step.starts_with("/usr/bin/sudo /bin/sh -c '"));
        assert!(step.contains(&plist_hash) && step.contains(&binary));
        // The start sits only in the branch where both hashes match.
        let (matched, mismatched) = step.split_once("; else ").unwrap();
        assert!(matched.contains("launchctl bootstrap system"));
        assert!(!mismatched.contains("launchctl"));
        assert!(mismatched.contains(&format!("/bin/rm -f {DAEMON_PATH}.new {PLIST_PATH}.new")));
        // Nothing lands on the installed paths before the check (U8).
        assert!(plan.commands[0].ends_with(&format!("{DAEMON_PATH}.new")));
        assert!(plan.commands[1].ends_with(&format!("{PLIST_PATH}.new")));
        assert!(matched.contains(&format!("/bin/mv -f {DAEMON_PATH}.new {DAEMON_PATH}")));
        assert!(!mismatched.contains("mv "));
        // The files move before the old daemon stops, and a failed move or
        // start says so (review V4).
        assert!(matched.find("/bin/mv -f").unwrap() < matched.find("bootout").unwrap());
        assert!(matched.contains("not installed\"; exit 1"));
        assert!(matched.contains(&format!(
            "start it with: sudo /bin/launchctl bootstrap system {PLIST_PATH}"
        )));
        // Every tool by absolute path; nothing found on PATH.
        for tool in ["shasum", "cut", "launchctl", "rm", "mv"] {
            for (at, _) in step.match_indices(tool) {
                assert_eq!(&step[at - 1..at], "/", "{tool} not by absolute path");
            }
        }
        for command in &plan.commands {
            assert!(command.starts_with("/usr/bin/sudo "));
        }
        // No single quote inside the root shell's own script.
        assert_eq!(step.matches('\'').count(), 2);
        assert_ne!(text_sha256(&plan.plist.replace("run", "rux")), plist_hash);
    }

    #[test]
    fn the_terminal_plan_refuses_paths_it_cannot_quote() {
        let hash = "a".repeat(64);
        assert!(plan("/a/it's", &hash, "/b", "/c").is_err());
        assert!(plan("relative", &hash, "/b", "/c").is_err());
        assert!(plan("/a", &hash, "/b\nrm -rf ~", "/c").is_err());
        assert!(plan("/a", "not-a-hash", "/b", "/c").is_err());
        assert!(
            plan("/a b/c", &hash, "/b", "/c").is_ok(),
            "spaces are quoted"
        );
    }

    #[test]
    fn status_without_a_collector_says_why() {
        if Path::new(READ_SOCKET).exists() {
            return;
        }
        let status = status();
        assert!(!status.reachable);
        assert!(status.problem.is_some());
        assert_eq!(status.supported, cfg!(target_os = "macos"));
    }

    #[test]
    fn fetch_without_a_collector_is_empty_and_fast() {
        let started = std::time::Instant::now();
        let reply = fetch_events(0, 1, None, 10);
        assert!(reply.events.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn file_hash_matches_a_known_value() {
        let path = std::env::temp_dir().join(format!("activity-hash-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            file_sha256(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&path);
    }
}

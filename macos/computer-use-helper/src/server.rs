//! The helper's socket: newline-delimited JSON with MonoCode.
//!
//! Requests carry an `id` and an `op`; replies carry the same `id` and `ok`.
//! Events (`record`, `exited`) carry `event` and no `id`.
//!
//! | op | takes | answers |
//! |---|---|---|
//! | `hello` | `version` | `helperVersion`, `driver` (found or not) |
//! | `start` | `runtime`, `manifest` | `pid`, `recording` (`on`, `error`) |
//! | `tools` | `runtime` | `tools` |
//! | `call` | `runtime`, `name`, `arguments` | `result` |
//! | `stop` | `runtime` | `ended`, `releasedButtons`, `warning` |
//! | `resolve` | `query` | `app` or null |
//!
//! `pid` is the runtime's one Cua process. `recording` is `{"on": true}`, or
//! `{"on": false, "error": "<at most 300 chars>"}` when Cua refused to record;
//! the runtime runs either way, and the helper then logs every call itself.
//!
//! A `record` event's `action` is Cua's turn (`turn: "turn-NNNNN"`) or the
//! helper's own (`turn: "call-NNNNNN"`, `source: "helper"`), with `tool`,
//! `arguments`, `error`, `at` and optional `pid`, `summary`, `screenshot`.
//!
//! Each request runs on its own thread, and each runtime has its own lock,
//! so a call in one chat never holds up another chat. A stop never waits
//! behind a running call: it ends the runtime's process first, which ends
//! the call (`runtime.rs`, `Ending`).

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::apps::{self, App};
use crate::lines::read_line_capped;
use crate::runtime::Runtimes;
use crate::stop::Mouse;

pub const PROTOCOL_VERSION: u64 = 1;
/// The largest request line: a manifest or a call's arguments.
const MAX_REQUEST: usize = 1024 * 1024;
const MAX_IN_FLIGHT: usize = 16;

pub struct Shared {
    /// Locks each runtime on its own.
    pub runtimes: Runtimes,
    pub mouse: Box<dyn Mouse + Send + Sync>,
    pub home: PathBuf,
    pub driver_found: bool,
    apps: OnceLock<Vec<App>>,
    in_flight: AtomicUsize,
}

impl Shared {
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "called only from the macOS `run` in main.rs")
    )]
    pub fn new(
        runtimes: Runtimes,
        mouse: Box<dyn Mouse + Send + Sync>,
        home: PathBuf,
        driver_found: bool,
    ) -> Self {
        Self {
            runtimes,
            mouse,
            home,
            driver_found,
            apps: OnceLock::new(),
            in_flight: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub fn with_apps(self, apps: Vec<App>) -> Self {
        let _ = self.apps.set(apps);
        self
    }
}

type Writer = Arc<Mutex<UnixStream>>;

fn send(writer: &Writer, message: &Value) {
    if let Ok(mut stream) = writer.lock() {
        let mut line = message.to_string();
        line.push('\n');
        let _ = stream.write_all(line.as_bytes());
    }
}

fn ok(id: &Value, mut body: Value) -> Value {
    if let Some(map) = body.as_object_mut() {
        map.insert("id".into(), id.clone());
        map.insert("ok".into(), json!(true));
    }
    body
}

fn fail(id: &Value, message: impl Into<String>) -> Value {
    json!({"id": id, "ok": false, "error": message.into()})
}

fn str_field<'a>(request: &'a Value, key: &str) -> Option<&'a str> {
    request.get(key).and_then(Value::as_str)
}

/// One request, answered.
pub fn handle(shared: &Shared, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if !(id.is_string() || id.is_u64()) {
        return fail(&Value::Null, "bad id");
    }
    match str_field(request, "op") {
        Some("hello") => ok(
            &id,
            json!({"helperVersion": env!("CARGO_PKG_VERSION"), "protocol": PROTOCOL_VERSION, "driver": shared.driver_found}),
        ),
        Some("start") => {
            let (Some(runtime), Some(manifest)) = (
                str_field(request, "runtime"),
                str_field(request, "manifest"),
            ) else {
                return fail(&id, "start needs runtime and manifest");
            };
            match shared
                .runtimes
                .start(runtime, manifest, shared.mouse.as_ref())
            {
                Ok(started) => ok(
                    &id,
                    json!({"pid": started.pid, "recording": started.recording}),
                ),
                Err(error) => fail(&id, error),
            }
        }
        Some("tools") => {
            let Some(runtime) = str_field(request, "runtime") else {
                return fail(&id, "tools needs runtime");
            };
            match shared.runtimes.tools(runtime) {
                Ok(tools) => ok(&id, json!({"tools": tools})),
                Err(error) => fail(&id, error),
            }
        }
        Some("call") => {
            let (Some(runtime), Some(name)) =
                (str_field(request, "runtime"), str_field(request, "name"))
            else {
                return fail(&id, "call needs runtime and name");
            };
            let arguments = request
                .get("arguments")
                .cloned()
                .filter(Value::is_object)
                .unwrap_or_else(|| json!({}));
            match shared.runtimes.call(runtime, name, arguments) {
                Ok(result) => ok(&id, json!({"result": result})),
                Err(error) => fail(&id, error),
            }
        }
        Some("stop") => {
            let Some(runtime) = str_field(request, "runtime") else {
                return fail(&id, "stop needs runtime");
            };
            let report = stop_now(shared, runtime);
            ok(
                &id,
                json!({"ended": report.ended, "releasedButtons": report.released, "warning": report.warning}),
            )
        }
        Some("resolve") => {
            let Some(query) = str_field(request, "query") else {
                return fail(&id, "resolve needs query");
            };
            let list = shared.apps.get_or_init(|| apps::installed(&shared.home));
            ok(
                &id,
                json!({"app": apps::find(query, list).map(App::to_json)}),
            )
        }
        _ => fail(&id, "unknown op"),
    }
}

/// Ends a runtime without waiting for a call in flight: `stop_recording`
/// when the runtime is free, else a signal that ends the call; then the Cua
/// session and process, then the mouse (`runtime.rs`, `Ending`).
pub fn stop_now(shared: &Shared, runtime: &str) -> crate::stop::StopReport {
    shared.runtimes.stop(runtime, shared.mouse.as_ref())
}

/// Serves one verified connection until it closes. Every runtime ends when
/// it does, so no grant outlives MonoCode's connection.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "called only from the macOS `run` in main.rs")
)]
pub fn serve(shared: Arc<Shared>, stream: UnixStream) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let writer: Writer = Arc::new(Mutex::new(stream));
    let open = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Records and ended runtimes, every half second.
    {
        let shared = shared.clone();
        let writer = writer.clone();
        let open = open.clone();
        thread::spawn(move || {
            while open.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(500));
                // A runtime a call holds is read on a later tick.
                let polled = shared.runtimes.poll();
                for (runtime, action) in polled.0 {
                    send(
                        &writer,
                        &json!({"event": "record", "runtime": runtime, "action": action}),
                    );
                }
                for (runtime, code) in polled.1 {
                    send(
                        &writer,
                        &json!({"event": "exited", "runtime": runtime, "code": code}),
                    );
                }
            }
        });
    }

    let mut reader = BufReader::new(reader);
    loop {
        let line = match read_line_capped(&mut reader, MAX_REQUEST) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                send(&writer, &fail(&Value::Null, "request too large"));
                continue;
            }
            Err(_) => break,
        };
        let Ok(request) = serde_json::from_slice::<Value>(&line) else {
            send(&writer, &fail(&Value::Null, "bad json"));
            continue;
        };
        if shared.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
            shared.in_flight.fetch_sub(1, Ordering::SeqCst);
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            send(&writer, &fail(&id, "too many requests at once"));
            continue;
        }
        let shared = shared.clone();
        let writer = writer.clone();
        thread::spawn(move || {
            let reply = handle(&shared, &request);
            shared.in_flight.fetch_sub(1, Ordering::SeqCst);
            send(&writer, &reply);
        });
    }
    open.store(false, Ordering::SeqCst);
    shared.runtimes.stop_all(shared.mouse.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stop::Button;
    use std::fs;

    struct StillMouse;
    impl Mouse for StillMouse {
        fn held(&self, _button: Button) -> Option<bool> {
            Some(false)
        }
        fn release(&self, _button: Button) -> bool {
            true
        }
    }

    fn shared(name: &str) -> (PathBuf, Arc<Shared>) {
        let root = std::env::temp_dir().join(format!("cu-srv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let runtimes = Runtimes::new(
            root.join("no-driver"),
            root.join("data"),
            root.join("s"),
            root.clone(),
        );
        let shared =
            Shared::new(runtimes, Box::new(StillMouse), root.clone(), false).with_apps(vec![App {
                bundle_id: "com.apple.calculator".into(),
                name: Some("Calculator".into()),
                path: "/System/Applications/Calculator.app".into(),
            }]);
        (root, Arc::new(shared))
    }

    /// A stand-in for `cua-driver mcp --direct --embedded` at Cua 0.37.0: it
    /// checks its arguments and environment, reads the manifest file once at
    /// start, serves one request at a time, and exits on stdin EOF. It logs
    /// what it did to `driver.log` beside itself.
    const FAKE_DRIVER: &str = r#"#!/bin/sh
log="$(dirname "$0")/driver.log"
say() { printf '%s\n' "$*" >> "$log"; }
[ "$*" = "mcp --direct --embedded" ] || { say "bad args: $*"; exit 64; }
[ "$CUA_DRIVER_EMBEDDED" = 1 ] || { say "not embedded"; exit 65; }
[ -n "$CUA_DRIVER_HOST_BUNDLE_ID" ] || { say "no host"; exit 65; }
[ "$CUA_DRIVER_PERMISSION_MODE" = bounded ] || { say "not bounded"; exit 65; }
[ "$CUA_DRIVER_RS_TELEMETRY_ENABLED" = false ] || { say "telemetry on"; exit 65; }
[ "$CUA_DRIVER_CAPABILITY_MANIFEST_APPROVED" = 1 ] || { say "not approved"; exit 65; }
[ -f "$CUA_DRIVER_CAPABILITY_MANIFEST_FILE" ] || { say "no manifest"; exit 65; }
[ -z "$CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS" ] || exit 65
manifest=$(cat "$CUA_DRIVER_CAPABILITY_MANIFEST_FILE")
say "start pid=$$ $*"
case "$manifest" in *'# fake: tamper'*) printf '# changed\n' >> "$CUA_DRIVER_CAPABILITY_MANIFEST_FILE" ;; esac
out=""
turn=0
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$1"; }
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
  *'"initialize"'*)
    say initialize
    reply '{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake"}}' ;;
  *'"tools/list"'*)
    reply '{"tools":[{"name":"click","annotations":{"readOnlyHint":false}},{"name":"drag","annotations":{"readOnlyHint":false}},{"name":"get_window_state","annotations":{"readOnlyHint":true}},{"name":"get_desktop_state","annotations":{"readOnlyHint":true}},{"name":"start_recording","annotations":{"readOnlyHint":false}},{"name":"set_config","annotations":{"readOnlyHint":false}}]}' ;;
  *'"start_recording"'*)
    say start_recording
    dir=$(printf '%s' "$line" | sed -n 's/.*"output_dir":"\([^"]*\)".*/\1/p')
    case "$manifest" in
    *'# fake: refuse-rpc'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"no recording here"}}\n' "$id" ;;
    *'# fake: refuse-status'*)
      reply '{"content":[{"type":"text","text":"refused"}],"structuredContent":{"status":"refused","refusal":{"code":"x","message":"refused"}}}' ;;
    *'# fake: refuse-iserror'*)
      reply '{"content":[{"type":"text","text":"bounded_resource_outside_manifest"}],"isError":true}' ;;
    *"- dir: \"$dir\""*)
      out="$dir"
      reply '{"content":[{"type":"text","text":"Recording started"}]}' ;;
    *)
      reply '{"content":[{"type":"text","text":"bounded_resource_outside_manifest"}],"isError":true,"structuredContent":{"status":"refused"}}' ;;
    esac ;;
  *'"stop_recording"'*)
    say stop_recording
    out=""
    reply '{"content":[{"type":"text","text":"stopped"}]}' ;;
  *'"get_window_state"'*)
    say "call get_window_state"
    reply '{"content":[{"type":"text","text":"window 1"},{"type":"image","data":"iVBORw0KGgpmYWtlLXBuZw==","mimeType":"image/png"}]}' ;;
  *'"name":"drag"'*)
    case "$manifest" in
    *'# fake: hang'*)
      # Never answers, and leaves a child holding stdout, so no EOF comes.
      sleep 600 </dev/null 2>/dev/null &
      printf '%s\n' "$!" >> "$(dirname "$0")/hang.pids"
      say "call drag"
      wait ;;
    esac
    say "call drag"
    sleep 5 </dev/null >/dev/null
    reply '{"content":[{"type":"text","text":"dragged"}]}' ;;
  *'"tools/call"'*)
    name=$(printf '%s' "$line" | sed -n 's/.*"name":"\([a-z_]*\)".*/\1/p')
    say "call $name"
    if [ -n "$out" ]; then
      turn=$((turn + 1))
      t=$(printf 'turn-%05d' "$turn")
      mkdir -p "$out/$t"
      printf '{"tool":"%s","arguments":{"pid":501},"result_summary":"ok","result_error":false,"timestamp":"2026-10-20T10:00:00.000Z","t_ms_from_session_start":5,"t_start_ms_from_session_start":1}' "$name" > "$out/$t/action.json"
    fi
    reply '{"content":[{"type":"text","text":"ok"}]}' ;;
  esac
done
say eof
"#;

    const MANIFEST: &str = "# Written by MonoCode for one chat. Do not edit.\nversion: 3\nexpires_after: \"8h\"\nresources:\n  desktop:\n    display: false\n";

    /// MonoCode's manifest with a line that steers the fake driver.
    fn manifest_with(marker: &str) -> String {
        MANIFEST.replacen("\n", &format!("\n# fake: {marker}\n"), 1)
    }

    /// A mouse whose left button reads as held until released, logging the
    /// release to the driver's log so its order against the driver shows.
    struct LogMouse {
        log: PathBuf,
        held: Arc<std::sync::atomic::AtomicBool>,
    }
    impl Mouse for LogMouse {
        fn held(&self, button: Button) -> Option<bool> {
            Some(button == Button::Left && self.held.load(Ordering::SeqCst))
        }
        fn release(&self, button: Button) -> bool {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.log)
                .unwrap();
            writeln!(file, "release {}", button.name()).unwrap();
            self.held.store(false, Ordering::SeqCst);
            true
        }
    }

    fn with_driver(name: &str) -> (PathBuf, Arc<Shared>) {
        let (root, shared, _) = with_driver_and_mouse(name);
        (root, shared)
    }

    fn with_driver_and_mouse(
        name: &str,
    ) -> (PathBuf, Arc<Shared>, Arc<std::sync::atomic::AtomicBool>) {
        let root = std::env::temp_dir().join(format!("cu-drv-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        let driver = root.join("bin").join("cua-driver");
        fs::write(&driver, FAKE_DRIVER).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
            fs::create_dir_all(root.join("s")).unwrap();
            fs::set_permissions(root.join("s"), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mouse = LogMouse {
            log: root.join("bin").join("driver.log"),
            held: held.clone(),
        };
        let runtimes = Runtimes::new(driver, root.join("data"), root.join("s"), root.clone());
        let shared = Shared::new(runtimes, Box::new(mouse), root.clone(), true);
        (root, Arc::new(shared), held)
    }

    fn driver_log(root: &std::path::Path) -> Vec<String> {
        fs::read_to_string(root.join("bin").join("driver.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn start(shared: &Shared, runtime: &str, manifest: &str) -> Value {
        handle(
            shared,
            &json!({"id": 1, "op": "start", "runtime": runtime, "manifest": manifest}),
        )
    }

    fn call(shared: &Shared, runtime: &str, name: &str) -> Value {
        handle(
            shared,
            &json!({"id": 2, "op": "call", "runtime": runtime, "name": name, "arguments": {"pid": 501}}),
        )
    }

    fn stop(shared: &Shared, runtime: &str) -> Value {
        handle(shared, &json!({"id": 3, "op": "stop", "runtime": runtime}))
    }

    fn poll(shared: &Shared) -> crate::runtime::Polled {
        shared.runtimes.poll()
    }

    /// Every file under `dir`, at any depth.
    fn files_under(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                    out.extend(files_under(&path));
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    fn wait_for_line(root: &std::path::Path, line: &str) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if driver_log(root).iter().any(|l| l == line) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn start_runs_one_direct_cua_child_and_no_socket() {
        let (root, shared) = with_driver("direct");
        let reply = start(&shared, "rt", MANIFEST);
        assert_eq!(reply["ok"], true, "{reply}");
        let pid = reply["pid"].as_u64().unwrap();
        let log = driver_log(&root);
        let starts: Vec<&String> = log.iter().filter(|l| l.starts_with("start ")).collect();
        assert_eq!(
            starts,
            vec![&format!("start pid={pid} mcp --direct --embedded")],
            "one child, and the pid is its pid"
        );
        assert!(
            !files_under(&root).iter().any(|f| f
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".sock"))),
            "no runtime socket"
        );
        assert!(
            fs::read_dir(root.join("s")).unwrap().next().is_none(),
            "the manifest and its folder are gone once Cua answered initialize"
        );
        assert_eq!(stop(&shared, "rt")["ended"], true);
        assert!(wait_for_line(&root, "eof"), "Stop closed its stdin");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manifest_changed_before_cua_read_it_fails_the_start() {
        let (root, shared) = with_driver("tamper");
        let reply = start(&shared, "rt", &manifest_with("tamper"));
        assert_eq!(reply["ok"], false);
        assert!(
            reply["error"]
                .as_str()
                .unwrap()
                .contains("the manifest changed before Cua read it"),
            "{reply}"
        );
        assert!(fs::read_dir(root.join("s")).unwrap().next().is_none());
        assert_eq!(call(&shared, "rt", "click")["ok"], false, "nothing runs");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn recording_gets_a_fresh_write_folder_each_start() {
        let (root, shared) = with_driver("rec");
        let first = start(&shared, "rt", MANIFEST);
        assert_eq!(first["recording"], json!({"on": true}), "{first}");
        assert_eq!(call(&shared, "rt", "click")["ok"], true);
        let (records, _) = poll(&shared);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].1["turn"], "turn-00001");
        assert_eq!(records[0].1["tool"], "click");
        assert!(records[0].1.get("source").is_none(), "Cua's own record");
        let second = start(&shared, "rt", MANIFEST);
        assert_eq!(second["recording"], json!({"on": true}));
        assert_eq!(call(&shared, "rt", "click")["ok"], true);
        let (records, _) = poll(&shared);
        assert_eq!(records.len(), 1);
        let starts: Vec<PathBuf> = fs::read_dir(root.join("data").join("recordings").join("rt"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        assert_eq!(starts.len(), 2, "one folder per start: {starts:?}");
        for folder in starts {
            assert!(folder.join("turn-00001").join("action.json").is_file());
        }
        stop(&shared, "rt");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refused_start_recording_turns_recording_off_and_the_runtime_runs() {
        let (root, shared) = with_driver("refused");
        for marker in ["refuse-iserror", "refuse-status", "refuse-rpc"] {
            let reply = start(&shared, "rt", &manifest_with(marker));
            assert_eq!(reply["ok"], true, "{marker}: {reply}");
            assert_eq!(reply["recording"]["on"], false, "{marker}: {reply}");
            let error = reply["recording"]["error"].as_str().unwrap();
            assert!(!error.is_empty() && error.chars().count() <= 300, "{error}");
            assert_eq!(call(&shared, "rt", "click")["ok"], true, "{marker}");
        }
        stop(&shared, "rt");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn calls_cua_never_records_are_logged_by_the_helper_in_order() {
        let (root, shared) = with_driver("helperlog");
        assert_eq!(start(&shared, "rt", MANIFEST)["recording"]["on"], true);
        for tool in ["click", "get_window_state", "click"] {
            assert_eq!(call(&shared, "rt", tool)["ok"], true, "{tool}");
        }
        let (records, _) = poll(&shared);
        let turns: Vec<&str> = records
            .iter()
            .map(|(_, r)| r["turn"].as_str().unwrap())
            .collect();
        assert_eq!(turns, ["turn-00001", "call-000001", "turn-00002"]);
        let own = &records[1].1;
        assert_eq!(own["source"], "helper");
        assert_eq!(own["tool"], "get_window_state");
        assert_eq!(own["error"], false);
        assert_eq!(own["summary"], "window 1");
        assert_eq!(own["arguments"], json!({"pid": 501}));
        let at = own["at"].as_str().unwrap();
        assert!(
            at.len() == 24 && at.ends_with('Z') && at.as_bytes()[10] == b'T',
            "{at}"
        );
        let shot = PathBuf::from(own["screenshot"].as_str().unwrap());
        assert!(shot.ends_with("calls/call-000001.png"), "{shot:?}");
        assert!(shot.starts_with(
            root.join("data")
                .join("recordings")
                .join("rt")
                .canonicalize()
                .unwrap()
        ));
        assert_eq!(fs::read(&shot).unwrap(), b"\x89PNG\r\n\x1a\nfake-png");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&shot).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        // With recording off, every call is the helper's to log.
        assert_eq!(
            start(&shared, "off", &manifest_with("refuse-iserror"))["recording"]["on"],
            false
        );
        assert_eq!(call(&shared, "off", "click")["ok"], true);
        let (records, _) = poll(&shared);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "off");
        assert_eq!(records[0].1["source"], "helper");
        assert_eq!(records[0].1["tool"], "click");
        assert!(records[0].1.get("screenshot").is_none());
        stop(&shared, "rt");
        stop(&shared, "off");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_ends_recording_then_cua_then_releases_the_mouse() {
        let (root, shared, held) = with_driver_and_mouse("order");
        assert_eq!(start(&shared, "rt", MANIFEST)["ok"], true);
        assert_eq!(call(&shared, "rt", "click")["ok"], true);
        fs::write(root.join("bin").join("driver.log"), "").unwrap();
        // The click "left the button held".
        held.store(true, Ordering::SeqCst);
        let reply = stop(&shared, "rt");
        assert_eq!(reply["ended"], true);
        assert_eq!(reply["releasedButtons"], json!(["left"]));
        assert_eq!(
            driver_log(&root),
            ["stop_recording", "eof", "release left"],
            "stop_recording, then the Cua session and process, then the mouse"
        );
        // The record of the last call still reaches MonoCode after the stop.
        let (records, exited) = poll(&shared);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].1["tool"], "click");
        assert!(
            exited.is_empty(),
            "a stopped runtime is not reported as exited"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_call_in_one_chat_does_not_block_another_chats_start_or_stop() {
        let (root, shared) = with_driver("parallel");
        assert_eq!(start(&shared, "a", MANIFEST)["ok"], true);
        assert_eq!(start(&shared, "b", MANIFEST)["ok"], true);
        let busy = {
            let shared = shared.clone();
            thread::spawn(move || call(&shared, "a", "drag"))
        };
        assert!(wait_for_line(&root, "call drag"));
        let started = std::time::Instant::now();
        assert_eq!(start(&shared, "c", MANIFEST)["ok"], true);
        assert_eq!(stop(&shared, "b")["ended"], true);
        let _ = poll(&shared);
        assert!(
            started.elapsed() < Duration::from_millis(2500),
            "another chat waited {:?} behind the call",
            started.elapsed()
        );
        // And a stop of the busy chat does not wait for its call either.
        let started = std::time::Instant::now();
        assert_eq!(stop(&shared, "a")["ok"], true);
        assert!(
            started.elapsed() < Duration::from_millis(3000),
            "stop waited {:?}",
            started.elapsed()
        );
        assert_eq!(
            busy.join().unwrap()["ok"],
            false,
            "the call ended with its runtime"
        );
        stop(&shared, "c");
        let _ = fs::remove_dir_all(&root);
    }

    /// Security review X9: a call that never answers, while something Cua
    /// started keeps its stdout open, must not hold Stop until the call
    /// timeout. Stop cancels the call, ends Cua, and releases the mouse.
    #[test]
    fn stop_does_not_wait_for_a_hung_call_whose_stdout_stays_open() {
        let (root, shared, held) = with_driver_and_mouse("hung");
        assert_eq!(start(&shared, "rt", &manifest_with("hang"))["ok"], true);
        let busy = {
            let shared = shared.clone();
            thread::spawn(move || call(&shared, "rt", "drag"))
        };
        assert!(wait_for_line(&root, "call drag"));
        held.store(true, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let shared = shared.clone();
            thread::spawn(move || {
                let _ = tx.send(stop(&shared, "rt"));
            });
        }
        let started = std::time::Instant::now();
        let reply = rx.recv_timeout(Duration::from_secs(5));
        let waited = started.elapsed();
        let kill_children = || {
            for pid in fs::read_to_string(root.join("bin").join("hang.pids"))
                .unwrap_or_default()
                .lines()
            {
                let _ = std::process::Command::new("kill").arg(pid.trim()).status();
            }
        };
        let Ok(reply) = reply else {
            kill_children();
            panic!("stop still waiting after {waited:?}");
        };
        kill_children();
        assert!(waited < Duration::from_secs(2), "stop waited {waited:?}");
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["releasedButtons"], json!(["left"]), "{reply}");
        assert!(driver_log(&root).iter().any(|l| l == "release left"));
        let call = busy.join().unwrap();
        assert_eq!(call["ok"], false, "{call}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn answers_each_op_by_id() {
        let (root, shared) = shared("ops");
        let hello = handle(&shared, &json!({"id": 1, "op": "hello", "version": 1}));
        assert_eq!(hello["ok"], true);
        assert_eq!(hello["id"], 1);
        assert_eq!(hello["driver"], false);
        let resolved = handle(
            &shared,
            &json!({"id": "r", "op": "resolve", "query": "calculator"}),
        );
        assert_eq!(resolved["app"]["bundleId"], "com.apple.calculator");
        let none = handle(&shared, &json!({"id": 2, "op": "resolve", "query": "Nope"}));
        assert!(none["app"].is_null());
        let start = handle(
            &shared,
            &json!({"id": 3, "op": "start", "runtime": "rt", "manifest": "# Written by MonoCode for one chat. Do not edit.\nversion: 3\n  desktop:\n    display: false\n"}),
        );
        assert_eq!(start["ok"], false, "no driver");
        assert!(start["error"].as_str().unwrap().contains("missing"));
        let call = handle(
            &shared,
            &json!({"id": 4, "op": "call", "runtime": "rt", "name": "click"}),
        );
        assert_eq!(call["ok"], false);
        let stop = handle(&shared, &json!({"id": 5, "op": "stop", "runtime": "rt"}));
        assert_eq!(stop["ok"], true);
        assert_eq!(stop["ended"], true);
        assert_eq!(
            handle(&shared, &json!({"id": 6, "op": "rm -rf"}))["ok"],
            false
        );
        assert_eq!(
            handle(&shared, &json!({"id": {"a": 1}, "op": "hello"}))["ok"],
            false
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_connection_answers_lines_and_survives_junk() {
        use std::io::BufRead;
        let (root, shared) = shared("conn");
        let (ours, theirs) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || serve(shared, theirs));
        let mut writer = ours.try_clone().unwrap();
        writer
            .write_all(b"not json\n{\"id\":7,\"op\":\"hello\"}\n")
            .unwrap();
        let mut reader = BufReader::new(ours.try_clone().unwrap());
        let mut replies = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            replies.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        assert!(replies.iter().any(|r| r["error"] == "bad json"));
        assert!(replies.iter().any(|r| r["id"] == 7 && r["ok"] == true));
        drop(writer);
        ours.shutdown(std::net::Shutdown::Both).unwrap();
        server.join().unwrap();
        let _ = fs::remove_dir_all(&root);
    }
}

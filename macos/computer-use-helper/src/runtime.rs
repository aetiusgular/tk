//! One Cua runtime per chat: one `cua-driver mcp --direct --embedded`
//! child, bounded by that chat's manifest, talked to over its stdio. No
//! socket (Mac test finding 4.14).
//!
//! Each runtime sits behind its own lock, so a call in one chat never holds
//! up another chat's start or stop. The map of runtimes is locked only to
//! look one up, never while a runtime is in use.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::base64;
use crate::driver::{direct_launch, runtime_id, Launch};
use crate::mcp_client::McpClient;
use crate::recording::{self, Scanner};
use crate::stop::{self, Mouse, Processes, StopReport};
use crate::tools::{output_path_argument, relayable};

/// The most runtimes at once, one per chat.
pub const MAX_RUNTIMES: usize = 8;
/// The largest manifest taken from MonoCode.
pub const MAX_MANIFEST: usize = 64 * 1024;
const INIT_TIMEOUT: Duration = Duration::from_secs(20);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Stop does not wait longer than this for Cua to close its recording.
const STOP_RECORDING_TIMEOUT: Duration = Duration::from_secs(2);
/// The largest screenshot the helper writes for a call it logs itself.
const MAX_SCREENSHOT: usize = 16 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
/// How MonoCode's manifest ends: the desktop block, under `resources:`
/// (`src-tauri/src/computer_use/manifest.rs`).
const MANIFEST_TAIL: &str = "  desktop:\n    display: false\n";
/// The longest error text the `start` reply carries for recording.
const MAX_RECORDING_ERROR: usize = 300;

struct Runtime {
    child: Child,
    client: McpClient,
    scanner: Scanner,
    /// This start's recording folder. Cua writes its turns here, the helper
    /// its own screenshots under `calls/`.
    recording_dir: PathBuf,
    recording_on: bool,
    /// Tools whose `annotations.readOnlyHint` is true. Cua never records
    /// those (`cua-driver-core/src/tool.rs:2244-2255`), so the helper does.
    read_only: HashSet<String>,
    /// Records not yet sent, Cua's and the helper's, in call order.
    queue: Vec<Value>,
    calls: u32,
}

struct Slot {
    pid: u32,
    /// Set once the child is reaped, so its pid, which the system may hand
    /// out again, is never signalled. Reaping happens only under `runtime`.
    reaped: AtomicBool,
    /// The runtime client's cancel flag, reachable without `runtime`'s lock,
    /// which a call in flight holds.
    cancel: Arc<AtomicBool>,
    runtime: Mutex<Runtime>,
}

/// What a start reports: the Cua process and whether it records.
#[derive(Debug)]
pub struct Started {
    pub pid: u32,
    /// `{"on": true}`, or `{"on": false, "error": "..."}`.
    pub recording: Value,
}

pub struct Runtimes {
    driver: PathBuf,
    /// Recordings, under MonoCode's data folder.
    data: PathBuf,
    /// The helper's own private folder (the parent of its `--socket`, which
    /// MonoCode makes 0700 with a random name). Each start's manifest goes in
    /// a fresh private folder inside it.
    private: PathBuf,
    home: PathBuf,
    running: Mutex<HashMap<String, Arc<Slot>>>,
    /// Records from runtimes that stopped or ended, for the next poll.
    pending: Arc<Mutex<Vec<(String, Value)>>>,
    starts: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn(launch: &Launch) -> std::io::Result<Child> {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args).env_clear();
    for (key, value) in &launch.env {
        command.env(key, value);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0100;
#[cfg(all(unix, not(target_os = "macos")))]
const O_NOFOLLOW: i32 = 0o400000;

/// Writes a new file only this user can read. Fails if anything is there,
/// a link included.
fn write_new_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

/// A file's bytes, opened without following a link, up to `max` bytes.
fn read_nofollow(path: &Path, max: usize) -> Option<Vec<u8>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options.open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= max).then_some(bytes)
}

pub fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_dir() {
            return Err(format!("{} is not a folder", path.display()));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Makes a folder that must not exist yet, 0700.
fn new_private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|e| format!("Cannot make {}: {e}", path.display()))
}

fn random_hex() -> Result<String, String> {
    let mut bytes = [0u8; 12];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("No random name: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Runtimes {
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "called only from the macOS `run` in main.rs")
    )]
    pub fn new(driver: PathBuf, data: PathBuf, private: PathBuf, home: PathBuf) -> Self {
        Self {
            driver,
            data,
            private,
            home,
            running: Mutex::new(HashMap::new()),
            pending: Arc::new(Mutex::new(Vec::new())),
            starts: AtomicU64::new(0),
        }
    }

    fn slot(&self, id: &str) -> Option<Arc<Slot>> {
        lock(&self.running).get(id).cloned()
    }

    #[cfg(test)]
    pub fn is_running(&self, id: &str) -> bool {
        lock(&self.running).contains_key(id)
    }

    /// Starts a runtime for `id` with this manifest, replacing one already
    /// running under that id.
    pub fn start(&self, id: &str, manifest: &str, mouse: &dyn Mouse) -> Result<Started, String> {
        let id = runtime_id(id).ok_or("Bad runtime id")?.to_string();
        if !manifest_acceptable(manifest) {
            return Err("Bad manifest".into());
        }
        if self.slot(&id).is_some() {
            self.stop(&id, mouse);
        }
        if lock(&self.running).len() >= MAX_RUNTIMES {
            return Err(format!(
                "At most {MAX_RUNTIMES} chats can use the computer at once."
            ));
        }
        if !self.driver.is_file() {
            return Err(format!(
                "Cua Driver is missing at {}",
                self.driver.display()
            ));
        }
        let recording_dir = self.new_recording_dir(&id)?;
        // After the check on MonoCode's text: the grant is the helper's own,
        // and its path names the app's data folder.
        let launched = with_recording_grant(manifest, &recording_dir)
            .and_then(|text| self.launch(&text, recording_dir.clone()));
        let mut runtime = match launched {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = fs::remove_dir(&recording_dir);
                return Err(error);
            }
        };
        if let Ok(result) = runtime
            .client
            .request("tools/list", json!({}), INIT_TIMEOUT)
        {
            runtime.read_only = read_only_tools(&result);
        }
        // C7: screenshots on, the accessibility tree off. A runtime that
        // cannot record still runs; the helper then logs its calls itself.
        let recording = runtime.start_recording();
        let pid = runtime.child.id();
        let slot = Arc::new(Slot {
            pid,
            reaped: AtomicBool::new(false),
            cancel: runtime.client.canceller(),
            runtime: Mutex::new(runtime),
        });
        let (old, over) = {
            let mut map = lock(&self.running);
            let old = map.insert(id.clone(), slot.clone());
            let over = map.len() > MAX_RUNTIMES;
            if over {
                map.remove(&id);
            }
            (old, over)
        };
        if over {
            self.end_slot(&id, &slot, mouse);
            return Err(format!(
                "At most {MAX_RUNTIMES} chats can use the computer at once."
            ));
        }
        // Another start of the same id won the race: end the one it made.
        if let Some(old) = old {
            self.end_slot(&id, &old, mouse);
        }
        Ok(Started { pid, recording })
    }

    /// A new folder per start, `<data>/recordings/<id>/<ms>-<n>`, so a
    /// restart never reads or overwrites the last one's turns (Cua numbers
    /// them from 1 on every start).
    fn new_recording_dir(&self, id: &str) -> Result<PathBuf, String> {
        private_dir(&self.data)?;
        let parent = self.data.join("recordings").join(id);
        private_dir(&parent)?;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let n = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        let dir = parent.join(format!("{ms:013}-{n}"));
        new_private_dir(&dir)?;
        // Cua grants and compares the canonical path
        // (`session_manifest.rs:1236-1238`).
        dir.canonicalize().map_err(|e| e.to_string())
    }

    /// Writes the manifest where only the helper looks, starts Cua with it,
    /// and checks that what Cua read is what was written.
    fn launch(&self, manifest: &str, recording_dir: PathBuf) -> Result<Runtime, String> {
        private_dir(&self.private)?;
        let folder = self.private.join(format!("m-{}", random_hex()?));
        new_private_dir(&folder)?;
        let path = folder.join("manifest.yaml");
        let result = self.launch_with(&path, manifest, recording_dir);
        let _ = fs::remove_dir_all(&folder);
        result
    }

    fn launch_with(
        &self,
        path: &Path,
        manifest: &str,
        recording_dir: PathBuf,
    ) -> Result<Runtime, String> {
        write_new_private(path, manifest.as_bytes())?;
        let mut child = spawn(&direct_launch(&self.driver, path, &self.home))
            .map_err(|e| format!("Cua Driver did not start: {e}"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cua Driver has no pipes.".into());
        };
        let mut runtime = Runtime {
            child,
            client: McpClient::new(stdout, stdin),
            scanner: Scanner::new(recording_dir.clone()),
            recording_dir,
            recording_on: false,
            read_only: HashSet::new(),
            queue: Vec::new(),
            calls: 0,
        };
        if let Err(error) = runtime.client.initialize(INIT_TIMEOUT) {
            runtime.finish(Duration::from_millis(200));
            return Err(format!("Cua Driver did not start: {error}"));
        }
        // Cua loads the manifest once, before it reads stdin
        // (`cua-driver/src/proxy.rs:41-47`), so by its answer it has read it.
        // A file that differs now may have differed then. This narrows a
        // same-user race; it does not close it.
        if read_nofollow(path, MAX_MANIFEST * 2).as_deref() != Some(manifest.as_bytes()) {
            runtime.finish(Duration::from_millis(200));
            return Err(
                "Cua Driver did not start: the manifest changed before Cua read it.".into(),
            );
        }
        Ok(runtime)
    }

    /// The runtime's tools, only those MonoCode may relay.
    pub fn tools(&self, id: &str) -> Result<Vec<Value>, String> {
        let slot = self.slot(id).ok_or("Not running")?;
        let mut runtime = lock(&slot.runtime);
        let result = runtime
            .client
            .request("tools/list", json!({}), INIT_TIMEOUT)?;
        runtime.read_only = read_only_tools(&result);
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .map(|tools| {
                tools
                    .iter()
                    .filter(|tool| {
                        tool.get("name")
                            .and_then(Value::as_str)
                            .is_some_and(relayable)
                    })
                    .cloned()
                    .map(without_output_paths)
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn call(&self, id: &str, name: &str, arguments: Value) -> Result<Value, String> {
        if !relayable(name) {
            return Err(format!("{name} is not relayed."));
        }
        if let Some(argument) = output_path_argument(name, &arguments) {
            return Err(format!(
                "{name} with {argument} is not relayed: MonoCode does not let an agent write files through Cua."
            ));
        }
        let slot = self.slot(id).ok_or("Not running")?;
        let mut runtime = lock(&slot.runtime);
        let reply = runtime.client.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            CALL_TIMEOUT,
        );
        // Cua writes a turn before it answers (`tool.rs:2448-2455`), so
        // reading the folder now keeps its records in order with the helper's.
        runtime.collect();
        if !runtime.recording_on || runtime.read_only.contains(name) {
            runtime.log_call(name, &arguments, &reply);
        }
        reply
    }

    pub fn stop(&self, id: &str, mouse: &dyn Mouse) -> StopReport {
        let slot = lock(&self.running).remove(id);
        match slot {
            Some(slot) => self.end_slot(id, &slot, mouse),
            // Nothing running, but a button may still be held from before.
            None => stop::stop(&mut NoProcesses, mouse),
        }
    }

    fn end_slot(&self, id: &str, slot: &Arc<Slot>, mouse: &dyn Mouse) -> StopReport {
        stop::stop(
            &mut Ending {
                id,
                slot,
                pending: &self.pending,
            },
            mouse,
        )
    }

    /// Ends every runtime: when MonoCode's connection closes, or MonoCode
    /// exits, before the helper quits.
    pub fn stop_all(&self, mouse: &dyn Mouse) {
        let ids: Vec<String> = lock(&self.running).keys().cloned().collect();
        for id in ids {
            self.stop(&id, mouse);
        }
    }

    /// New action records and runtimes that ended on their own. A runtime a
    /// call holds is read on a later poll.
    pub fn poll(&self) -> Polled {
        let mut records = std::mem::take(&mut *lock(&self.pending));
        let mut exited = Vec::new();
        let slots: Vec<(String, Arc<Slot>)> = lock(&self.running)
            .iter()
            .map(|(id, slot)| (id.clone(), slot.clone()))
            .collect();
        for (id, slot) in slots {
            let mut runtime = match slot.runtime.try_lock() {
                Ok(runtime) => runtime,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => continue,
            };
            runtime.collect();
            records.extend(runtime.queue.drain(..).map(|record| (id.clone(), record)));
            let Ok(Some(status)) = runtime.child.try_wait() else {
                continue;
            };
            slot.reaped.store(true, Ordering::SeqCst);
            {
                let mut map = lock(&self.running);
                // A stop that took it first reports it, not this.
                if !map.get(&id).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
                    continue;
                }
                map.remove(&id);
            }
            runtime.finish(Duration::from_millis(200));
            exited.push((id, status.code()));
        }
        (records, exited)
    }
}

impl Runtime {
    fn start_recording(&mut self) -> Value {
        let reply = self.client.request(
            "tools/call",
            json!({"name": "start_recording", "arguments": {
                "output_dir": self.recording_dir.to_string_lossy(),
                "include_accessibility_tree": false,
                "record_video": false
            }}),
            INIT_TIMEOUT,
        );
        match recording_refusal(&reply) {
            None => {
                self.recording_on = true;
                json!({"on": true})
            }
            Some(error) => {
                self.recording_on = false;
                json!({"on": false, "error": error})
            }
        }
    }

    /// Cua's new turn records, onto the queue.
    fn collect(&mut self) {
        let records = self.scanner.scan();
        self.queue.extend(records);
    }

    /// The helper's own record of a call Cua does not record.
    fn log_call(&mut self, name: &str, arguments: &Value, reply: &Result<Value, String>) {
        self.calls += 1;
        let turn = format!("call-{:06}", self.calls);
        let (error, summary, image) = match reply {
            Ok(result) => (
                result.get("isError") == Some(&Value::Bool(true)),
                first_text(result).map(str::to_string),
                first_png(result),
            ),
            Err(error) => (true, Some(error.clone()), None),
        };
        let screenshot = image.and_then(|data| self.save_png(&turn, data));
        self.queue.push(recording::helper_record(
            &turn,
            name,
            arguments,
            error,
            &iso8601_utc(now_ms()),
            summary.as_deref(),
            screenshot.as_deref(),
        ));
    }

    /// Writes a call's PNG to `<recording>/calls/<turn>.png`. None for
    /// anything that is not a PNG under the cap, or any write failure.
    fn save_png(&self, turn: &str, data: &str) -> Option<String> {
        if data.len() > MAX_SCREENSHOT / 3 * 4 + 4 {
            return None;
        }
        let bytes = base64::decode(data)?;
        if bytes.len() > MAX_SCREENSHOT || !bytes.starts_with(PNG_SIGNATURE) {
            return None;
        }
        let calls = self.recording_dir.join("calls");
        match fs::symlink_metadata(&calls) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return None,
            Err(_) => new_private_dir(&calls).ok()?,
        }
        let path = calls.join(format!("{turn}.png"));
        write_new_private(&path, &bytes).ok()?;
        Some(path.to_string_lossy().into_owned())
    }

    /// Closes Cua's stdin, which ends its sessions and the process
    /// (`cua-driver/src/proxy.rs:66-84`); waits `grace`, then signals.
    fn finish(&mut self, grace: Duration) -> bool {
        self.client.close();
        wait_exit(&mut self.child, grace) || end_child(&mut self.child, grace)
    }
}

/// Ends one runtime for Stop: `stop_recording`, a last read of its turns,
/// then the Cua session and the process. `stop::stop` releases the mouse
/// after (the order of findings 4.7 and 4.12).
struct Ending<'a> {
    id: &'a str,
    slot: &'a Arc<Slot>,
    pending: &'a Arc<Mutex<Vec<(String, Value)>>>,
}

impl Processes for Ending<'_> {
    fn end(&mut self, grace: Duration) -> bool {
        // A poll holds the lock only for a moment; a call may hold it long.
        let mut runtime = match lock_within(&self.slot.runtime, Duration::from_millis(50)) {
            Some(mut runtime) => {
                if runtime.recording_on {
                    let _ = runtime.client.request(
                        "tools/call",
                        json!({"name": "stop_recording", "arguments": {}}),
                        STOP_RECORDING_TIMEOUT,
                    );
                }
                runtime
            }
            None => {
                // A call holds the runtime. Cua answers one request at a
                // time, so a `stop_recording` would wait behind the call, and
                // Cua cannot cancel it (issue #3796). Cancel the call on our
                // side first: its answer needs EOF on Cua's stdout, which
                // never comes if anything Cua started holds that pipe
                // (security review X9). Then end the process, so the action
                // itself stops.
                self.slot.cancel.store(true, Ordering::SeqCst);
                self.signal(15);
                match lock_within(&self.slot.runtime, grace) {
                    Some(runtime) => runtime,
                    None => {
                        self.signal(9);
                        match lock_within(&self.slot.runtime, grace) {
                            Some(runtime) => runtime,
                            None => {
                                // Still held. The mouse release needs no
                                // runtime, so it goes ahead now; the runtime
                                // is finished when the call lets go.
                                let (id, slot, pending) =
                                    (self.id.to_string(), self.slot.clone(), self.pending.clone());
                                thread::spawn(move || {
                                    let mut runtime = lock(&slot.runtime);
                                    end_runtime(&id, &slot, &mut runtime, &pending, grace);
                                });
                                return false;
                            }
                        }
                    }
                }
            }
        };
        end_runtime(self.id, self.slot, &mut runtime, self.pending, grace)
    }
}

/// A last read of the runtime's turns, then the Cua session and process.
/// Called with `runtime`'s lock held, which is what makes reaping safe.
fn end_runtime(
    id: &str,
    slot: &Slot,
    runtime: &mut Runtime,
    pending: &Mutex<Vec<(String, Value)>>,
    grace: Duration,
) -> bool {
    runtime.collect();
    let records: Vec<(String, Value)> = runtime
        .queue
        .drain(..)
        .map(|record| (id.to_string(), record))
        .collect();
    lock(pending).extend(records);
    let ended = runtime.finish(grace);
    slot.reaped.store(true, Ordering::SeqCst);
    ended
}

impl Ending<'_> {
    /// Only while the lock is held elsewhere: then nothing reaps the child,
    /// so a pid not yet marked reaped is still this runtime's.
    fn signal(&self, number: i32) {
        if !self.slot.reaped.load(Ordering::SeqCst) {
            signal(self.slot.pid, number);
        }
    }
}

fn lock_within<T>(mutex: &Mutex<T>, wait: Duration) -> Option<MutexGuard<'_, T>> {
    let started = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) if started.elapsed() < wait => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(TryLockError::WouldBlock) => return None,
        }
    }
}

/// A tool as listed, minus the output-path arguments `call` refuses, so an
/// agent is not offered them.
fn without_output_paths(mut tool: Value) -> Value {
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for (owner, argument) in crate::tools::OUTPUT_PATH_ARGUMENTS {
        if *owner != name {
            continue;
        }
        if let Some(properties) = tool
            .pointer_mut("/inputSchema/properties")
            .and_then(Value::as_object_mut)
        {
            properties.remove(*argument);
        }
    }
    tool
}

/// The names of tools whose `annotations.readOnlyHint` is true.
fn read_only_tools(result: &Value) -> HashSet<String> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tool| tool.pointer("/annotations/readOnlyHint") == Some(&Value::Bool(true)))
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Why recording is off, if it is: a JSON-RPC error, `isError`, or a
/// structured `refused` (Cua's refusal envelope, `tool.rs:6110-6139`).
fn recording_refusal(reply: &Result<Value, String>) -> Option<String> {
    let text = match reply {
        Err(error) => error.clone(),
        Ok(result) => {
            let refused = result.get("isError") == Some(&Value::Bool(true))
                || result
                    .pointer("/structuredContent/status")
                    .and_then(Value::as_str)
                    == Some("refused");
            if !refused {
                return None;
            }
            first_text(result)
                .or_else(|| {
                    result
                        .pointer("/structuredContent/refusal/message")
                        .and_then(Value::as_str)
                })
                .unwrap_or("Cua refused to record.")
                .to_string()
        }
    };
    let text: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_RECORDING_ERROR)
        .collect();
    Some(if text.is_empty() {
        "Cua refused to record.".into()
    } else {
        text
    })
}

fn content(result: &Value) -> impl Iterator<Item = &Value> {
    result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn first_text(result: &Value) -> Option<&str> {
    content(result)
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .find_map(|item| item.get("text").and_then(Value::as_str))
}

fn first_png(result: &Value) -> Option<&str> {
    content(result)
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("image")
                && item.get("mimeType").and_then(Value::as_str) == Some("image/png")
        })
        .find_map(|item| item.get("data").and_then(Value::as_str))
}

/// MonoCode's manifest with a `files.write` grant for this start's recording
/// folder, which Cua needs before `start_recording` in bounded mode
/// (`session_manifest.rs:424-429`). MonoCode's text must end with its desktop
/// block, under `resources:` as the last key, and grant no files itself.
pub fn with_recording_grant(manifest: &str, dir: &Path) -> Result<String, String> {
    let top_keys: Vec<&str> = manifest
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with([' ', '#']))
        .collect();
    if !manifest.ends_with(MANIFEST_TAIL) || top_keys.last() != Some(&"resources:") {
        return Err("Bad manifest: it does not end with the desktop block.".into());
    }
    if manifest.lines().any(names_files) {
        return Err("Bad manifest: it grants files itself.".into());
    }
    let path = dir
        .to_str()
        .ok_or("The recording folder's path is not text.")?;
    // By the text: `Path::components` drops an inner `.` silently, and Cua
    // wants the normalized spelling (`session_manifest.rs:1199-1236`).
    let plain = dir.is_absolute()
        && path.starts_with('/')
        && !path[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        && !path.chars().any(|c| c.is_control() || c == '"');
    if !plain {
        return Err(format!("Cannot grant the recording folder {path:?}."));
    }
    let quoted = path.replace('\\', "\\\\");
    Ok(format!(
        "{manifest}  files:\n    write:\n      - dir: \"{quoted}\"\n        recursive: true\n"
    ))
}

/// A line whose key is `files`, however it is quoted or listed.
fn names_files(line: &str) -> bool {
    let key = line
        .trim_start_matches([' ', '\t', '-', '"', '\''])
        .to_ascii_lowercase();
    key.strip_prefix("files").is_some_and(|rest| {
        rest.trim_start_matches([' ', '\t', '"', '\''])
            .starts_with(':')
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the form Cua writes (`recording.rs:1529-1542`).
fn iso8601_utc(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let in_day = ms % 86_400_000;
    // Days to a civil date (H. Hinnant, "chrono-Compatible Low-Level Date
    // Algorithms", days_from_civil's inverse).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        in_day / 3_600_000,
        in_day / 60_000 % 60,
        in_day / 1000 % 60,
        in_day % 1000
    )
}

/// A bounded v3 manifest that never names MonoCode, never shows the whole
/// display, never asks for another mode, and grants no files. MonoCode writes
/// it; this only stops a mistake from reaching Cua.
pub fn manifest_acceptable(manifest: &str) -> bool {
    let lower = manifest.to_ascii_lowercase();
    // Exactly one `display:` key, and it says false; no other value form
    // (`yes`, `on`, a quoted true) gets through (security review, finding 13).
    let display: Vec<&str> = lower
        .lines()
        .filter(|line| line.trim_start().starts_with("display:"))
        .collect();
    manifest.len() <= MAX_MANIFEST
        && manifest.is_ascii()
        && lower.lines().next() == Some("# written by monocode for one chat. do not edit.")
        && lower.contains("\nversion: 3\n")
        && display.len() == 1
        && display[0].trim() == "display: false"
        && !lower.contains("com.monocode")
        && !lower.contains("unrestricted")
        && !lower.contains("mode:")
        && !lower.contains("bypass")
}

/// New action records, and runtimes that ended with their exit code.
pub type Polled = (Vec<(String, Value)>, Vec<(String, Option<i32>)>);

struct NoProcesses;
impl Processes for NoProcesses {
    fn end(&mut self, _grace: Duration) -> bool {
        true
    }
}

fn wait_exit(child: &mut Child, wait: Duration) -> bool {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if started.elapsed() < wait => thread::sleep(Duration::from_millis(10)),
            _ => return false,
        }
    }
}

fn end_child(child: &mut Child, grace: Duration) -> bool {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return true;
    }
    // SIGTERM first, so the driver can let go of what it holds.
    signal(child.id(), 15);
    if wait_exit(child, grace) {
        return true;
    }
    let _ = child.kill();
    child.wait().is_ok()
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

/// Sends `signal` to `pid`. Callers make sure the pid is not yet reaped.
fn signal(pid: u32, signal: i32) {
    #[cfg(unix)]
    if pid > 1 && pid <= i32::MAX as u32 {
        unsafe {
            kill(pid as i32, signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stop::Button;

    struct StillMouse;
    impl Mouse for StillMouse {
        fn held(&self, _button: Button) -> Option<bool> {
            Some(false)
        }
        fn release(&self, _button: Button) -> bool {
            true
        }
    }

    /// A small stand-in for `cua-driver mcp --direct --embedded`; the full
    /// one, with recording, is in `server.rs`'s tests.
    const FAKE_DRIVER: &str = r#"#!/bin/sh
[ "$*" = "mcp --direct --embedded" ] || exit 64
[ "$CUA_DRIVER_PERMISSION_MODE" = bounded ] || exit 3
[ "$CUA_DRIVER_RS_TELEMETRY_ENABLED" = false ] || exit 4
[ -f "$CUA_DRIVER_CAPABILITY_MANIFEST_FILE" ] || exit 5
[ "$CUA_DRIVER_CAPABILITY_MANIFEST_APPROVED" = 1 ] || exit 6
cp "$CUA_DRIVER_CAPABILITY_MANIFEST_FILE" "$(dirname "$0")/read-manifest.yaml"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"tools/list"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"click"},{"name":"get_desktop_state"},{"name":"start_recording"},{"name":"set_config"}]}}\n' "$id" ;;
    *) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}]}}\n' "$id" ;;
  esac
done
"#;

    fn setup(name: &str) -> (PathBuf, Runtimes) {
        let root = std::env::temp_dir().join(format!("cu-rt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let driver = root.join("cua-driver");
        fs::write(&driver, FAKE_DRIVER).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runtimes = Runtimes::new(driver, root.join("data"), root.join("s"), root.clone());
        (root, runtimes)
    }

    const MANIFEST: &str = "# Written by MonoCode for one chat. Do not edit.\nversion: 3\nexpires_after: \"8h\"\nresources:\n  desktop:\n    display: false\n";

    #[test]
    fn starts_lists_only_relayable_tools_calls_and_stops() {
        let (root, runtimes) = setup("main");
        let started = runtimes.start("rt1", MANIFEST, &StillMouse).unwrap();
        assert!(started.pid > 0);
        assert!(runtimes.is_running("rt1"));
        assert!(
            fs::read_dir(root.join("s")).unwrap().next().is_none(),
            "the manifest is gone once Cua is up"
        );
        let tools = runtimes.tools("rt1").unwrap();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["click"]);
        let result = runtimes.call("rt1", "click", json!({"x": 1})).unwrap();
        assert_eq!(result["content"][0]["text"], "ok");
        assert!(runtimes.call("rt1", "start_recording", json!({})).is_err());
        assert!(runtimes.call("rt1", "set_config", json!({})).is_err());
        let report = runtimes.stop("rt1", &StillMouse);
        assert!(report.ended);
        assert!(!runtimes.is_running("rt1"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cua_reads_monocodes_manifest_plus_the_recording_grant() {
        let (root, runtimes) = setup("grant");
        runtimes.start("rt", MANIFEST, &StillMouse).unwrap();
        let read = fs::read_to_string(root.join("read-manifest.yaml")).unwrap();
        let dir = fs::read_dir(root.join("data").join("recordings").join("rt"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .canonicalize()
            .unwrap();
        assert_eq!(
            read,
            format!(
                "{MANIFEST}  files:\n    write:\n      - dir: \"{}\"\n        recursive: true\n",
                dir.display()
            )
        );
        runtimes.stop_all(&StillMouse);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_recording_grant_goes_only_on_monocodes_shape() {
        let dir = Path::new("/Users/t/Library/Application Support/com.monocode.desktop/computer-use/recordings/rt/1-1");
        let granted = with_recording_grant(MANIFEST, dir).unwrap();
        assert!(granted.starts_with(MANIFEST));
        assert!(granted.ends_with(&format!(
            "  files:\n    write:\n      - dir: \"{}\"\n        recursive: true\n",
            dir.display()
        )));
        let head = "# Written by MonoCode for one chat. Do not edit.\nversion: 3\n";
        for bad in [
            format!("{head}resources:\n  desktop:\n    display: false\n  apps: []\n"),
            format!("{head}resources:\n  desktop:\n    display: false"),
            format!("{head}resources:\n  files:\n    write: [\"/tmp\"]\n  desktop:\n    display: false\n"),
            format!("{head}resources:\n  \"files\" :\n    read: []\n  desktop:\n    display: false\n"),
            format!("{head}resources:\n  desktop:\n    display: false\nallow: {{}}\n  desktop:\n    display: false\n"),
            format!("{head}  desktop:\n    display: false\n"),
        ] {
            assert!(with_recording_grant(&bad, dir).is_err(), "{bad}");
        }
        for bad_dir in [
            "relative/rec",
            "/a/../b",
            "/a/./b",
            "/a/b\"c",
            "/a/b\nc",
            "/a/b\u{7}",
            "/a/b/",
        ] {
            assert!(
                with_recording_grant(MANIFEST, Path::new(bad_dir)).is_err(),
                "{bad_dir:?}"
            );
        }
    }

    #[test]
    fn refuses_bad_ids_and_manifests() {
        let (root, runtimes) = setup("bad");
        assert!(runtimes.start("../x", MANIFEST, &StillMouse).is_err());
        assert!(runtimes
            .start("rt", "mode: unrestricted", &StillMouse)
            .is_err());
        assert!(runtimes
            .start("rt", &"x".repeat(MAX_MANIFEST + 1), &StillMouse)
            .is_err());
        let head = "# Written by MonoCode for one chat. Do not edit.\nversion: 3\n";
        for bad in [
            format!(
                "{head}  - bundle_id: \"com.monocode.desktop\"\n  desktop:\n    display: false\n"
            ),
            format!("{head}  desktop:\n    display: true\n"),
            format!("{head}  desktop:\n    display: yes\n"),
            format!("{head}  desktop:\n    display: false\n    display: true\n"),
            format!("{head}mode: unrestricted\n  desktop:\n    display: false\n"),
            "version: 3\n  desktop:\n    display: false\n".to_string(),
            // MonoCode's shape, but granting files itself.
            format!("{head}resources:\n  files:\n    write:\n      - \"/\"\n  desktop:\n    display: false\n"),
        ] {
            assert!(runtimes.start("rt", &bad, &StillMouse).is_err(), "{bad}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_driver_is_a_plain_error() {
        let (root, runtimes) = setup("missing");
        fs::remove_file(root.join("cua-driver")).unwrap();
        let error = runtimes.start("rt", MANIFEST, &StillMouse).unwrap_err();
        assert!(error.contains("missing"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn restarting_replaces_the_runtime() {
        let (root, runtimes) = setup("restart");
        let first = runtimes.start("rt", MANIFEST, &StillMouse).unwrap();
        let second = runtimes.start("rt", MANIFEST, &StillMouse).unwrap();
        assert_ne!(first.pid, second.pid);
        runtimes.stop_all(&StillMouse);
        assert!(!runtimes.is_running("rt"));
        let _ = fs::remove_dir_all(&root);
    }

    /// Security review X10: `click.debug_image_out` and
    /// `get_window_state.screenshot_out_file` write a file where the caller
    /// says, and the recording grant would let them write into the recording
    /// folder and forge Cua's turns. The helper refuses either argument.
    #[test]
    fn a_call_naming_an_output_path_is_refused_and_never_relayed() {
        let (root, runtimes) = setup("outpath");
        runtimes.start("rt", MANIFEST, &StillMouse).unwrap();
        for (tool, argument) in [
            ("click", "debug_image_out"),
            ("get_window_state", "screenshot_out_file"),
        ] {
            let mut arguments = json!({"pid": 1, "window_id": 2, "x": 1, "y": 1});
            arguments[argument] = json!("/tmp/rec/turn-00001/action.json");
            let error = runtimes.call("rt", tool, arguments).unwrap_err();
            assert!(error.contains(argument), "{tool}: {error}");
        }
        assert!(runtimes
            .call("rt", "click", json!({"pid": 1, "x": 1, "y": 1}))
            .is_ok());
        let listed = without_output_paths(json!({"name": "click", "inputSchema": {
            "properties": {"x": {}, "debug_image_out": {}}}}));
        assert_eq!(listed["inputSchema"]["properties"], json!({"x": {}}));
        runtimes.stop_all(&StillMouse);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn timestamps_are_iso_8601_utc_with_milliseconds() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_utc(1_760_644_800_123), "2025-10-16T20:00:00.123Z");
        assert_eq!(iso8601_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso8601_utc(4_107_542_399_999), "2100-02-28T23:59:59.999Z");
    }
}

//! End to end on the fixtures: replay eslogger JSON through the tracker and
//! filter, log it with the hash chain, then check the chain catches an
//! edited line and a cut last line.
//!
//! The fixture is written from Apple's documented eslogger format; it is
//! not verified on macOS Tahoe. The Mac test re-runs `parse-fixture` on a
//! real capture.

use std::fs;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;

use monocode_activity_collector::event::ActivityEvent;
use monocode_activity_collector::log::{self, LogWriter};
use monocode_activity_collector::replay::{self, Setup};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/harness_session.jsonl"
);
const HARNESS: u32 = 1000;
const SHELL: u32 = 1001;
const GIT: u32 = 1002;
const OTHER: u32 = 2000;
const CALC: u32 = 3000;

fn temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "mc-collector-e2e-{name}-{}-{nanos}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn setup() -> Setup {
    Setup {
        roots: vec![(HARNESS, "chat-1".to_owned())],
        grants: vec![(
            "chat-2".to_owned(),
            "com.apple.calculator".to_owned(),
            "/System/Applications/Calculator.app".to_owned(),
        )],
    }
}

fn replayed() -> Vec<ActivityEvent> {
    let file = fs::File::open(FIXTURE).expect("fixture");
    replay::replay(BufReader::new(file), &setup()).expect("replay")
}

fn kinds_for(events: &[ActivityEvent], pid: u32) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.pid == Some(pid))
        .map(|e| e.kind.clone())
        .collect()
}

fn write_log(dir: &Path, events: &[ActivityEvent]) -> PathBuf {
    let mut w = LogWriter::open(dir).expect("open log");
    for ev in events {
        w.append(ev.clone()).expect("append");
    }
    let day = w.current_day().expect("a day");
    dir.join(log::log_name(day))
}

#[test]
fn harness_tree_is_followed_and_others_dropped() {
    let events = replayed();
    assert_eq!(kinds_for(&events, HARNESS), vec!["fork"]);
    assert_eq!(
        kinds_for(&events, SHELL),
        vec!["exec", "fork", "unlink", "open", "exit"]
    );
    assert_eq!(
        kinds_for(&events, GIT),
        vec!["exec", "create", "write", "rename", "exit"]
    );
    // The unrelated process, its unlink and its credentials read: dropped.
    assert!(kinds_for(&events, OTHER).is_empty());
    // After the shell exited its pid is no longer followed.
    assert!(!events.iter().any(|e| e.detail["path"] == "/tmp/after-exit"));

    let git_exec = events
        .iter()
        .find(|e| e.pid == Some(GIT) && e.kind == "exec")
        .unwrap();
    assert_eq!(
        git_exec.detail["argv"],
        serde_json::json!(["git", "commit", "-am", "wip"])
    );
    assert_eq!(git_exec.detail["ppid"], SHELL);
    assert_eq!(git_exec.chat_id, "chat-1");
    let unlink = events.iter().find(|e| e.kind == "unlink").unwrap();
    assert_eq!(unlink.detail["path"], "/Users/dev/Documents/notes.txt");
    let open = events.iter().find(|e| e.kind == "open").unwrap();
    assert_eq!(open.detail["path"], "/Users/dev/.ssh/id_ed25519");
    let rename = events.iter().find(|e| e.kind == "rename").unwrap();
    assert_eq!(rename.detail["dest"], "/Users/dev/proj/.git/index");
    for e in &events {
        assert_eq!(e.harness, "");
        assert!(e.summary.chars().count() <= 300 && !e.summary.contains('\n'));
    }
}

#[test]
fn grant_attributes_calculator() {
    let events = replayed();
    let calc: Vec<&ActivityEvent> = events.iter().filter(|e| e.pid == Some(CALC)).collect();
    assert_eq!(
        calc.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec!["exec", "create", "tcc", "exit"]
    );
    for e in &calc {
        assert_eq!(e.chat_id, "chat-2");
        assert_eq!(e.detail["grant"], "com.apple.calculator");
    }
    assert_eq!(calc[2].detail["service"], "kTCCServiceAccessibility");
    // Without the grant, Calculator is dropped.
    let file = fs::File::open(FIXTURE).unwrap();
    let only_root = Setup {
        roots: setup().roots,
        grants: Vec::new(),
    };
    let events = replay::replay(BufReader::new(file), &only_root).unwrap();
    assert!(kinds_for(&events, CALC).is_empty());
}

#[test]
fn log_verifies_then_catches_edit_and_cut() {
    let events = replayed();
    let dir = temp_dir("chain");
    let path = write_log(&dir, &events);
    let report = log::verify(log::open_log(&path, false).unwrap()).unwrap();
    assert!(report.ok);
    assert_eq!(report.lines, events.len());
    assert!(!report.cut);

    let text = fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // Edit line 9 (the unlink outside the folder) to hide the path.
    let target = lines
        .iter()
        .position(|l| l.contains("/Users/dev/Documents/notes.txt"))
        .unwrap();
    lines[target] = lines[target].replace("notes.txt", "other.txt");
    let edited = lines.join("\n") + "\n";
    let report = log::verify(Cursor::new(edited.clone())).unwrap();
    assert!(!report.ok);
    assert_eq!(report.first_bad_line, Some(target + 1));

    // Cut the last line short.
    let cut = &text[..text.len() - 20];
    let report = log::verify(Cursor::new(cut.to_owned())).unwrap();
    assert!(!report.ok);
    assert!(report.cut);
    assert_eq!(report.first_bad_line, Some(events.len()));

    // The `verify` mode reports the same.
    let bin = env!("CARGO_BIN_EXE_monocode-activity-collector");
    let good = Command::new(bin).arg("verify").arg(&path).output().unwrap();
    assert!(good.status.success());
    let v: serde_json::Value = serde_json::from_slice(&good.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["lines"], events.len());
    fs::write(&path, edited).unwrap();
    let bad = Command::new(bin).arg("verify").arg(&path).output().unwrap();
    assert_eq!(bad.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&bad.stdout).unwrap();
    assert_eq!(v["firstBadLine"], target + 1);
    fs::write(&path, cut).unwrap();
    let v: serde_json::Value = serde_json::from_slice(
        &Command::new(bin)
            .arg("verify")
            .arg(&path)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(v["cut"], true);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn parse_fixture_mode_matches_the_library() {
    let bin = env!("CARGO_BIN_EXE_monocode-activity-collector");
    let out = Command::new(bin)
        .args([
            "parse-fixture",
            FIXTURE,
            "--pid",
            "1000",
            "--chat",
            "chat-1",
            "--grant",
            "chat-2",
            "com.apple.calculator",
            "/System/Applications/Calculator.app",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let printed: Vec<ActivityEvent> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(printed, replayed());

    let run = Command::new(bin).arg("run").output().unwrap();
    if cfg!(not(target_os = "macos")) {
        assert_eq!(run.status.code(), Some(2));
    }
    let bad = Command::new(bin).arg("frobnicate").output().unwrap();
    assert!(!bad.status.success());
}

#[test]
fn logged_events_read_back_through_the_read_path() {
    let events = replayed();
    let dir = temp_dir("read");
    write_log(&dir, &events);
    let res = log::read_events(&dir, 0, i64::MAX / 2, "chat-2", 100).unwrap();
    assert_eq!(res.events.len(), 4);
    assert!(res.chain_ok && !res.truncated);
    assert!(res.events[0].id.starts_with("es-2025-10-09-"));
    fs::remove_dir_all(&dir).unwrap();
}

const FILE_EVENTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/file_events.jsonl"
);

fn replayed_file_events() -> Vec<ActivityEvent> {
    let file = fs::File::open(FILE_EVENTS).expect("fixture");
    let setup = Setup {
        roots: vec![(5000, "chat-9".to_owned())],
        grants: Vec::new(),
    };
    replay::replay(BufReader::new(file), &setup).expect("replay")
}

#[test]
fn a_fork_names_its_child_and_events_carry_tier_and_lineage() {
    // Finding 3.8: the fork is logged under the parent with the new
    // process in `detail.child`, and every event says root or child.
    let events = replayed_file_events();
    let fork = events.iter().find(|e| e.kind == "fork").expect("fork kept");
    assert_eq!(fork.pid, Some(5000));
    assert_eq!(fork.detail["child"], 5001);
    assert_eq!(fork.detail["tier"], "root");
    let exec = events.iter().find(|e| e.kind == "exec").expect("exec kept");
    assert_eq!(exec.detail["tier"], "child");
    assert_eq!(exec.detail["ppid"], 1);
    assert_eq!(exec.detail["originalPpid"], 5000);
    assert_eq!(exec.detail["groupId"], 5000);
    assert_eq!(exec.detail["sessionId"], 100001);
}

#[test]
fn truncate_link_copyfile_clone_and_exchangedata_are_logged_with_paths() {
    // Finding 1.3: the events eslogger offers on macOS 26 that the fixed
    // list left out.
    let events = replayed_file_events();
    let by = |kind: &str| {
        events
            .iter()
            .find(|e| e.kind == kind)
            .unwrap_or_else(|| panic!("{kind} not logged"))
    };
    assert_eq!(by("truncate").detail["path"], "/Users/dev/proj/out.log");
    let link = by("link");
    assert_eq!(link.detail["path"], "/Users/dev/proj/a.txt");
    assert_eq!(link.detail["dest"], "/Users/dev/Desktop/a-link.txt");
    assert_eq!(
        by("copyfile").detail["dest"],
        "/Users/dev/Desktop/a-copy.txt"
    );
    assert_eq!(by("clone").detail["dest"], "/Users/dev/Desktop/b-clone.txt");
    let swap = by("exchangedata");
    assert_eq!(swap.detail["path"], "/Users/dev/proj/c.txt");
    assert_eq!(swap.detail["dest"], "/Users/dev/Documents/c.txt");
    assert_eq!(
        swap.summary,
        "exchangedata /Users/dev/proj/c.txt <-> /Users/dev/Documents/c.txt"
    );
}

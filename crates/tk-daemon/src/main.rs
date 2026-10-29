//! `tk-daemon --foreground [--socket PATH] [--data-dir PATH]`: binds the
//! socket and serves until killed. A service manager runs it (the systemd
//! and launchd units in `units/`). `--socket` and `--data-dir` take
//! absolute paths and exist for tests; clients find the default socket by
//! the contract's rule. `TK_LOG_CONTENT=1` keeps prompt and reply text in
//! telemetry; off by default.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: tk-daemon --foreground [--socket PATH] [--data-dir PATH]\n       tk-daemon cu-mcp\n       tk-daemon collector-plan --binary ABSOLUTE_PATH";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The computer-use MCP server a harness starts (lane K's relay; request
    // 1 to integration w2). It reads the chat token `tk run` gave the
    // harness and the bridge address beside the daemon's socket; with
    // neither it answers as an empty server and never exits early (case 39).
    #[cfg(feature = "computer-use")]
    if args.first().map(String::as_str) == Some("cu-mcp") {
        return ExitCode::from(u8::try_from(tk_daemon::computer_use::relay::run()).unwrap_or(1));
    }
    if args.first().map(String::as_str) == Some("collector-plan") {
        return collector_plan(&args[1..]);
    }
    let Some(mut paths) = tk_daemon::service::default_paths() else {
        eprintln!("tk-daemon: no HOME or XDG_RUNTIME_DIR");
        return ExitCode::from(2);
    };
    let mut foreground = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--foreground" => foreground = true,
            "--socket" | "--data-dir" => {
                let Some(value) = args.get(i + 1).map(PathBuf::from) else {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                };
                if !value.is_absolute() {
                    eprintln!("tk-daemon: {} needs an absolute path", args[i]);
                    return ExitCode::from(2);
                }
                if args[i] == "--socket" {
                    paths.socket = value;
                } else {
                    paths.data = value;
                }
                i += 1;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    if !foreground {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let log_prompts = std::env::var("TK_LOG_CONTENT").is_ok_and(|v| v == "1");
    let running = match tk_daemon::service::start(paths, log_prompts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tk-daemon: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "tk-daemon: listening on {}; log in {}",
        running.paths.socket.display(),
        running.paths.log_dir().display()
    );
    let listener = match running.listener.try_clone() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tk-daemon: socket: {e}");
            return ExitCode::from(1);
        }
    };
    // Lane K's seam (wave 2): the Mac collector's registration and the
    // computer-use bridge. Does nothing until lane K fills it.
    tk_daemon::collector::start(&running.daemon, &running.paths);
    // Serves until the listener fails; `running` (and the pid file's lock)
    // lives until then.
    tk_daemon::socket::serve(listener, running.daemon.clone());
    drop(running);
    ExitCode::from(1)
}

/// `tk-daemon collector-plan --binary <abs path>`: prints, as JSON, what
/// installing the macOS collector would do (the plist, the root commands
/// with every tool by absolute path, the collector's SHA-256 and this
/// daemon as its peer), staging the plist 0600 under the data folder. It
/// runs nothing as root: the commands are for you to read and run (rules
/// section 5, item 2; lane K's request 1). macOS only.
fn collector_plan(args: &[String]) -> ExitCode {
    if !cfg!(target_os = "macos") {
        eprintln!("tk-daemon collector-plan: the collector is macOS only");
        return ExitCode::from(2);
    }
    let binary = match args {
        [flag, path] if flag == "--binary" => PathBuf::from(path),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(paths) = tk_daemon::service::default_paths() else {
        eprintln!("tk-daemon: no HOME or XDG_RUNTIME_DIR");
        return ExitCode::from(2);
    };
    let peer = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("tk-daemon collector-plan: cannot find this binary: {e}");
            return ExitCode::from(1);
        }
    };
    match tk_daemon::collector::install_plan(&paths, &binary, &peer) {
        Ok(plan) => match serde_json::to_string_pretty(&plan) {
            Ok(text) => {
                println!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("tk-daemon collector-plan: {e}");
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("tk-daemon collector-plan: {e}");
            ExitCode::from(1)
        }
    }
}

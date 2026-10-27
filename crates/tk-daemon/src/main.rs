//! `tk-daemon --foreground [--socket PATH] [--data-dir PATH]`: binds the
//! socket and serves until killed. A service manager runs it (the systemd
//! and launchd units in `units/`). `--socket` and `--data-dir` take
//! absolute paths and exist for tests; clients find the default socket by
//! the contract's rule. `TK_LOG_CONTENT=1` keeps prompt and reply text in
//! telemetry; off by default.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: tk-daemon --foreground [--socket PATH] [--data-dir PATH]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    // Serves until the listener fails; `running` (and the pid file's lock)
    // lives until then.
    tk_daemon::socket::serve(listener, running.daemon.clone());
    drop(running);
    ExitCode::from(1)
}

//! `tk-daemon`: session 0 stub. `--foreground` binds the socket and serves
//! connections until killed; anything else prints usage and exits 2. Lane B
//! adds the receiver, the log, the pid file and the retention job.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--foreground" => {
            let Some(path) = tk_daemon::socket_path() else {
                eprintln!("tk-daemon: no HOME or XDG_RUNTIME_DIR");
                return ExitCode::from(2);
            };
            let listener = match tk_daemon::socket::bind(&path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("tk-daemon: cannot bind {}: {e}", path.display());
                    return ExitCode::from(1);
                }
            };
            eprintln!("tk-daemon: listening on {}", path.display());
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        std::thread::spawn(move || {
                            let _ = tk_daemon::socket::serve_one(s);
                        });
                    }
                    Err(e) => eprintln!("tk-daemon: accept: {e}"),
                }
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: tk-daemon --foreground");
            ExitCode::from(2)
        }
    }
}

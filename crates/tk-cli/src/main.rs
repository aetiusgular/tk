//! `tk`: the command line. `main.rs` only dispatches; each subcommand lives
//! in its own folder and is owned by one lane (plans/parallel_build_tk.md,
//! section 2, point 5): `connect/` lane C, `run/` lane D, `watch/` lane J in
//! wave 2. No lane edits this file during a wave; a new subcommand is a
//! request for integration.

mod connect;
mod run;
mod watch;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = dispatch(&args);
    ExitCode::from(code)
}

fn dispatch(args: &[String]) -> u8 {
    match args.first().map(String::as_str) {
        Some("connect") => connect::main(&args[1..]),
        Some("run") => run::main(&args[1..]),
        Some("watch") | Some("status") => watch::main(args),
        // Computer use's controls (toolkit plan T1.5; integration w2).
        Some("cu") => watch::cu::main(&args[1..]),
        Some("--version") => {
            println!("tk {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!("usage: tk <connect|run|watch|status|cu> ...");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn unknown_or_empty_command_is_usage_error() {
        assert_eq!(dispatch(&s(&[])), 2);
        assert_eq!(dispatch(&s(&["frobnicate"])), 2);
    }

    #[test]
    fn version_exits_zero() {
        assert_eq!(dispatch(&s(&["--version"])), 0);
    }

    #[test]
    fn each_subcommand_reaches_its_module() {
        // With no arguments each answers its usage error (2); the point is
        // that none panics. `status` is left out: it is a real report since
        // wave 2 (exit 0) and reads the machine's daemon socket and agent
        // config files, which a unit test must not (lane J's request 2,
        // integration w2). `watch::status` has its own tests on fixtures.
        for cmd in ["connect", "run", "watch", "cu"] {
            assert_eq!(dispatch(&s(&[cmd])), 2, "{cmd}");
        }
    }
}

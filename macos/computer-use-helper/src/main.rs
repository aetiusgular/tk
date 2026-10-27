//! MonoCode Computer Use: the helper app that holds Accessibility and Screen
//! Recording and embeds Cua Driver (wave 3, lane C, item C1).
//!
//! - MonoCode starts it through Launch Services (`open -n -g -a`), so macOS
//!   credits the grants to this app, not to MonoCode or a harness. No agent
//!   shell inherits them (note W, section 4).
//! - It listens on one Unix socket MonoCode names, takes one caller, and
//!   checks that caller's code signature against MonoCode's (`caller.rs`).
//! - Per chat it starts one `cua-driver mcp --direct --embedded` itself,
//!   bounded by the chat's manifest, talked to over its stdio with no socket,
//!   with Cua's telemetry and update check off.
//! - When MonoCode's connection closes, or MonoCode exits, every runtime ends
//!   and the helper quits.
//!
//! Usage: `monocode-computer-use-helper serve --socket <path> --monocode-pid
//! <pid> --data <dir>`. A debug build also takes `--driver <path>`; a release
//! build uses the `cua-driver` beside its own executable only.

mod apps;
mod base64;
mod caller;
mod driver;
mod lines;
#[cfg(target_os = "macos")]
mod macos;
mod mcp_client;
mod recording;
mod runtime;
mod server;
mod stop;
mod tools;

use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
struct Args {
    socket: PathBuf,
    monocode_pid: i32,
    data: PathBuf,
    driver: Option<PathBuf>,
}

fn parse_args(args: &[String], debug: bool) -> Result<Args, String> {
    if args.first().map(String::as_str) != Some("serve") {
        return Err("usage: serve --socket <path> --monocode-pid <pid> --data <dir>".into());
    }
    let mut socket = None;
    let mut pid = None;
    let mut data = None;
    let mut driver = None;
    let mut rest = args[1..].iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--socket" => socket = Some(PathBuf::from(value)),
            "--monocode-pid" => pid = Some(value.parse::<i32>().map_err(|_| "bad pid")?),
            "--data" => data = Some(PathBuf::from(value)),
            "--driver" if debug => driver = Some(PathBuf::from(value)),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let socket = socket.ok_or("--socket is required")?;
    let data = data.ok_or("--data is required")?;
    let monocode_pid = pid
        .filter(|pid| *pid > 1)
        .ok_or("--monocode-pid is required")?;
    if !socket.is_absolute() || !data.is_absolute() {
        return Err("paths must be absolute".into());
    }
    if socket.as_os_str().len() > 100 {
        return Err("socket path too long".into());
    }
    Ok(Args {
        socket,
        monocode_pid,
        data,
        driver,
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv, cfg!(debug_assertions)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("monocode-computer-use-helper: {error}");
            std::process::exit(2);
        }
    };
    std::process::exit(run(args));
}

#[cfg(not(target_os = "macos"))]
fn run(_args: Args) -> i32 {
    eprintln!("monocode-computer-use-helper: macOS only");
    2
}

#[cfg(target_os = "macos")]
fn run(args: Args) -> i32 {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let driver = args.driver.clone().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("cua-driver")))
            .unwrap_or_else(|| PathBuf::from("/nonexistent/cua-driver"))
    });
    let Some(socket_dir) = args.socket.parent().map(PathBuf::from) else {
        return 2;
    };
    if runtime::private_dir(&socket_dir).is_err() {
        return 2;
    }
    // Each start's manifest goes in a fresh folder inside this private one,
    // which MonoCode made 0700 with a random name.
    let runtimes = runtime::Runtimes::new(
        driver.clone(),
        args.data.clone(),
        socket_dir.clone(),
        home.clone(),
    );
    let shared = Arc::new(server::Shared::new(
        runtimes,
        Box::new(macos::SystemMouse),
        home,
        driver.is_file(),
    ));

    let _ = std::fs::remove_file(&args.socket);
    let listener = match UnixListener::bind(&args.socket) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("monocode-computer-use-helper: cannot listen: {error}");
            return 2;
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&args.socket, std::fs::Permissions::from_mode(0o600));
    }

    // Quit when MonoCode is gone, ending every runtime first.
    {
        let shared = shared.clone();
        let pid = args.monocode_pid;
        let socket = args.socket.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            if !process_alive(pid) {
                shared.runtimes.stop_all(shared.mouse.as_ref());
                let _ = std::fs::remove_file(&socket);
                std::process::exit(0);
            }
        });
    }

    let requirement = caller::built_requirement();
    let started = Instant::now();
    listener.set_nonblocking(true).ok();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).ok();
                let fd = stream.as_raw_fd();
                let signed = requirement.and_then(|req| macos::peer_meets(fd, req));
                let verdict = caller::decide(
                    signed,
                    macos::peer_pid(fd),
                    args.monocode_pid,
                    cfg!(debug_assertions),
                );
                match verdict {
                    caller::Verdict::Allow => {
                        if requirement.is_none() {
                            eprintln!("monocode-computer-use-helper: debug build, caller checked by pid only");
                        }
                        server::serve(shared.clone(), stream);
                        // One MonoCode connection per helper run.
                        let _ = std::fs::remove_file(&args.socket);
                        return 0;
                    }
                    caller::Verdict::Refuse(why) => {
                        eprintln!("monocode-computer-use-helper: refused a caller: {why}");
                        drop(stream);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() > Duration::from_secs(60) {
                    let _ = std::fs::remove_file(&args.socket);
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return 2,
        }
    }
}

#[cfg(target_os = "macos")]
fn process_alive(pid: i32) -> bool {
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    unsafe { kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// `bundle.sh` with `DRY_RUN=1`: its output, and whether it succeeded.
    fn dry_run(env: &[(&str, &str)]) -> (bool, String) {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundle.sh");
        let mut command = std::process::Command::new("bash");
        command
            .arg(&script)
            .arg("/out")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("DRY_RUN", "1")
            .env("CUA_DRIVER_BIN", "/verified/cua-driver");
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cu-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_debug_bundle_signs_ad_hoc_with_its_own_ids_and_no_keychain() {
        let (ok, out) = dry_run(&[("HELPER_DEBUG", "1")]);
        assert!(ok, "{out}");
        assert!(!out.contains("security"), "no keychain lookup: {out}");
        assert!(out.contains("--sign - "), "{out}");
        assert!(!out.contains("MONOCODE_HELPER_CERT_SHA1"), "{out}");
        assert!(
            out.contains("--identifier com.monocode.computer-use-helper.dev "),
            "{out}"
        );
        assert!(
            out.contains("Set\\ :CFBundleIdentifier\\ com.monocode.computer-use-helper.dev"),
            "{out}"
        );
        assert!(
            out.contains("MONOCODE_APP_ID=com.monocode.desktop.dev"),
            "{out}"
        );
        let (ok, out) = dry_run(&[
            ("HELPER_DEBUG", "1"),
            ("MONOCODE_APP_ID", "com.example.mine"),
        ]);
        assert!(
            ok && out.contains("MONOCODE_APP_ID=com.example.mine"),
            "{out}"
        );
        let (ok, out) = dry_run(&[("HELPER_DEBUG", "1"), ("MONOCODE_APP_ID", "x\" or 1")]);
        assert!(!ok, "{out}");
    }

    #[test]
    fn a_release_bundle_takes_exactly_one_certificate_by_its_exact_name() {
        let dir = scratch("ids");
        let listing = dir.join("identities.txt");
        let a = "1111111111111111111111111111111111111111";
        let b = "2222222222222222222222222222222222222222";
        let c = "3333333333333333333333333333333333333333";
        std::fs::write(
            &listing,
            format!(
                "  1) {a} \"MonoCode Dev\"\n  2) {b} \"MonoCode Dev 2\"\n  3) {c} \"Evil MonoCode Dev\"\n     3 valid identities found\n"
            ),
        )
        .unwrap();
        let listing = listing.to_str().unwrap();
        let (ok, out) = dry_run(&[
            ("MONOCODE_HELPER_IDENTITY", "MonoCode Dev"),
            ("DRY_RUN_IDENTITIES", listing),
        ]);
        assert!(ok, "{out}");
        assert!(
            out.contains("security find-identity -v -p codesigning"),
            "{out}"
        );
        assert!(!out.contains("find-certificate"), "{out}");
        assert!(
            out.contains(&format!("MONOCODE_HELPER_CERT_SHA1={a}")),
            "{out}"
        );
        assert!(
            out.contains(&format!("--sign {a} ")),
            "signs by the SHA-1: {out}"
        );
        assert!(
            out.contains("MONOCODE_APP_ID=com.monocode.desktop "),
            "{out}"
        );
        assert!(
            out.contains("--identifier com.monocode.computer-use-helper "),
            "{out}"
        );
        assert!(!out.contains(".dev"), "{out}");
        // A name only part of others matches nothing.
        let (ok, out) = dry_run(&[
            ("MONOCODE_HELPER_IDENTITY", "MonoCode"),
            ("DRY_RUN_IDENTITIES", listing),
        ]);
        assert!(!ok && out.contains("No signing identity named"), "{out}");
        // Two certificates with the same name: refused.
        let twice = dir.join("twice.txt");
        std::fs::write(
            &twice,
            format!("  1) {a} \"MonoCode Dev\"\n  2) {b} \"MonoCode Dev\"\n     2 valid identities found\n"),
        )
        .unwrap();
        let (ok, out) = dry_run(&[
            ("MONOCODE_HELPER_IDENTITY", "MonoCode Dev"),
            ("DRY_RUN_IDENTITIES", twice.to_str().unwrap()),
        ]);
        assert!(!ok && out.contains("2 signing identities named"), "{out}");
        // A release build needs the identity.
        let (ok, out) = dry_run(&[]);
        assert!(!ok && out.contains("MONOCODE_HELPER_IDENTITY"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_the_serve_command() {
        let parsed = parse_args(
            &args(&[
                "serve",
                "--socket",
                "/t/s",
                "--monocode-pid",
                "42",
                "--data",
                "/d",
            ]),
            false,
        )
        .unwrap();
        assert_eq!(parsed.monocode_pid, 42);
        assert_eq!(parsed.driver, None);
    }

    #[test]
    fn a_release_build_takes_no_driver_path() {
        let list = args(&[
            "serve",
            "--socket",
            "/t/s",
            "--monocode-pid",
            "42",
            "--data",
            "/d",
            "--driver",
            "/tmp/x",
        ]);
        assert!(parse_args(&list, false).is_err());
        assert_eq!(
            parse_args(&list, true).unwrap().driver,
            Some(PathBuf::from("/tmp/x"))
        );
    }

    #[test]
    fn refuses_bad_arguments() {
        for list in [
            vec![],
            vec!["mcp"],
            vec![
                "serve",
                "--socket",
                "s",
                "--monocode-pid",
                "42",
                "--data",
                "/d",
            ],
            vec![
                "serve",
                "--socket",
                "/t/s",
                "--monocode-pid",
                "1",
                "--data",
                "/d",
            ],
            vec![
                "serve",
                "--socket",
                "/t/s",
                "--monocode-pid",
                "x",
                "--data",
                "/d",
            ],
            vec!["serve", "--socket", "/t/s", "--data", "/d"],
            vec!["serve", "--socket"],
        ] {
            assert!(parse_args(&args(&list), true).is_err(), "{list:?}");
        }
        let long = format!("/{}", "x".repeat(120));
        assert!(parse_args(
            &args(&[
                "serve",
                "--socket",
                &long,
                "--monocode-pid",
                "9",
                "--data",
                "/d"
            ]),
            true
        )
        .is_err());
    }
}

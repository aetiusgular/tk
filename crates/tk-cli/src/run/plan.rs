//! The command `tk run` starts, built as an argv list and an environment,
//! with no I/O: everything here is a pure function of its inputs, so every
//! boundary and harness is tested without starting anything.
//!
//! - `none`: the harness itself, with the chat's variables.
//! - `sbx`: `sbx run --name tk-<chat> -e NAME... [--kit KIT] AGENT CWD -- ARGS`
//!   (https://docs.docker.com/reference/cli/sbx/run/). Each variable goes as
//!   a bare `-e NAME`, which sbx copies from its own environment, so the
//!   token is never an argument. Loopback URLs become `host.docker.internal`
//!   (sbx's development workflow page), and the policy lines they need are
//!   printed, not run.
//! - `srt`: `srt --settings FILE -- COMMAND ARGS` (srt 0.0.81 `dist/cli.js:
//!   270-278`). The `--` matters: srt's commander would otherwise read a
//!   harness's `-c` as its own option. The settings allow the chat's loopback
//!   services by IP literal only, writes to the working folder, the
//!   harness's state and `/tmp`, and hide the user's secrets, the daemon's
//!   folder and the other harnesses' credentials.

use serde_json::json;

use super::args::{Args, Boundary};
use super::harness::{Harness, HARNESSES};

/// Variables `tk run` owns: removed from what the child inherits, then set
/// from this run only, so a value left by an outer `tk run` never leaks in.
pub const OWNED_VARS: &[&str] = &[
    "TK_CHAT_TOKEN",
    "TK_CHAT_TOKEN_FILE",
    "TK_CHAT_ID",
    "TK_HARNESS",
    "TK_BOUNDARY",
    "TK_OTLP_ENDPOINT",
    "TK_GATEWAY_URL",
];

pub struct Inputs<'a> {
    pub args: &'a Args,
    pub chat: String,
    /// The working folder, absolute.
    pub cwd: String,
    pub home: String,
    pub token: Option<String>,
    /// A loopback `http://` URL (checked by `client::loopback_http`).
    pub otlp: Option<String>,
    pub gateway: Option<String>,
    /// The daemon's folder (socket, token), hidden under srt.
    pub daemon_dir: Option<String>,
    /// Where srt's settings file for this chat goes.
    pub settings_path: String,
    /// Where the chat token is written for hooks (`TK_CHAT_TOKEN_FILE`),
    /// beside srt's settings in the daemon's 0700 folder. Named to the child
    /// only under `--boundary none`.
    pub token_file: String,
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub program: String,
    pub args: Vec<String>,
    /// Set in the child. Holds the token: never printed (`describe` shows
    /// names only).
    pub env_set: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub kit: Option<String>,
    pub sandbox_name: Option<String>,
    /// srt's settings, written to `settings_path` (0600) before the start.
    pub srt_settings: Option<String>,
    /// The chat's folders and the harness's state folders, for `run.start`
    /// (the feed reads them).
    pub folders: Vec<String>,
    pub state_dirs: Vec<String>,
    /// Lines for the terminal: what the user may want to run themselves.
    pub notes: Vec<String>,
}

/// A URL's parts: scheme, host (brackets removed), port (explicit or the
/// scheme's), and the rest from the first `/`. Only `http` and `https`.
pub fn split_url(url: &str) -> Option<(&str, String, u16, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, String::new()),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(r) = authority.strip_prefix('[') {
        let (h, p) = r.split_once(']')?;
        (h.to_string(), p.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let port = match port {
        Some(p) => p.parse().ok().filter(|p| *p > 0)?,
        None if scheme == "https" => 443,
        None => 80,
    };
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
    {
        return None;
    }
    Some((scheme, host, port, path))
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

fn with_host(scheme: &str, host: &str, port: u16, path: &str) -> String {
    let h = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    format!("{scheme}://{h}:{port}{path}")
}

/// A loopback URL as srt can allow it: `localhost` becomes `127.0.0.1`,
/// since srt resolves `localhost` to loopback and denies loopback unless an
/// IP literal is listed (srt README, "Resolved-address check").
fn srt_target(url: &str) -> Option<(String, String)> {
    let (scheme, host, port, path) = split_url(url)?;
    let host = if host == "localhost" {
        "127.0.0.1".to_string()
    } else {
        host
    };
    let allow = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Some((with_host(scheme, &host, port, &path), allow))
}

pub fn build(i: &Inputs) -> Result<Plan, String> {
    let h: &Harness = i.args.harness;
    let home = i.home.trim_end_matches('/');
    let under_home = |rel: &str| format!("{home}/{rel}");
    let state_dirs: Vec<String> = h.state.iter().map(|s| under_home(s)).collect();
    let mut env: Vec<(String, String)> = vec![
        ("TK_CHAT_ID".into(), i.chat.clone()),
        ("TK_HARNESS".into(), h.id.into()),
        ("TK_BOUNDARY".into(), i.args.boundary.name().into()),
    ];
    let mut otlp = i.otlp.clone();
    let mut gateway = i.gateway.clone();
    if let Some(g) = &gateway {
        if split_url(g).is_none() {
            return Err(format!(
                "--gateway-url {g:?} is not an http(s) URL with a host"
            ));
        }
    }
    let mut notes = Vec::new();
    let mut program = h.command.to_string();
    let mut args: Vec<String> = Vec::new();
    let mut kit = None;
    let mut sandbox_name = None;
    let mut srt_settings = None;
    match i.args.boundary {
        Boundary::None => {
            args.extend(i.args.rest.iter().cloned());
            notes.push(format!(
                "tk run: {} runs as you, with no sandbox; only its own gate and the OS feed watch it",
                h.id
            ));
        }
        Boundary::Sbx => {
            let sbx = h.sbx.ok_or_else(|| {
                format!(
                    "{} has no sbx form here (no built-in agent, no kit known); use srt or none",
                    h.id
                )
            })?;
            for url in [&mut otlp, &mut gateway].into_iter().flatten() {
                let (scheme, host, port, path) = split_url(url).ok_or("bad URL")?;
                if is_loopback(&host) {
                    *url = with_host(scheme, "host.docker.internal", port, &path);
                    notes.push(format!(
                        "tk run: the sandbox reaches the host's port {port} only if your sbx policy allows it: \
                         sbx policy allow network localhost:{port} (tk does not change sbx policy)"
                    ));
                }
            }
            program = "sbx".into();
            let name = format!("tk-{}", i.chat);
            args.extend(["run".to_string(), "--name".into(), name.clone()]);
            sandbox_name = Some(name);
            // The variables are named here and copied by sbx from its own
            // environment (`-e KEY`), which the child gets below.
            let mut names: Vec<&str> = OWNED_VARS.to_vec();
            names.retain(|n| *n != "TK_CHAT_TOKEN" || i.token.is_some());
            // A host path means nothing inside the sandbox.
            names.retain(|n| *n != "TK_CHAT_TOKEN_FILE");
            names.retain(|n| *n != "TK_OTLP_ENDPOINT" || otlp.is_some());
            names.retain(|n| *n != "TK_GATEWAY_URL" || gateway.is_some());
            for n in names {
                args.extend(["-e".to_string(), n.to_string()]);
            }
            let k = i.args.kit.clone().or(sbx.kit.map(str::to_string));
            if let Some(k) = &k {
                args.extend(["--kit".to_string(), k.clone()]);
            }
            kit = k;
            args.push(sbx.agent.into());
            args.push(i.cwd.clone());
            if !i.args.rest.is_empty() {
                args.push("--".into());
                args.extend(i.args.rest.iter().cloned());
            }
            notes.push(
                "tk run: sbx runs the harness with its own defaults inside the VM, and local stdio MCP servers run on the host"
                    .into(),
            );
        }
        Boundary::Srt => {
            // srt's own subcommands must not be reachable as a harness name.
            if matches!(h.command, "proxy" | "windows-install" | "windows-uninstall") {
                return Err(format!("{} collides with an srt subcommand", h.command));
            }
            let mut allowed: Vec<String> = Vec::new();
            for url in [&mut otlp, &mut gateway].into_iter().flatten() {
                let (rewritten, allow) = srt_target(url).ok_or("bad URL")?;
                *url = rewritten;
                allowed.push(allow);
            }
            if let Some(m) = &i.args.model_url {
                let (_, allow) = srt_target(m)
                    .ok_or_else(|| format!("--model-url {m:?} is not an http(s) URL"))?;
                allowed.push(allow);
            }
            allowed.extend(i.args.allow_hosts.iter().cloned());
            allowed.sort();
            allowed.dedup();
            let mut deny_read: Vec<String> = [
                ".ssh",
                ".gnupg",
                ".aws",
                ".config/gh",
                ".kube",
                ".password-store",
                ".local/share/keyrings",
                ".docker/config.json",
                ".netrc",
                ".git-credentials",
                ".tk",
            ]
            .iter()
            .map(|r| under_home(r))
            .collect();
            if let Some(d) = &i.daemon_dir {
                deny_read.push(d.clone());
            }
            for other in HARNESSES.iter().filter(|o| o.id != h.id) {
                if let Some(c) = other.credential {
                    if Some(c) != h.credential {
                        deny_read.push(under_home(c));
                    }
                }
            }
            deny_read.sort();
            deny_read.dedup();
            let mut allow_write = vec![i.cwd.clone()];
            allow_write.extend(state_dirs.iter().cloned());
            allow_write.push("/tmp".into());
            let settings = json!({
                "network": {
                    "allowedDomains": allowed,
                    "deniedDomains": [],
                },
                "filesystem": {
                    "denyRead": deny_read,
                    "allowRead": [],
                    "allowWrite": allow_write,
                    // srt already refuses writes to .git/hooks, .git/config,
                    // shell rc files, .vscode and .claude/commands (README,
                    // "Mandatory Deny Paths").
                    "denyWrite": [],
                },
            });
            srt_settings =
                Some(serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?);
            program = "srt".into();
            args.extend([
                "--settings".to_string(),
                i.settings_path.clone(),
                "--".into(),
                h.command.into(),
            ]);
            args.extend(i.args.rest.iter().cloned());
            // Node's fetch takes srt's proxy only when asked (srt README,
            // "Node.js (Windows)": srt sets this on Windows only).
            env.push(("NODE_USE_ENV_PROXY".into(), "1".into()));
        }
    }
    if let Some(t) = &i.token {
        env.push(("TK_CHAT_TOKEN".into(), t.clone()));
        if i.args.boundary == Boundary::None {
            env.push(("TK_CHAT_TOKEN_FILE".into(), i.token_file.clone()));
        }
    }
    if let Some(o) = &otlp {
        env.push(("TK_OTLP_ENDPOINT".into(), o.clone()));
    }
    if let Some(g) = &gateway {
        env.push(("TK_GATEWAY_URL".into(), g.clone()));
    }
    let dangerous: Vec<&String> = i
        .args
        .rest
        .iter()
        .filter(|a| a.to_ascii_lowercase().starts_with("--dangerously") || a.as_str() == "--yolo")
        .collect();
    if !dangerous.is_empty() {
        notes.push(format!(
            "tk run: {} turns the harness's own approvals off; recorded in run.start",
            dangerous
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    let plan = Plan {
        program,
        args,
        env_set: env,
        env_remove: OWNED_VARS.iter().map(|s| s.to_string()).collect(),
        kit,
        sandbox_name,
        srt_settings,
        folders: vec![i.cwd.clone()],
        state_dirs,
        notes,
    };
    if let Some(t) = &i.token {
        // Defence in depth: the token never rides in an argument (rules
        // section 5, item 1).
        if plan.args.iter().any(|a| a.contains(t.as_str())) {
            return Err(
                "internal: the chat token reached the command line; refusing to start".into(),
            );
        }
    }
    Ok(plan)
}

impl Plan {
    /// The parts `exec::spawn` reads, for a start under another program
    /// path (tests point `sbx` and `srt` at fakes).
    pub fn clone_for_start(&self) -> Plan {
        Plan {
            program: self.program.clone(),
            args: self.args.clone(),
            env_set: self.env_set.clone(),
            env_remove: self.env_remove.clone(),
            kit: None,
            sandbox_name: None,
            srt_settings: None,
            folders: Vec::new(),
            state_dirs: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// The plan as the terminal shows it: argv, variable names (values only for
/// variables that carry no secret), the srt settings.
pub fn describe(p: &Plan) -> String {
    let mut s = String::new();
    s.push_str(&format!("program: {}\n", p.program));
    s.push_str(&format!(
        "argv: {}\n",
        serde_json::to_string(&p.args).unwrap_or_default()
    ));
    for (k, v) in &p.env_set {
        if k == "TK_CHAT_TOKEN" {
            s.push_str(&format!("env: {k}=<chat token, not shown>\n"));
        } else {
            s.push_str(&format!("env: {k}={v}\n"));
        }
    }
    if let Some(k) = &p.kit {
        s.push_str(&format!("kit: {k}\n"));
    }
    if let Some(t) = &p.srt_settings {
        s.push_str("srt settings:\n");
        s.push_str(t);
        s.push('\n');
    }
    for n in &p.notes {
        s.push_str(n);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::args::parse;

    const TOKEN: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn inputs<'a>(args: &'a Args) -> Inputs<'a> {
        Inputs {
            args,
            chat: "chat-b".into(),
            cwd: "/home/u/proj dir".into(),
            home: "/home/u".into(),
            token: Some(TOKEN.into()),
            otlp: Some("http://127.0.0.1:4318".into()),
            gateway: Some("http://127.0.0.1:8811/mcp".into()),
            daemon_dir: Some("/run/user/1000/tk".into()),
            settings_path: "/run/user/1000/tk/srt/chat-b.json".into(),
            token_file: "/run/user/1000/tk/chats/chat-b.token".into(),
        }
    }

    fn a(v: &[&str]) -> Args {
        parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    fn env<'p>(p: &'p Plan, k: &str) -> Option<&'p str> {
        p.env_set
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn none_is_the_bare_harness_with_the_chats_variables() {
        let args = a(&["pi", "--boundary", "none", "--", "-c", "a b"]);
        let p = build(&inputs(&args)).unwrap();
        assert_eq!(p.program, "pi");
        assert_eq!(p.args, vec!["-c", "a b"]);
        assert_eq!(env(&p, "TK_CHAT_TOKEN"), Some(TOKEN));
        assert_eq!(env(&p, "TK_OTLP_ENDPOINT"), Some("http://127.0.0.1:4318"));
        assert_eq!(env(&p, "TK_GATEWAY_URL"), Some("http://127.0.0.1:8811/mcp"));
        assert_eq!(env(&p, "TK_BOUNDARY"), Some("none"));
        assert!(p.notes.iter().any(|n| n.contains("runs as you")));
        for v in OWNED_VARS {
            assert!(p.env_remove.iter().any(|r| r == v), "{v} is cleared first");
        }
    }

    /// Integration w1: hooks read the chat token from a file when a harness
    /// strips secret-named variables (rules case 76). Only the bare boundary
    /// names it: srt hides the daemon's folder and sbx cannot see host paths.
    #[test]
    fn the_token_file_is_named_only_for_the_bare_boundary_and_only_with_a_token() {
        for (b, want) in [("none", true), ("srt", false), ("sbx", false)] {
            let args = a(&["pi", "--boundary", b]);
            let p = build(&inputs(&args)).unwrap();
            assert_eq!(
                env(&p, "TK_CHAT_TOKEN_FILE"),
                want.then_some("/run/user/1000/tk/chats/chat-b.token"),
                "{b}"
            );
            assert!(
                !p.args.iter().any(|x| x.contains("TK_CHAT_TOKEN_FILE")),
                "{b}"
            );
        }
        let args = a(&["pi", "--boundary", "none"]);
        let mut i = inputs(&args);
        i.token = None;
        assert!(env(&build(&i).unwrap(), "TK_CHAT_TOKEN_FILE").is_none());
        assert!(OWNED_VARS.contains(&"TK_CHAT_TOKEN_FILE"));
    }

    #[test]
    fn sbx_for_pi_uses_the_kit_names_variables_and_rewrites_loopback() {
        let args = a(&["pi", "--boundary", "sbx", "--", "-p", "hi"]);
        let p = build(&inputs(&args)).unwrap();
        assert_eq!(p.program, "sbx");
        assert_eq!(
            p.args,
            vec![
                "run",
                "--name",
                "tk-chat-b",
                "-e",
                "TK_CHAT_TOKEN",
                "-e",
                "TK_CHAT_ID",
                "-e",
                "TK_HARNESS",
                "-e",
                "TK_BOUNDARY",
                "-e",
                "TK_OTLP_ENDPOINT",
                "-e",
                "TK_GATEWAY_URL",
                "--kit",
                "docker.io/sbx/pi-kit:latest",
                "pi",
                "/home/u/proj dir",
                "--",
                "-p",
                "hi",
            ]
        );
        assert_eq!(
            env(&p, "TK_OTLP_ENDPOINT"),
            Some("http://host.docker.internal:4318")
        );
        assert_eq!(
            env(&p, "TK_GATEWAY_URL"),
            Some("http://host.docker.internal:8811/mcp")
        );
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("sbx policy allow network localhost:4318")));
        assert!(p.notes.iter().any(|n| n.contains("localhost:8811")));
        assert_eq!(p.kit.as_deref(), Some("docker.io/sbx/pi-kit:latest"));
    }

    #[test]
    fn sbx_for_a_built_in_agent_has_no_kit_and_a_harness_without_one_is_refused() {
        let args = a(&["claude-code", "--boundary", "sbx"]);
        let p = build(&inputs(&args)).unwrap();
        assert!(!p.args.contains(&"--kit".to_string()));
        assert_eq!(p.args[p.args.len() - 2], "claude");
        assert!(
            !p.args.contains(&"--".to_string()),
            "no separator without harness args"
        );
        let args = a(&["goose", "--boundary", "sbx"]);
        assert!(build(&inputs(&args)).unwrap_err().contains("no sbx form"));
    }

    #[test]
    fn srt_puts_the_separator_before_the_harness_and_allows_only_named_hosts() {
        let args = a(&[
            "pi",
            "--boundary",
            "srt",
            "--model-url",
            "http://localhost:8080/v1",
            "--allow-host",
            "[::1]:9000",
            "--",
            "-c",
        ]);
        let p = build(&inputs(&args)).unwrap();
        assert_eq!(p.program, "srt");
        assert_eq!(
            p.args,
            vec![
                "--settings",
                "/run/user/1000/tk/srt/chat-b.json",
                "--",
                "pi",
                "-c"
            ]
        );
        assert_eq!(env(&p, "NODE_USE_ENV_PROXY"), Some("1"));
        let s: serde_json::Value =
            serde_json::from_str(p.srt_settings.as_deref().unwrap()).unwrap();
        assert_eq!(
            s["network"]["allowedDomains"],
            json!([
                "127.0.0.1:4318",
                "127.0.0.1:8080",
                "127.0.0.1:8811",
                "[::1]:9000"
            ])
        );
        let deny = s["filesystem"]["denyRead"].as_array().unwrap();
        for d in [
            "/home/u/.ssh",
            "/home/u/.tk",
            "/run/user/1000/tk",
            "/home/u/.claude/.credentials.json",
            "/home/u/.codex/auth.json",
        ] {
            assert!(deny.iter().any(|v| v == d), "{d}");
        }
        assert!(
            !deny.iter().any(|v| v == "/home/u/.pi/agent/auth.json"),
            "Pi keeps its own login"
        );
        assert_eq!(
            s["filesystem"]["allowWrite"],
            json!(["/home/u/proj dir", "/home/u/.pi/agent", "/tmp"])
        );
    }

    #[test]
    fn srt_for_claude_code_hides_pis_login_and_not_its_own() {
        let args = a(&["claude-code", "--boundary", "srt"]);
        let p = build(&inputs(&args)).unwrap();
        let s: serde_json::Value =
            serde_json::from_str(p.srt_settings.as_deref().unwrap()).unwrap();
        let deny = s["filesystem"]["denyRead"].as_array().unwrap();
        assert!(deny.iter().any(|v| v == "/home/u/.pi/agent/auth.json"));
        assert!(!deny
            .iter()
            .any(|v| v == "/home/u/.claude/.credentials.json"));
    }

    #[test]
    fn no_daemon_means_no_token_and_no_feed_variables() {
        let args = a(&["pi", "--boundary", "sbx", "--no-daemon"]);
        let mut i = inputs(&args);
        i.token = None;
        i.otlp = None;
        i.gateway = None;
        let p = build(&i).unwrap();
        assert!(env(&p, "TK_CHAT_TOKEN").is_none());
        assert!(!p.args.contains(&"TK_CHAT_TOKEN".to_string()));
        assert!(!p.args.contains(&"TK_OTLP_ENDPOINT".to_string()));
    }

    #[test]
    fn the_token_never_reaches_the_argv_or_the_description() {
        for b in ["none", "sbx", "srt"] {
            let args = a(&["pi", "--boundary", b, "--", "x"]);
            let p = build(&inputs(&args)).unwrap();
            assert!(p.args.iter().all(|x| !x.contains(TOKEN)), "{b}");
            assert!(!describe(&p).contains(TOKEN), "{b}");
        }
        // A harness argument that happens to hold the token is refused.
        let args = a(&["pi", "--boundary", "none", "--", TOKEN]);
        assert!(build(&inputs(&args)).unwrap_err().contains("token"));
    }

    #[test]
    fn dangerous_flags_are_noted() {
        let args = a(&[
            "claude-code",
            "--boundary",
            "none",
            "--",
            "--dangerously-skip-permissions",
        ]);
        let p = build(&inputs(&args)).unwrap();
        assert!(p
            .notes
            .iter()
            .any(|n| n.contains("--dangerously-skip-permissions")));
    }

    #[test]
    fn urls_are_parsed_strictly() {
        assert_eq!(
            split_url("http://127.0.0.1:8811/mcp"),
            Some(("http", "127.0.0.1".into(), 8811, "/mcp".into()))
        );
        assert_eq!(
            split_url("https://h"),
            Some(("https", "h".into(), 443, String::new()))
        );
        assert_eq!(
            split_url("http://[::1]:4318"),
            Some(("http", "::1".into(), 4318, String::new()))
        );
        for bad in [
            "ftp://h",
            "http://",
            "http://u:p@h:1",
            "http://h:0",
            "http://h:x",
            "h:80",
            "http://a b",
        ] {
            assert!(split_url(bad).is_none(), "{bad}");
        }
        let args = a(&[
            "pi",
            "--boundary",
            "none",
            "--gateway-url",
            "file:///etc/passwd",
        ]);
        let mut i = inputs(&args);
        i.gateway = args.gateway_url.clone();
        assert!(build(&i).is_err());
    }
}

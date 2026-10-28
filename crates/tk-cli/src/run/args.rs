//! `tk run <harness> --boundary sbx|srt|none [options] [-- harness args]`.
//!
//! Every value is taken literally; nothing is split or expanded. The
//! harness's own arguments go after `--` and reach it as separate argv
//! items (rules section 5, item 3).

use super::harness::{self, Harness};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Sbx,
    Srt,
    None,
}

impl Boundary {
    pub fn name(self) -> &'static str {
        match self {
            Boundary::Sbx => "sbx",
            Boundary::Srt => "srt",
            Boundary::None => "none",
        }
    }
}

#[derive(Debug)]
pub struct Args {
    pub harness: &'static Harness,
    pub boundary: Boundary,
    pub chat: Option<String>,
    pub cwd: Option<String>,
    pub gateway_url: Option<String>,
    pub model_url: Option<String>,
    pub allow_hosts: Vec<String>,
    pub kit: Option<String>,
    pub no_daemon: bool,
    pub dry_run: bool,
    pub rest: Vec<String>,
}

pub const USAGE: &str = "usage: tk run <harness> --boundary sbx|srt|none [--chat ID] [--cwd DIR] \
[--gateway-url URL] [--model-url URL] [--allow-host HOST:PORT]... [--kit REF] [--no-daemon] \
[--dry-run] [-- harness args...]";

/// A chat id: 1 to 64 of `[A-Za-z0-9._-]`, not starting with `-` or `.`.
pub fn valid_chat(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with(['-', '.'])
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `host:port` for srt's allow list: a host name or IPv4 literal, or a
/// bracketed IPv6 literal, then a port. No wildcard: `tk run` widens srt's
/// network by named destinations only.
pub fn valid_host_port(s: &str) -> bool {
    let (host, port, v6) = if let Some(rest) = s.strip_prefix('[') {
        match rest.split_once("]:") {
            Some((h, p)) => (h, p, true),
            None => return false,
        }
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h, p, false),
            None => return false,
        }
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && if v6 {
            host.bytes().all(|b| b.is_ascii_hexdigit() || b == b':')
        } else {
            host.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        };
    host_ok && port.parse::<u16>().is_ok_and(|p| p > 0) && port.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse(args: &[String]) -> Result<Args, String> {
    let Some(id) = args.first() else {
        return Err(USAGE.to_string());
    };
    let harness = match harness::find(id) {
        Some(h) => h,
        None if harness::NOT_RUN.contains(&id.as_str()) => {
            return Err(format!(
                "tk run does not start {id}: its preset names no agent command line to wrap"
            ))
        }
        None => return Err(format!("unknown harness {id:?}\n{USAGE}")),
    };
    let mut out = Args {
        harness,
        boundary: Boundary::None,
        chat: None,
        cwd: None,
        gateway_url: None,
        model_url: None,
        allow_hosts: Vec::new(),
        kit: None,
        no_daemon: false,
        dry_run: false,
        rest: Vec::new(),
    };
    let mut boundary = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        let mut take = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a {
            "--" => {
                out.rest = args[i + 1..].to_vec();
                break;
            }
            "--boundary" => {
                boundary = Some(match take("--boundary")?.as_str() {
                    "sbx" => Boundary::Sbx,
                    "srt" => Boundary::Srt,
                    "none" => Boundary::None,
                    other => return Err(format!("--boundary is sbx, srt or none, not {other:?}")),
                })
            }
            "--chat" => {
                let v = take("--chat")?;
                if !valid_chat(&v) {
                    return Err(format!("--chat {v:?}: 1 to 64 of A-Z a-z 0-9 . _ -"));
                }
                out.chat = Some(v);
            }
            "--cwd" => out.cwd = Some(take("--cwd")?),
            "--gateway-url" => out.gateway_url = Some(take("--gateway-url")?),
            "--model-url" => out.model_url = Some(take("--model-url")?),
            "--allow-host" => {
                let v = take("--allow-host")?;
                if !valid_host_port(&v) {
                    return Err(format!("--allow-host {v:?}: give HOST:PORT, no wildcard"));
                }
                out.allow_hosts.push(v);
            }
            "--kit" => out.kit = Some(take("--kit")?),
            "--no-daemon" => out.no_daemon = true,
            "--dry-run" => out.dry_run = true,
            other => {
                return Err(format!(
                    "unknown option {other:?}; harness arguments go after --\n{USAGE}"
                ))
            }
        }
        i += 1;
    }
    // The boundary is never assumed: running a harness with no sandbox is a
    // choice the command line states.
    out.boundary = boundary.ok_or("--boundary is required: sbx, srt or none")?;
    if out.boundary != Boundary::Srt && (!out.allow_hosts.is_empty() || out.model_url.is_some()) {
        return Err(
            "--allow-host and --model-url shape srt's allow list; they need --boundary srt".into(),
        );
    }
    if out.kit.is_some() && out.boundary != Boundary::Sbx {
        return Err("--kit needs --boundary sbx".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_harness_args_after_the_separator_are_kept_whole() {
        let a = parse(&s(&[
            "pi",
            "--boundary",
            "srt",
            "--",
            "-c",
            "--model",
            "a b; rm -rf ~",
        ]))
        .unwrap();
        assert_eq!(a.harness.id, "pi");
        assert_eq!(a.boundary, Boundary::Srt);
        assert_eq!(a.rest, s(&["-c", "--model", "a b; rm -rf ~"]));
    }

    #[test]
    fn the_boundary_is_required_and_checked() {
        assert!(parse(&s(&["pi"])).unwrap_err().contains("required"));
        assert!(parse(&s(&["pi", "--boundary", "docker"])).is_err());
        assert!(parse(&s(&["pi", "--boundary"])).is_err());
    }

    #[test]
    fn unknown_harnesses_and_options_are_refused() {
        assert!(parse(&s(&["vim", "--boundary", "none"]))
            .unwrap_err()
            .contains("unknown harness"));
        for app in ["cursor", "zed"] {
            assert!(parse(&s(&[app, "--boundary", "none"]))
                .unwrap_err()
                .contains("tk run does not start"));
        }
        assert!(parse(&s(&["pi", "--boundary", "none", "--model", "x"]))
            .unwrap_err()
            .contains("after --"));
        assert!(parse(&s(&[])).is_err());
    }

    #[test]
    fn chat_ids_and_hosts_are_shaped() {
        assert!(valid_chat("chat-b_1.2"));
        for bad in ["", "-x", ".x", "a/b", "a b", &"a".repeat(65)] {
            assert!(!valid_chat(bad), "{bad:?}");
        }
        for ok in [
            "127.0.0.1:8811",
            "localhost:8080",
            "[::1]:4318",
            "api.example.com:443",
        ] {
            assert!(valid_host_port(ok), "{ok}");
        }
        for bad in [
            "*.example.com:443",
            "127.0.0.1",
            "h:0",
            "h:99999",
            "a b:1",
            "[::1]",
            "::1:443",
        ] {
            assert!(!valid_host_port(bad), "{bad}");
        }
        assert!(parse(&s(&["pi", "--boundary", "srt", "--allow-host", "*:443"])).is_err());
    }

    #[test]
    fn srt_and_sbx_options_need_their_boundary() {
        assert!(parse(&s(&["pi", "--boundary", "none", "--allow-host", "h:1"])).is_err());
        assert!(parse(&s(&["pi", "--boundary", "srt", "--kit", "x"])).is_err());
        assert!(parse(&s(&["pi", "--boundary", "sbx", "--kit", "x"])).is_ok());
    }
}

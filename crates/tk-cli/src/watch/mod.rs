//! `tk watch` and `tk status` (lane J, wave 2; toolkit plan T5). Views on
//! the daemon (`contracts/daemon-protocol.md`) and on the preset table
//! (`contracts/harness-presets.json`):
//!
//! - `tk watch tail`: the feed, live, one line per event.
//! - `tk watch report`: counts, approvals and the check's flags over a window.
//! - `tk watch --approve` (or `tk watch approve`): a terminal approver for the
//!   daemon's `approve` channel, which the hooks of Claude Code, Codex and
//!   OpenCode ask when a rule says `ask`.
//! - `tk status`: per agent, whether tk's gate is there, whether it fails
//!   open, the version its row was verified at, and what the daemon sees.
//!
//! `main.rs` sends both `tk watch ...` and `tk status ...` here with the
//! subcommand word still first.

mod approve;
mod conn;
pub mod cu;
mod peer;
mod register;
mod report;
mod status;
mod tail;
mod text;

#[cfg(test)]
mod live_tests;

pub const USAGE: &str = "usage: tk watch <tail|report|approve> [options]
       tk watch --approve [options]
       tk status [--json] [--probe]

  tail      the daemon's feed, live; --since 10m shows the backlog first
  report    counts, approvals and flags over a window (default the last day)
  approve   answer the daemon's approval requests from this terminal
  status    each agent's gate, its fail mode and the daemon's state

Every command takes --socket <path> (absolute) to use another daemon.";

pub fn main(args: &[String]) -> u8 {
    match args.first().map(String::as_str) {
        Some("status") => run_parsed(&args[1..], status::FLAGS, status::USAGE, status::main),
        Some("watch") => watch(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn watch(args: &[String]) -> u8 {
    match args.first().map(String::as_str) {
        Some("tail") => run_parsed(&args[1..], tail::FLAGS, tail::USAGE, tail::main),
        Some("report") => run_parsed(&args[1..], report::FLAGS, report::USAGE, report::main),
        Some("approve") => run_parsed(&args[1..], approve::FLAGS, approve::USAGE, approve::main),
        Some("--approve") => run_parsed(&args[1..], approve::FLAGS, approve::USAGE, approve::main),
        Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn run_parsed(args: &[String], flags: &[Flag], usage: &str, f: fn(&Args) -> u8) -> u8 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{usage}");
        return 0;
    }
    match Args::parse(args, flags) {
        Ok(a) => f(&a),
        Err(e) => {
            eprintln!("{e}\n{usage}");
            2
        }
    }
}

/// A flag a command takes: its name and whether it takes a value.
pub type Flag = (&'static str, bool);

/// Parsed flags. Only the flags a command lists are accepted; a flag given
/// twice, an unknown flag and a bare word are usage errors.
#[derive(Debug, Default)]
pub struct Args {
    values: Vec<(&'static str, String)>,
    flags: Vec<&'static str>,
}

impl Args {
    pub fn parse(args: &[String], known: &[Flag]) -> Result<Args, String> {
        let mut a = Args::default();
        let mut i = 0;
        while i < args.len() {
            let raw = args[i].as_str();
            let (name, inline) = match raw.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
                _ => (raw, None),
            };
            let Some(&(flag, takes)) = known.iter().find(|(n, _)| *n == name) else {
                return Err(format!("unknown argument {raw:?}"));
            };
            if a.has(flag) {
                return Err(format!("{flag} given twice"));
            }
            if takes {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("{flag} needs a value"))?
                    }
                };
                a.values.push((flag, v));
            } else {
                if inline.is_some() {
                    return Err(format!("{flag} takes no value"));
                }
                a.flags.push(flag);
            }
            i += 1;
        }
        Ok(a)
    }

    fn has(&self, flag: &str) -> bool {
        self.flags.contains(&flag) || self.values.iter().any(|(n, _)| *n == flag)
    }

    pub fn value(&self, flag: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(n, _)| *n == flag)
            .map(|(_, v)| v.as_str())
    }

    pub fn flag(&self, flag: &str) -> bool {
        self.flags.contains(&flag)
    }

    pub fn socket(&self) -> Result<std::path::PathBuf, String> {
        conn::socket(self.value("--socket"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_subcommand_is_a_usage_error() {
        assert_eq!(main(&s(&["watch"])), 2);
        assert_eq!(main(&s(&["watch", "frobnicate"])), 2);
        assert_eq!(main(&s(&[])), 2);
    }

    #[test]
    fn help_exits_zero() {
        assert_eq!(main(&s(&["watch", "--help"])), 0);
        assert_eq!(main(&s(&["watch", "tail", "--help"])), 0);
    }

    #[test]
    fn flags_parse_with_space_or_equals_and_refuse_the_rest() {
        let known: &[Flag] = &[("--since", true), ("--json", false)];
        let a = Args::parse(&s(&["--since", "10m", "--json"]), known).unwrap();
        assert_eq!(a.value("--since"), Some("10m"));
        assert!(a.flag("--json"));
        let a = Args::parse(&s(&["--since=2h"]), known).unwrap();
        assert_eq!(a.value("--since"), Some("2h"));
        for bad in [
            &["--since"][..],
            &["--json=1"],
            &["--json", "--json"],
            &["--chat", "x"],
            &["tail"],
        ] {
            assert!(Args::parse(&s(bad), known).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bad_flags_are_usage_errors_before_any_connection() {
        assert_eq!(main(&s(&["watch", "tail", "--since", "soon"])), 2);
        assert_eq!(main(&s(&["watch", "report", "--until", "x"])), 2);
        assert_eq!(main(&s(&["watch", "tail", "--socket", "rel/sock"])), 2);
        assert_eq!(main(&s(&["status", "--frob"])), 2);
    }
}

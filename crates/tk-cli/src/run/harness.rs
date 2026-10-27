//! What `tk run` needs to know per harness: the command it starts, how sbx
//! runs it, and the folders it keeps its own state in. Ids and commands are
//! `contracts/harness-presets.json` (`id`, `cli`).
//!
//! sbx: the built-in agents of `sbx run` are claude, codex, copilot, cursor,
//! devin, docker-agent, droid, gemini, kiro, opencode, shell
//! (https://docs.docker.com/reference/cli/sbx/run/, read 27 Oct 2026). Pi
//! runs from the community kit `docker.io/sbx/pi-kit:latest` as Pi 1.4.0's
//! `docs/containerization.md:110` and the kit's README give it:
//! `sbx run --kit "docker.io/sbx/pi-kit:latest" pi`. The other harnesses
//! have no sbx form here; `--boundary sbx` refuses them rather than guess.
//!
//! State folders are where a harness writes its sessions and settings, so
//! srt lets it write there and the feed reads those writes as its own. Pi
//! `~/.pi/agent` (Pi's docs; the sbx kit's `credentialFile`), Claude Code
//! `~/.claude` and `~/.claude.json` (box plan 5.4), Codex `~/.codex`
//! (the fork's harness.rs), OpenCode and Gemini from their presets' config
//! paths (I until lane C verifies the presets).

#[derive(Debug)]
pub struct Harness {
    pub id: &'static str,
    /// The command, from the presets' `cli`.
    pub command: &'static str,
    pub sbx: Option<Sbx>,
    /// Folders and files under the home it writes on its own.
    pub state: &'static [&'static str],
    /// Its own credential file under the home, which srt leaves readable
    /// for it and hides from every other harness.
    pub credential: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub struct Sbx {
    /// The AGENT argument of `sbx run`.
    pub agent: &'static str,
    /// A kit passed with `--kit`, when the agent is not built in.
    pub kit: Option<&'static str>,
}

pub const PI_KIT: &str = "docker.io/sbx/pi-kit:latest";

pub const HARNESSES: &[Harness] = &[
    Harness {
        id: "pi",
        command: "pi",
        sbx: Some(Sbx {
            agent: "pi",
            kit: Some(PI_KIT),
        }),
        state: &[".pi/agent"],
        credential: Some(".pi/agent/auth.json"),
    },
    Harness {
        id: "claude-code",
        command: "claude",
        sbx: Some(Sbx {
            agent: "claude",
            kit: None,
        }),
        state: &[".claude", ".claude.json"],
        credential: Some(".claude/.credentials.json"),
    },
    Harness {
        id: "codex",
        command: "codex",
        sbx: Some(Sbx {
            agent: "codex",
            kit: None,
        }),
        state: &[".codex"],
        credential: Some(".codex/auth.json"),
    },
    Harness {
        id: "opencode",
        command: "opencode",
        sbx: Some(Sbx {
            agent: "opencode",
            kit: None,
        }),
        state: &[".local/share/opencode", ".config/opencode"],
        credential: None,
    },
    Harness {
        id: "gemini",
        command: "gemini",
        sbx: Some(Sbx {
            agent: "gemini",
            kit: None,
        }),
        state: &[".gemini"],
        credential: Some(".gemini/oauth_creds.json"),
    },
    Harness {
        id: "copilot-cli",
        command: "copilot",
        sbx: Some(Sbx {
            agent: "copilot",
            kit: None,
        }),
        state: &[".copilot"],
        credential: None,
    },
    Harness {
        id: "openclaw",
        command: "openclaw",
        sbx: None,
        state: &[".openclaw"],
        credential: None,
    },
    Harness {
        id: "goose",
        command: "goose",
        sbx: None,
        state: &[".config/goose", ".local/share/goose"],
        credential: None,
    },
    Harness {
        id: "qwen-code",
        command: "qwen",
        sbx: None,
        state: &[".qwen"],
        credential: None,
    },
    Harness {
        id: "amp",
        command: "amp",
        sbx: None,
        state: &[".config/amp", ".local/share/amp"],
        credential: None,
    },
    Harness {
        id: "crush",
        command: "crush",
        sbx: None,
        state: &[".config/crush", ".local/share/crush"],
        credential: None,
    },
];

/// Ids in the presets that have no command line to start (`cli: null`):
/// Cursor, Hermes and Zed are apps or have no CLI named there.
pub const NO_CLI: &[&str] = &["cursor", "hermes", "zed"];

pub fn find(id: &str) -> Option<&'static Harness> {
    HARNESSES.iter().find(|h| h.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESETS: &str = include_str!("../../../../contracts/harness-presets.json");

    #[test]
    fn every_preset_with_a_cli_is_here_with_the_same_command() {
        let v: serde_json::Value = serde_json::from_str(PRESETS).unwrap();
        let rows = v["harnesses"].as_array().unwrap();
        assert_eq!(rows.len(), HARNESSES.len() + NO_CLI.len());
        for row in rows {
            let id = row["id"].as_str().unwrap();
            match row["cli"].as_str() {
                Some(cli) => assert_eq!(find(id).map(|h| h.command), Some(cli), "{id}"),
                None => assert!(NO_CLI.contains(&id) && find(id).is_none(), "{id}"),
            }
        }
    }

    #[test]
    fn only_pi_needs_a_kit_and_state_paths_are_relative() {
        for h in HARNESSES {
            if let Some(s) = h.sbx {
                assert_eq!(s.kit.is_some(), h.id == "pi", "{}", h.id);
            }
            for p in h.state.iter().chain(h.credential.iter()) {
                assert!(!p.starts_with('/') && !p.contains(".."), "{p}");
            }
        }
    }
}

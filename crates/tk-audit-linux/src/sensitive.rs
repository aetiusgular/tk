//! Paths whose reading is worth a record, and files whose writing hands a
//! later process code to run. The Linux form of the macOS collector's list
//! (`macos/activity-collector/src/filter.rs:34-90`), plus the coding agents'
//! own credential files (Sysdig's coding-agent rules treat `~/.claude/`,
//! `~/.gemini/`, `~/.codex/` as sensitive) and tk's own token folder.

/// Folders under the home whose files are secrets.
pub const HOME_DIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gh",
    ".kube",
    ".password-store",
    ".local/share/keyrings",
    ".tk/run",
];

/// Files under the home that are secrets. The agent credential files:
/// Claude Code `~/.claude/.credentials.json` (box plan 5.4, V); Pi
/// `~/.pi/agent/auth.json` (the sbx pi kit's `credentialFile`, V); Codex
/// `~/.codex/auth.json` and Gemini `~/.gemini/oauth_creds.json` (I).
pub const HOME_FILES: &[&str] = &[
    ".netrc",
    ".git-credentials",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".docker/config.json",
    ".claude/.credentials.json",
    ".codex/auth.json",
    ".pi/agent/auth.json",
    ".gemini/oauth_creds.json",
];

/// Files outside the home that are secrets.
pub const SYSTEM_FILES: &[&str] = &["/etc/shadow", "/etc/gshadow", "/etc/sudoers"];
pub const SYSTEM_DIRS: &[&str] = &["/etc/sudoers.d"];

fn under(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

/// Whether reading or writing `path` touches a secret. `home` is the user's
/// home folder; `runtime_dir` is `$XDG_RUNTIME_DIR` when known (tk's token
/// sits in `<runtime_dir>/tk`).
pub fn is_sensitive(path: &str, home: &str, runtime_dir: Option<&str>) -> bool {
    if path.contains('\0') || path.split('/').any(|p| p == "..") {
        // `..` is never resolved here; such a path is read as sensitive so
        // a check cannot be walked around (the daemon's `inside` refuses it
        // the same way).
        return path.split('/').any(|p| p == "..");
    }
    let name = path.rsplit('/').next().unwrap_or("");
    if name == ".env" || name.starts_with(".env.") {
        return true;
    }
    if SYSTEM_FILES.contains(&path) || SYSTEM_DIRS.iter().any(|d| under(path, d)) {
        return true;
    }
    if let Some(rt) = runtime_dir {
        if under(path, &format!("{}/tk", rt.trim_end_matches('/'))) {
            return true;
        }
    }
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return false;
    }
    HOME_DIRS
        .iter()
        .any(|d| under(path, &format!("{home}/{d}")))
        || HOME_FILES.iter().any(|f| path == format!("{home}/{f}"))
}

/// Files a write to which can make something run later outside the chat:
/// git's hooks and config (rules cases 27, 31), Claude Code's settings and
/// hooks (CVE-2026-25725 in note B), editor tasks, shell start-up files,
/// user units and autostart entries. Read in any folder, inside the chat's
/// folders too.
pub fn is_trusted_file(path: &str, home: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let has = |a: &str, b: &str| parts.windows(2).any(|w| w[0] == a && w[1] == b);
    let name = parts.last().copied().unwrap_or("");
    if has(".git", "hooks")
        || has(".git", "config")
        || has(".git", "info")
        || path.ends_with("/.gitmodules")
    {
        return true;
    }
    if path.contains("/.git/modules/") && (name == "config" || path.contains("/hooks/")) {
        return true;
    }
    if has(".claude", "settings.json")
        || has(".claude", "settings.local.json")
        || has(".claude", "hooks")
    {
        return true;
    }
    if has(".vscode", "tasks.json") || has(".vscode", "settings.json") {
        return true;
    }
    if name == ".envrc" || name == ".npmrc" || name == ".worktreeinclude" {
        return true;
    }
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return false;
    }
    const HOME_RUNS: &[&str] = &[
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".profile",
        ".zshrc",
        ".zshenv",
        ".zprofile",
        ".config/fish/config.fish",
        ".ssh/authorized_keys",
        ".ssh/config",
        ".gitconfig",
        ".config/git/config",
    ];
    const HOME_RUN_DIRS: &[&str] = &[
        ".config/systemd/user",
        ".config/autostart",
        ".local/bin",
        ".pi/agent/extensions",
    ];
    HOME_RUNS.iter().any(|f| path == format!("{home}/{f}"))
        || HOME_RUN_DIRS
            .iter()
            .any(|d| under(path, &format!("{home}/{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "/home/ubuntu";

    #[test]
    fn secrets_in_the_home_and_the_system() {
        for p in [
            "/home/ubuntu/.ssh/id_test",
            "/home/ubuntu/.netrc",
            "/home/ubuntu/.claude/.credentials.json",
            "/home/ubuntu/.tk/run/tk/daemon.token",
            "/run/user/1000/tk/daemon.token",
            "/etc/shadow",
            "/etc/sudoers.d/90-x",
            "/srv/app/.env.local",
        ] {
            assert!(is_sensitive(p, H, Some("/run/user/1000")), "{p}");
        }
        for p in [
            "/home/ubuntu/proj/a.txt",
            "/home/ubuntu/.sshx",
            "/etc/hosts",
            "/home/ubuntu/.claude/projects/x.jsonl",
        ] {
            assert!(!is_sensitive(p, H, Some("/run/user/1000")), "{p}");
        }
    }

    #[test]
    fn dot_dot_is_never_walked_around() {
        assert!(is_sensitive("/home/ubuntu/proj/../.ssh/id_test", H, None));
    }

    #[test]
    fn files_that_run_later() {
        for p in [
            "/home/ubuntu/proj/.git/hooks/pre-commit",
            "/home/ubuntu/proj/.git/config",
            "/home/ubuntu/proj/.claude/settings.json",
            "/home/ubuntu/proj/.vscode/tasks.json",
            "/home/ubuntu/.bashrc",
            "/home/ubuntu/.config/systemd/user/x.service",
            "/home/ubuntu/.pi/agent/extensions/evil.ts",
        ] {
            assert!(is_trusted_file(p, H), "{p}");
        }
        for p in [
            "/home/ubuntu/proj/src/main.rs",
            "/home/ubuntu/proj/.gitignore",
            "/home/ubuntu/.bash_history",
        ] {
            assert!(!is_trusted_file(p, H), "{p}");
        }
    }
}

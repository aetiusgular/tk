//! Where `tk-daemon cu-mcp` finds the bridge and what it presents (lane K).
//!
//! The fork's relay read `MONOCODE_CU_ENDPOINT` and `MONOCODE_CU_TOKEN`,
//! which MonoCode set on the harness, so the token also reached the agent's
//! shell (lane A's wave 1 finding; `cu-shell-claude` fails on purpose). tk
//! gives a harness no computer-use secret: the relay presents the chat token
//! `tk run` already gave it, read by the same rules as the hooks'
//! (`packages/tk-claude-plugin/src/core/daemon.ts`, `chatToken` and
//! `socketPath`):
//!
//! - `TK_CHAT_TOKEN` (64 lowercase hex), else the file `TK_CHAT_TOKEN_FILE`
//!   names: absolute, a regular file (not a link), yours, no group or other
//!   bits, at most 256 bytes, outside the session folder, not named
//!   `daemon.token`. Never the daemon's own token: the bridge also refuses
//!   anything but a chat token (`service.rs`).
//! - The socket from `TK_DAEMON_SOCKET` (absolute, outside the session
//!   folder, which a project can set) or the contract's default. The bridge's
//!   `cu.endpoint` sits beside it, in a folder you own that no one else can
//!   write; the file is yours, private and loopback.
//!
//! What this does not change: the chat token sits in the harness's
//! environment and its tools' shells (rules, section 4, case 66). With it a
//! shell can reach the bridge too, past the harness's own gate, but only as
//! its chat and only for apps the user granted that chat.

use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

use super::ENDPOINT_FILE;

/// The longest token file and endpoint file read.
const MAX_FILE: u64 = 256;

/// The endpoint and token the relay dials with, or why there are none.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub endpoint: String,
    pub token: String,
}

/// `tk-daemon cu-mcp`: finds its target in this process's environment and
/// serves MCP on stdin and stdout until the harness closes stdin. Without a
/// target it still serves, as an empty server that says why (the fork's
/// rule: Claude Code does not restart a stdio server that exits, case 39).
pub fn run() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let env = |name: &str| std::env::var(name).ok();
    match target(&env, &cwd, uid()) {
        Ok(t) => super::bridge_cli::run_with(Some(&t.endpoint), Some(&t.token)),
        Err(why) => {
            eprintln!("tk-daemon cu-mcp: {why}");
            super::bridge_cli::run_with(None, None)
        }
    }
}

fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The target from the environment `env` gives, for a process whose session
/// folder is `cwd`, run by `uid`.
pub fn target(
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
    uid: u32,
) -> Result<Target, String> {
    let token = chat_token(env, cwd, uid).ok_or(
        "no chat token: start the harness with tk run (TK_CHAT_TOKEN or TK_CHAT_TOKEN_FILE)",
    )?;
    let socket = socket_path(env, cwd)?;
    let dir = socket.parent().ok_or("the daemon's socket has no folder")?;
    let endpoint = endpoint(dir, uid)?;
    Ok(Target { endpoint, token })
}

fn hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `TK_CHAT_TOKEN`, else the file `TK_CHAT_TOKEN_FILE` names, by the hooks'
/// rules (module docs).
pub fn chat_token(env: &dyn Fn(&str) -> Option<String>, cwd: &Path, uid: u32) -> Option<String> {
    if let Some(token) = env("TK_CHAT_TOKEN") {
        let token = token.trim();
        if hex64(token) {
            return Some(token.to_string());
        }
    }
    let file = env("TK_CHAT_TOKEN_FILE")?;
    let file = PathBuf::from(file.trim());
    if !file.is_absolute() || inside(&file, cwd) {
        return None;
    }
    if file.file_name().is_some_and(|n| n == "daemon.token") {
        return None;
    }
    let text = read_private(&file, uid)?;
    let text = text.trim();
    hex64(text).then(|| text.to_string())
}

/// The daemon's socket: `TK_DAEMON_SOCKET` when set (absolute, outside the
/// session folder), else the contract's default.
pub fn socket_path(env: &dyn Fn(&str) -> Option<String>, cwd: &Path) -> Result<PathBuf, String> {
    match env("TK_DAEMON_SOCKET").filter(|s| !s.trim().is_empty()) {
        Some(set) => {
            let path = PathBuf::from(set.trim());
            if !path.is_absolute() {
                return Err("TK_DAEMON_SOCKET is not absolute; not used".into());
            }
            if inside(&path, cwd) {
                return Err(
                    "TK_DAEMON_SOCKET is inside the session folder, which a project can set; not used"
                        .into(),
                );
            }
            Ok(path)
        }
        None => crate::socket_path().ok_or_else(|| "no HOME or XDG_RUNTIME_DIR".to_string()),
    }
}

/// The bridge's address from `<dir>/cu.endpoint`, when the folder is yours
/// and no one else can write it, and the file is yours and private.
pub fn endpoint(dir: &Path, uid: u32) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(dir).map_err(|_| "no daemon folder".to_string())?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err("the daemon's folder is not yours alone".into());
    }
    let file = dir.join(ENDPOINT_FILE);
    let text = read_private(&file, uid).ok_or(
        "no computer-use endpoint: the daemon's bridge runs on macOS with the computer-use feature",
    )?;
    let text = text.trim().to_string();
    super::bridge_cli::endpoint(Some(&text))?;
    Ok(text)
}

/// A small regular file you own with no group or other bits, read without
/// following a link.
fn read_private(path: &Path, uid: u32) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file()
        || meta.uid() != uid
        || meta.mode() & 0o077 != 0
        || meta.len() > MAX_FILE
    {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut text = String::new();
    use std::io::Read;
    file.take(MAX_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Whether `path` is `folder` or inside it, by components after `.` and
/// `..` are folded; and again with both resolved where they exist, so a link
/// into the folder counts as inside.
pub fn inside(path: &Path, folder: &Path) -> bool {
    let lexical = |p: &Path| -> PathBuf {
        let mut out = PathBuf::new();
        for part in p.components() {
            match part {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        out
    };
    if lexical(path).starts_with(lexical(folder)) {
        return true;
    }
    let real_folder = std::fs::canonicalize(folder).ok();
    // The file may not exist yet: resolve the nearest folder that does.
    let mut probe = path.to_path_buf();
    let real_path = loop {
        if let Ok(real) = std::fs::canonicalize(&probe) {
            break Some(real);
        }
        if !probe.pop() {
            break None;
        }
    };
    matches!((real_path, real_folder), (Some(p), Some(f)) if p.starts_with(&f))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("tk-relay-{name}-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            Scratch(dir)
        }
        fn file(&self, name: &str, text: &str, mode: u32) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn env_of(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn me() -> u32 {
        uid()
    }

    #[test]
    fn the_env_token_wins_and_must_be_a_chat_token_shape() {
        let cwd = Path::new("/nonexistent/project");
        let env = env_of(&[("TK_CHAT_TOKEN", TOKEN.to_string())]);
        assert_eq!(chat_token(&env, cwd, me()).as_deref(), Some(TOKEN));
        for bad in ["", "short", &TOKEN.to_uppercase(), &format!("{TOKEN}0")] {
            let env = env_of(&[("TK_CHAT_TOKEN", bad.to_string())]);
            assert_eq!(chat_token(&env, cwd, me()), None, "{bad:?}");
        }
    }

    #[test]
    fn the_token_file_must_be_absolute_private_yours_and_outside_the_session() {
        let home = Scratch::new("home");
        let project = Scratch::new("project");
        let good = home.file("c1.token", &format!("{TOKEN}\n"), 0o600);
        let env = env_of(&[("TK_CHAT_TOKEN_FILE", good.display().to_string())]);
        assert_eq!(chat_token(&env, &project.0, me()).as_deref(), Some(TOKEN));

        let loose = home.file("loose.token", TOKEN, 0o640);
        let named = home.file("daemon.token", TOKEN, 0o600);
        let planted = project.file("planted.token", TOKEN, 0o600);
        let link = home.0.join("link.token");
        std::os::unix::fs::symlink(&good, &link).unwrap();
        let big = home.file("big.token", &"a".repeat(300), 0o600);
        for (what, path) in [
            ("group-readable", loose.display().to_string()),
            ("named daemon.token", named.display().to_string()),
            ("inside the session folder", planted.display().to_string()),
            ("a link", link.display().to_string()),
            ("too big", big.display().to_string()),
            ("relative", "c1.token".to_string()),
        ] {
            let env = env_of(&[("TK_CHAT_TOKEN_FILE", path)]);
            assert_eq!(chat_token(&env, &project.0, me()), None, "{what}");
        }
        // Another user's file (simulated with a uid that is not ours).
        let env = env_of(&[("TK_CHAT_TOKEN_FILE", good.display().to_string())]);
        assert_eq!(chat_token(&env, &project.0, me().wrapping_add(1)), None);
    }

    #[test]
    fn a_socket_the_project_names_is_not_used() {
        let project = Scratch::new("sock-project");
        let inside_path = project.0.join("fake.sock").display().to_string();
        let env = env_of(&[("TK_DAEMON_SOCKET", inside_path)]);
        assert!(socket_path(&env, &project.0)
            .unwrap_err()
            .contains("inside the session folder"));
        let env = env_of(&[("TK_DAEMON_SOCKET", "rel/daemon.sock".to_string())]);
        assert!(socket_path(&env, &project.0).is_err());
        let env = env_of(&[("TK_DAEMON_SOCKET", "/run/user/1/tk/daemon.sock".into())]);
        assert_eq!(
            socket_path(&env, &project.0).unwrap(),
            PathBuf::from("/run/user/1/tk/daemon.sock")
        );
        // `..` back into the project still counts as inside it.
        let sneaky = format!("{}/x/../fake.sock", project.0.display());
        let env = env_of(&[("TK_DAEMON_SOCKET", sneaky)]);
        assert!(socket_path(&env, &project.0).is_err());
    }

    #[test]
    fn the_endpoint_file_must_be_private_in_a_private_folder_and_loopback() {
        let dir = Scratch::new("run");
        dir.file(ENDPOINT_FILE, "127.0.0.1:41234\n", 0o600);
        assert_eq!(endpoint(&dir.0, me()).unwrap(), "127.0.0.1:41234");

        dir.file(ENDPOINT_FILE, "10.0.0.5:41234\n", 0o600);
        assert!(endpoint(&dir.0, me()).is_err(), "not loopback");
        dir.file(ENDPOINT_FILE, "127.0.0.1:41234\n", 0o644);
        assert!(endpoint(&dir.0, me()).is_err(), "readable by others");
        dir.file(ENDPOINT_FILE, "127.0.0.1:41234\n", 0o600);
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(endpoint(&dir.0, me()).is_err(), "folder others can write");
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(endpoint(&dir.0, me().wrapping_add(1)).is_err(), "not yours");
        let missing = Scratch::new("empty");
        assert!(endpoint(&missing.0, me())
            .unwrap_err()
            .contains("no computer-use endpoint"));
    }

    #[test]
    fn the_full_target_reads_the_socket_folder_and_the_token() {
        let run = Scratch::new("full-run");
        let project = Scratch::new("full-project");
        run.file(ENDPOINT_FILE, "127.0.0.1:5000\n", 0o600);
        let token_file = run.file("chats-c1.token", TOKEN, 0o600);
        let env = env_of(&[
            (
                "TK_DAEMON_SOCKET",
                run.0.join("daemon.sock").display().to_string(),
            ),
            ("TK_CHAT_TOKEN_FILE", token_file.display().to_string()),
        ]);
        assert_eq!(
            target(&env, &project.0, me()).unwrap(),
            Target {
                endpoint: "127.0.0.1:5000".into(),
                token: TOKEN.into()
            }
        );
        // No token: says so, and names tk run.
        let env = env_of(&[(
            "TK_DAEMON_SOCKET",
            run.0.join("daemon.sock").display().to_string(),
        )]);
        assert!(target(&env, &project.0, me())
            .unwrap_err()
            .contains("tk run"));
    }
}

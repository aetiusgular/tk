//! `docker mcp` for the Connectors page: Docker's MCP catalog, MonoCode's own
//! connector profiles, their secrets and their OAuth sign-ins.
//!
//! Only the argument shapes in `allowed_timeout` run. Every free value is
//! checked against a tight pattern, profile changes are limited to MonoCode's
//! own `monocode_` profiles, and a secret's value only ever arrives on stdin.
//! Nothing here logs stdin.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::dirs_home;
use crate::harness::{exec_output_with_stdin, gui_search_path};

const CATALOG: &str = "mcp/docker-mcp-catalog";
const CATALOG_REF_PREFIX: &str = "catalog://mcp/docker-mcp-catalog/";
/// The user's own catalog of servers Docker's catalog lacks, built with
/// `docker mcp catalog create monocode/extra:latest --server file://...`.
const EXTRA_CATALOG: &str = "monocode/extra";
const EXTRA_CATALOG_REF_PREFIX: &str = "catalog://monocode/extra/";
const OWN_PROFILE_PREFIX: &str = "monocode_";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const CATALOG_TIMEOUT: Duration = Duration::from_secs(120);
const OAUTH_TIMEOUT: Duration = Duration::from_secs(300);
/// A dry run may pull a server's image first.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(180);
const MIN_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_SECRET_BYTES: usize = 16 * 1024;
const MAX_ERROR_CHARS: usize = 2000;
const MAX_PROFILE_SERVERS: usize = 32;
const MAX_STDERR_CHARS: usize = 64 * 1024;
const MAX_CONFIG_SETS: usize = 64;
const MAX_CONFIG_VALUE_CHARS: usize = 1000;
/// Tools one `profile tools` call may turn on; the gateway's catalog cap.
const MAX_ENABLE_TOOLS: usize = 200;
/// Config property names that would put a credential in plain profile
/// config. Docker's secret store is the place for those.
const SECRET_LIKE: [&str; 7] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "private_key",
];
const DOCKER_MISSING: &str = "Docker CLI not found. Install Docker Desktop.";

#[cfg(windows)]
const DOCKER_BIN: &str = "docker.exe";
#[cfg(not(windows))]
const DOCKER_BIN: &str = "docker";

#[derive(Serialize)]
pub struct DockerMcpOutput {
    stdout: String,
    /// Bounded. The gateway's dry run reports each server here.
    stderr: String,
    success: bool,
    /// The docker binary that ran, so sessions can start the gateway with it.
    docker: String,
}

/// Run one allowlisted `docker mcp` command. Returns its output on success,
/// and a bounded error carrying stderr otherwise. A gateway dry run returns
/// its output either way: a failing run is a test result, not an error.
#[tauri::command]
pub async fn docker_mcp(
    args: Vec<String>,
    stdin: Option<String>,
    timeout_ms: Option<u64>,
) -> Result<DockerMcpOutput, String> {
    let home = dirs_home().map(PathBuf::from);
    let limit = allowed_timeout(&args, home.as_deref())
        .ok_or_else(|| "docker_mcp: unsupported arguments".to_string())?;
    check_stdin(&args, stdin.as_deref())?;
    let timeout = timeout_ms
        .map(Duration::from_millis)
        .map_or(limit, |requested| requested.clamp(MIN_TIMEOUT, limit));
    tauri::async_runtime::spawn_blocking(move || run(&args, stdin.as_deref(), timeout))
        .await
        .map_err(|e| e.to_string())?
}

fn run(args: &[String], stdin: Option<&str>, timeout: Duration) -> Result<DockerMcpOutput, String> {
    let docker = resolve_docker().ok_or_else(|| DOCKER_MISSING.to_string())?;
    let command = docker.to_string_lossy().into_owned();
    let output = exec_output_with_stdin(&command, args, None, timeout, stdin.map(str::as_bytes))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let success = output.status.success();
    if success || is_dry_run(args) {
        return Ok(DockerMcpOutput {
            stdout,
            stderr: stderr.chars().take(MAX_STDERR_CHARS).collect(),
            success,
            docker: command,
        });
    }
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(bounded(message))
}

/// The installer's symlink, Homebrew, the app bundle, then the user's PATH.
fn resolve_docker() -> Option<PathBuf> {
    [
        "/usr/local/bin/docker",
        "/opt/homebrew/bin/docker",
        "/Applications/Docker.app/Contents/Resources/bin/docker",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
    .or_else(|| {
        std::env::split_paths(&gui_search_path())
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(DOCKER_BIN))
            .find(|path| path.is_file())
    })
}

fn bounded(message: &str) -> String {
    if message.is_empty() {
        return "docker mcp failed".into();
    }
    message.chars().take(MAX_ERROR_CHARS).collect()
}

fn is_dry_run(args: &[String]) -> bool {
    args.len() == 7 && args[1] == "gateway" && args[5] == "--dry-run"
}

fn is_secret_set(args: &[String]) -> bool {
    args.len() == 4 && args[0] == "mcp" && args[1] == "secret" && args[2] == "set"
}

/// A secret's value goes on stdin, and nothing else takes stdin.
fn check_stdin(args: &[String], stdin: Option<&str>) -> Result<(), String> {
    match (is_secret_set(args), stdin) {
        (true, Some(value)) if !value.is_empty() && value.len() <= MAX_SECRET_BYTES => Ok(()),
        (true, _) => Err("docker_mcp: secret set needs a value on stdin".into()),
        (false, None) => Ok(()),
        (false, Some(_)) => Err("docker_mcp: only secret set takes stdin".into()),
    }
}

/// The timeout cap for an allowed command, or None when it is not allowed.
fn allowed_timeout(args: &[String], home: Option<&Path>) -> Option<Duration> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["mcp", "version"] => Some(DEFAULT_TIMEOUT),
        ["mcp", "catalog", "show", catalog, "--format", "json"]
            if *catalog == CATALOG || *catalog == EXTRA_CATALOG =>
        {
            Some(CATALOG_TIMEOUT)
        }
        ["mcp", "catalog", "pull", catalog] if *catalog == CATALOG => Some(CATALOG_TIMEOUT),
        ["mcp", "profile", "list", "--format", "json"] => Some(DEFAULT_TIMEOUT),
        ["mcp", "profile", "show", id, "--format", "json"] if is_profile_id(id) => {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "create", "--name", name, "--id", id, servers @ ..]
            if is_server_name(name) && is_own_profile_id(id) && is_server_list(servers, home) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "remove", id] if is_own_profile_id(id) => Some(DEFAULT_TIMEOUT),
        ["mcp", "profile", "server", "add", id, "--server", server]
            if is_own_profile_id(id) && is_server_ref(server, home) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "server", "remove", id, name]
            if is_own_profile_id(id) && is_server_name(name) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "config", id, "--get-all", "--format", "json"]
            if is_own_profile_id(id) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "config", id, "--del", key]
            if is_own_profile_id(id) && is_config_key(key) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "config", id, sets @ ..]
            if is_own_profile_id(id) && is_config_sets(sets) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        // A connector's tool allowlist: one tool on or off, or all back on.
        ["mcp", "profile", "tools", id, "--enable" | "--disable", tool]
            if is_own_profile_id(id) && is_tool_ref(tool) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "profile", "tools", id, "--enable-all", server]
            if is_own_profile_id(id) && is_server_name(server) && !server.contains('.') =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        // Mac test 4.15: the first tool switched off on a server whose catalog
        // lists no tools. `--disable` would expand a null allowlist from the
        // empty snapshot and turn every tool off, so the allowlist is written
        // as every other tool, one server, in one call.
        ["mcp", "profile", "tools", id, rest @ ..]
            if is_own_profile_id(id) && is_enable_list(rest) =>
        {
            Some(DEFAULT_TIMEOUT)
        }
        // Mac test 4.15: the tool names of a connector whose catalog lists
        // none, from the gateway serving its profile.
        ["mcp", "tools", "ls", "--format", "json", profile_flag, id_flag]
            if *profile_flag == "--gateway-arg=--profile"
                && id_flag
                    .strip_prefix("--gateway-arg=")
                    .is_some_and(is_own_profile_id) =>
        {
            Some(DRY_RUN_TIMEOUT)
        }
        ["mcp", "gateway", "run", "--profile", id, "--dry-run", "--verbose"]
            if is_own_profile_id(id) =>
        {
            Some(DRY_RUN_TIMEOUT)
        }
        ["mcp", "secret", "ls", "--json"] => Some(DEFAULT_TIMEOUT),
        ["mcp", "secret", "set", name] if is_secret_name(name) && !name.contains('=') => {
            Some(DEFAULT_TIMEOUT)
        }
        ["mcp", "secret", "rm", name] if is_secret_name(name) => Some(DEFAULT_TIMEOUT),
        ["mcp", "oauth", "ls", "--json"] => Some(DEFAULT_TIMEOUT),
        ["mcp", "oauth", "authorize", app, "--open-browser"] if is_server_name(app) => {
            Some(OAUTH_TIMEOUT)
        }
        ["mcp", "oauth", "revoke", app] if is_server_name(app) => Some(DEFAULT_TIMEOUT),
        _ => None,
    }
}

/// `^[a-z0-9_]{1,64}$`
fn is_profile_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A profile MonoCode made. Only these can be created, changed or removed.
fn is_own_profile_id(value: &str) -> bool {
    is_profile_id(value)
        && value.len() > OWN_PROFILE_PREFIX.len()
        && value.starts_with(OWN_PROFILE_PREFIX)
}

/// `<server>.<tool>`: the gateway splits at the first dot, so the server
/// part has none. A tool name is `^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$`.
fn is_tool_ref(value: &str) -> bool {
    let Some((server, tool)) = value.split_once('.') else {
        return false;
    };
    let bytes = tool.as_bytes();
    is_server_name(server)
        && !server.contains('.')
        && (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Two to `MAX_ENABLE_TOOLS` pairs of `--enable <server>.<tool>`, all for one
/// server. One pair is the single-tool shape above.
fn is_enable_list(args: &[&str]) -> bool {
    if args.len() < 4 || !args.len().is_multiple_of(2) || args.len() > MAX_ENABLE_TOOLS * 2 {
        return false;
    }
    let server = args[1].split_once('.').map(|(server, _)| server);
    args.chunks(2).all(|pair| {
        pair[0] == "--enable"
            && is_tool_ref(pair[1])
            && pair[1].split_once('.').map(|(s, _)| s) == server
    })
}

/// `^[a-z0-9][a-z0-9._-]{0,99}$`
fn is_server_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=100).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// `^[A-Za-z0-9._-]{1,128}$`
fn is_secret_name(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `^[a-z0-9][a-z0-9._-]{0,99}\.[A-Za-z0-9_]{1,64}$`: `<server>.<property>`,
/// where the property does not look like a credential.
fn is_config_key(value: &str) -> bool {
    let Some((server, property)) = value.rsplit_once('.') else {
        return false;
    };
    let lower = property.to_ascii_lowercase();
    is_server_name(server)
        && (1..=64).contains(&property.len())
        && property
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !SECRET_LIKE.iter().any(|word| lower.contains(word))
}

/// One or more `--set <key>=<value>` pairs. A value is one line of at most
/// 1000 characters.
fn is_config_sets(args: &[&str]) -> bool {
    !args.is_empty()
        && args.len().is_multiple_of(2)
        && args.len() <= MAX_CONFIG_SETS * 2
        && args.chunks(2).all(|pair| {
            pair[0] == "--set"
                && pair[1].split_once('=').is_some_and(|(key, value)| {
                    is_config_key(key)
                        && value.chars().count() <= MAX_CONFIG_VALUE_CHARS
                        && !value.contains(['\n', '\r', '\0'])
                })
        })
}

/// One or more `--server <ref>` pairs.
fn is_server_list(args: &[&str], home: Option<&Path>) -> bool {
    !args.is_empty()
        && args.len().is_multiple_of(2)
        && args.len() <= MAX_PROFILE_SERVERS * 2
        && args
            .chunks(2)
            .all(|pair| pair[0] == "--server" && is_server_ref(pair[1], home))
}

/// A server in Docker's catalog, or a server file under the user's
/// `~/.docker/mcp/catalogs/`.
fn is_server_ref(value: &str, home: Option<&Path>) -> bool {
    if let Some(name) = value
        .strip_prefix(CATALOG_REF_PREFIX)
        .or_else(|| value.strip_prefix(EXTRA_CATALOG_REF_PREFIX))
    {
        return is_server_name(name);
    }
    match (value.strip_prefix("file://"), home) {
        (Some(path), Some(home)) => is_catalog_file(path, home),
        _ => false,
    }
}

fn is_catalog_file(path: &str, home: &Path) -> bool {
    if path.is_empty() || path.len() > 1024 || path.chars().any(char::is_control) {
        return false;
    }
    let path = Path::new(path);
    if !path.is_absolute()
        || path.components().any(|part| {
            !matches!(
                part,
                Component::RootDir | Component::Prefix(_) | Component::Normal(_)
            )
        })
    {
        return false;
    }
    let root = home.join(".docker").join("mcp").join("catalogs");
    path.starts_with(&root) && path != root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|value| value.to_string()).collect()
    }

    fn allowed(list: &[&str]) -> bool {
        allowed_timeout(&args(list), Some(Path::new("/Users/me"))).is_some()
    }

    #[test]
    fn allows_the_listed_shapes() {
        assert!(allowed(&["mcp", "version"]));
        assert!(allowed(&[
            "mcp",
            "catalog",
            "show",
            "mcp/docker-mcp-catalog",
            "--format",
            "json"
        ]));
        assert!(allowed(&[
            "mcp",
            "catalog",
            "pull",
            "mcp/docker-mcp-catalog"
        ]));
        assert!(allowed(&["mcp", "profile", "list", "--format", "json"]));
        assert!(allowed(&[
            "mcp", "profile", "show", "research", "--format", "json"
        ]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "create",
            "--name",
            "monocode-notion-remote",
            "--id",
            "monocode_notion_remote",
            "--server",
            "catalog://mcp/docker-mcp-catalog/notion-remote",
        ]));
        assert!(allowed(&["mcp", "profile", "remove", "monocode_github"]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "server",
            "add",
            "monocode_github",
            "--server",
            "file:///Users/me/.docker/mcp/catalogs/exa.yaml",
        ]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "server",
            "remove",
            "monocode_github",
            "github"
        ]));
        assert!(allowed(&["mcp", "secret", "ls", "--json"]));
        assert!(allowed(&[
            "mcp",
            "secret",
            "set",
            "github.personal_access_token"
        ]));
        assert!(allowed(&[
            "mcp",
            "secret",
            "rm",
            "github.personal_access_token"
        ]));
        assert!(allowed(&["mcp", "oauth", "ls", "--json"]));
        assert!(allowed(&[
            "mcp",
            "oauth",
            "authorize",
            "notion-remote",
            "--open-browser"
        ]));
        assert!(allowed(&["mcp", "oauth", "revoke", "notion-remote"]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "config",
            "monocode_gmail_mcp",
            "--set",
            "gmail-mcp.email_address=test@example.com",
            "--set",
            "gmail-mcp.label=a=b c",
        ]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "config",
            "monocode_gmail_mcp",
            "--get-all",
            "--format",
            "json",
        ]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "config",
            "monocode_gmail_mcp",
            "--del",
            "gmail-mcp.email_address",
        ]));
        assert!(allowed(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "monocode_hugging_face",
            "--dry-run",
            "--verbose",
        ]));
    }

    #[test]
    fn refuses_bad_config_and_test_calls() {
        let config = |sets: &[&str]| {
            let mut list = vec!["mcp", "profile", "config", "monocode_gmail_mcp"];
            list.extend_from_slice(sets);
            allowed(&list)
        };
        assert!(!config(&[]));
        assert!(!config(&["--set"]));
        assert!(!config(&["--set", "email_address=x"]));
        assert!(!config(&["--set", "gmail-mcp.email_address"]));
        assert!(!config(&["--set", "Gmail.email=x"]));
        assert!(!config(&["--set", "gmail-mcp.email-address=x"]));
        assert!(!config(&["--set", "gmail-mcp.email_address=a\nb"]));
        assert!(!config(&["--set", "gmail-mcp.email_address=a\0b"]));
        let long = format!("gmail-mcp.email_address={}", "x".repeat(1001));
        assert!(!config(&["--set", &long]));
        let fits = format!("gmail-mcp.email_address={}", "x".repeat(1000));
        assert!(config(&["--set", &fits]));
        assert!(!config(&["--set", "github.personal_access_token=ghp"]));
        assert!(!config(&["--set", "x.API_KEY=1"]));
        assert!(!config(&["--set", "x.password=1"]));
        assert!(!config(&["--get", "gmail-mcp.email_address"]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "config",
            "research",
            "--set",
            "exa.region=us",
        ]));
        assert!(!allowed(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "research",
            "--dry-run",
            "--verbose",
        ]));
        assert!(!allowed(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "monocode_x",
            "--verbose",
        ]));
        assert!(!allowed(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "monocode_x",
            "--dry-run",
            "--verbose",
            "--port",
            "1",
        ]));
    }

    #[test]
    fn refuses_everything_else() {
        assert!(!allowed(&[]));
        assert!(!allowed(&["ps"]));
        assert!(!allowed(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "monocode_github"
        ]));
        assert!(!allowed(&[
            "mcp",
            "catalog",
            "show",
            "other/catalog",
            "--format",
            "json"
        ]));
        assert!(allowed(&[
            "mcp",
            "catalog",
            "show",
            "monocode/extra",
            "--format",
            "json"
        ]));
        assert!(allowed(&[
            "mcp",
            "profile",
            "create",
            "--name",
            "monocode-vercel-remote",
            "--id",
            "monocode_vercel_remote",
            "--server",
            "catalog://monocode/extra/vercel-remote"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "create",
            "--name",
            "x",
            "--id",
            "monocode_x",
            "--server",
            "catalog://other/catalog/x"
        ]));
        assert!(!allowed(&["mcp", "catalog", "pull", "monocode/extra"]));
        assert!(!allowed(&["mcp", "profile", "remove", "research"]));
        assert!(!allowed(&["mcp", "profile", "remove", "monocode_"]));
        assert!(!allowed(&[
            "mcp", "profile", "show", "Research", "--format", "json"
        ]));
        assert!(!allowed(&["mcp", "secret", "set", "token=abc"]));
        assert!(!allowed(&["mcp", "secret", "set", "token", "value"]));
        assert!(!allowed(&[
            "mcp",
            "oauth",
            "authorize",
            "-x",
            "--open-browser"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "create",
            "--name",
            "x",
            "--id",
            "monocode_x",
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "create",
            "--name",
            "x",
            "--id",
            "monocode_x",
            "--server",
            "docker.io/mcp/github",
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "server",
            "add",
            "monocode_x",
            "--server",
            "file:///Users/me/.docker/mcp/catalogs/../../.ssh/id_rsa",
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "server",
            "add",
            "monocode_x",
            "--server",
            "file:///etc/passwd",
        ]));
    }

    #[test]
    fn caps_timeouts_by_command() {
        let home = Some(Path::new("/Users/me"));
        assert_eq!(
            allowed_timeout(
                &args(&["mcp", "oauth", "authorize", "linear", "--open-browser"]),
                home
            ),
            Some(OAUTH_TIMEOUT)
        );
        assert_eq!(
            allowed_timeout(
                &args(&["mcp", "catalog", "pull", "mcp/docker-mcp-catalog"]),
                home
            ),
            Some(CATALOG_TIMEOUT)
        );
        assert_eq!(
            allowed_timeout(&args(&["mcp", "version"]), home),
            Some(DEFAULT_TIMEOUT)
        );
        let dry_run = args(&[
            "mcp",
            "gateway",
            "run",
            "--profile",
            "monocode_x",
            "--dry-run",
            "--verbose",
        ]);
        assert_eq!(allowed_timeout(&dry_run, home), Some(DRY_RUN_TIMEOUT));
        assert!(is_dry_run(&dry_run));
        assert!(!is_dry_run(&args(&["mcp", "version"])));
    }

    #[test]
    fn only_secret_set_takes_stdin() {
        let set = args(&["mcp", "secret", "set", "exa.api_key"]);
        assert!(check_stdin(&set, Some("value")).is_ok());
        assert!(check_stdin(&set, None).is_err());
        assert!(check_stdin(&set, Some("")).is_err());
        assert!(check_stdin(&set, Some(&"x".repeat(MAX_SECRET_BYTES + 1))).is_err());
        let list = args(&["mcp", "secret", "ls", "--json"]);
        assert!(check_stdin(&list, None).is_ok());
        assert!(check_stdin(&list, Some("value")).is_err());
    }

    #[test]
    fn bounds_error_text() {
        assert_eq!(bounded(""), "docker mcp failed");
        assert_eq!(
            bounded(&"é".repeat(MAX_ERROR_CHARS + 10)).chars().count(),
            MAX_ERROR_CHARS
        );
    }

    #[test]
    fn allows_one_tool_on_or_off_in_own_profiles() {
        for flag in ["--enable", "--disable"] {
            assert!(allowed(&[
                "mcp",
                "profile",
                "tools",
                "monocode_github",
                flag,
                "github.create_issue"
            ]));
            assert!(!allowed(&[
                "mcp",
                "profile",
                "tools",
                "research",
                flag,
                "github.create_issue"
            ]));
            for bad in [
                "github",
                ".create_issue",
                "github.",
                "github..create_issue",
                "github.-x",
                "github.a b",
                "github.a;b",
                "--all",
                "Github.create_issue",
            ] {
                assert!(
                    !allowed(&["mcp", "profile", "tools", "monocode_github", flag, bad]),
                    "{bad}"
                );
            }
        }
        assert!(allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--disable",
            "github.v2.list-repos_x"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--disable",
            &format!("github.{}", "a".repeat(129))
        ]));
    }

    #[test]
    fn allows_an_enable_list_for_one_server_only() {
        let list = |refs: &[&str]| {
            let mut out = vec!["mcp", "profile", "tools", "monocode_notion_remote"];
            for r in refs {
                out.push("--enable");
                out.push(r);
            }
            allowed(&out)
        };
        assert!(list(&[
            "notion-remote.notion-search",
            "notion-remote.notion-fetch"
        ]));
        // Another server, a disable or a bad name anywhere in the list fails.
        assert!(!list(&[
            "notion-remote.notion-search",
            "github.create_issue"
        ]));
        assert!(!list(&["notion-remote.notion-search", "notion-remote.a b"]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_notion_remote",
            "--enable",
            "notion-remote.a",
            "--disable",
            "notion-remote.b"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "research",
            "--enable",
            "notion-remote.a",
            "--enable",
            "notion-remote.b"
        ]));
        let many: Vec<String> = (0..=MAX_ENABLE_TOOLS)
            .map(|i| format!("notion-remote.t{i}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(!list(&refs));
        assert!(list(&refs[..MAX_ENABLE_TOOLS]));
    }

    #[test]
    fn allows_the_tool_list_of_own_profiles_only() {
        assert!(allowed(&[
            "mcp",
            "tools",
            "ls",
            "--format",
            "json",
            "--gateway-arg=--profile",
            "--gateway-arg=monocode_notion_remote"
        ]));
        for (flag, id) in [
            ("--gateway-arg=--profile", "--gateway-arg=research"),
            ("--gateway-arg=--servers", "--gateway-arg=monocode_x"),
            (
                "--gateway-arg=--profile",
                "--gateway-arg=monocode_x,--verbose",
            ),
            ("--gateway-arg=--profile", "monocode_x"),
        ] {
            assert!(
                !allowed(&["mcp", "tools", "ls", "--format", "json", flag, id]),
                "{flag} {id}"
            );
        }
        assert!(!allowed(&["mcp", "tools", "call", "notion-search"]));
    }

    #[test]
    fn allows_all_tools_back_on_but_never_all_off() {
        assert!(allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--enable-all",
            "github"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--disable-all",
            "github"
        ]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--enable-all",
            "git.hub"
        ]));
        assert!(!allowed(&["mcp", "profile", "tools", "monocode_github"]));
        assert!(!allowed(&[
            "mcp",
            "profile",
            "tools",
            "monocode_github",
            "--disable",
            "github.a",
            "--disable",
            "github.b"
        ]));
    }
}

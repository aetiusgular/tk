//! How the helper starts Cua Driver for one runtime.
//!
//! One child per runtime: `cua-driver mcp --direct --embedded`, which runs
//! Cua's runtime in that process and talks MCP on its stdio. No socket, so no
//! other process of this user can connect and drive the granted apps (Mac
//! test finding 4.14). Verified in Cua 0.37.0:
//! - `--direct` runs the runtime in the MCP process; with `--socket` it is
//!   refused (`cua-driver/src/main.rs:430-450`).
//! - `mcp` refuses every authorization flag (`--permission-mode`,
//!   `--capability-manifest`, `--approve-capability-manifest`, ...;
//!   `cua-driver/src/cli.rs:268-287`, `:798-805`), so the mode and the
//!   manifest go in through the environment only
//!   (`cua-driver-core/src/authorization.rs:1306-1331`).
//! - The manifest is read once, at startup, before stdin is read
//!   (`cua-driver/src/proxy.rs:41-47`, `session_manifest.rs:693-698`).
//! - On stdin EOF it ends this transport's Cua sessions and shuts down
//!   (`proxy.rs:66-84`, `:174`).

use std::path::{Path, PathBuf};

/// The helper's own bundle id. A debug helper has its own, so its grants are
/// never the release helper's (the user's decision 1). MonoCode's manifest
/// never names either.
pub const HELPER_BUNDLE_ID: &str = if cfg!(debug_assertions) {
    "com.monocode.computer-use-helper.dev"
} else {
    "com.monocode.computer-use-helper"
};

/// One process to start: program, arguments, and the environment to set on
/// top of a cleared one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// The environment every driver process gets. The helper clears its own
/// environment before adding these, so nothing from the caller leaks in.
pub fn base_env(home: &Path) -> Vec<(String, String)> {
    vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        // C6: Cua's own calls off for this process, whatever its saved config.
        ("CUA_DRIVER_RS_TELEMETRY_ENABLED".into(), "false".into()),
        ("CUA_DRIVER_RS_UPDATE_CHECK".into(), "false".into()),
        ("DO_NOT_TRACK".into(), "1".into()),
        ("CUA_TELEMETRY".into(), "0".into()),
        ("CUA_DRIVER_DISABLE_RUN_SCRIPT".into(), "1".into()),
    ]
}

/// The one Cua process of a runtime, bounded by the manifest at `manifest`.
pub fn direct_launch(driver: &Path, manifest: &Path, home: &Path) -> Launch {
    let mut env = base_env(home);
    env.extend([
        ("CUA_DRIVER_EMBEDDED".into(), "1".into()),
        ("CUA_DRIVER_HOST_BUNDLE_ID".into(), HELPER_BUNDLE_ID.into()),
        ("CUA_DRIVER_PERMISSION_MODE".into(), "bounded".into()),
        (
            "CUA_DRIVER_CAPABILITY_MANIFEST_FILE".into(),
            manifest.to_string_lossy().into_owned(),
        ),
        ("CUA_DRIVER_CAPABILITY_MANIFEST_APPROVED".into(), "1".into()),
    ]);
    Launch {
        program: driver.to_path_buf(),
        args: vec!["mcp".into(), "--direct".into(), "--embedded".into()],
        env,
    }
}

/// A runtime id MonoCode sent, if it is safe to put in a file name.
pub fn runtime_id(raw: &str) -> Option<&str> {
    let ok = !raw.is_empty()
        && raw.len() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_direct_child_bounded_by_the_manifest_with_no_socket() {
        let launch = direct_launch(
            Path::new("/H.app/Contents/MacOS/cua-driver"),
            Path::new("/t/m-1/manifest.yaml"),
            Path::new("/Users/t"),
        );
        assert_eq!(launch.args, ["mcp", "--direct", "--embedded"]);
        // Cua's `mcp` exits on any of these (cli.rs:268-276).
        for flag in [
            "--socket",
            "--permission-mode",
            "--capability-manifest",
            "--approve-capability-manifest",
            "--session-policy",
            "--approve-session-policy",
            "--dangerously-bypass-approvals",
            "--no-permissions-gate",
        ] {
            assert!(!launch.args.iter().any(|a| a.starts_with(flag)), "{flag}");
        }
        let env = |key: &str| {
            launch
                .env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(env("CUA_DRIVER_EMBEDDED"), Some("1"));
        assert_eq!(env("CUA_DRIVER_HOST_BUNDLE_ID"), Some(HELPER_BUNDLE_ID));
        assert_eq!(env("CUA_DRIVER_PERMISSION_MODE"), Some("bounded"));
        assert_eq!(
            env("CUA_DRIVER_CAPABILITY_MANIFEST_FILE"),
            Some("/t/m-1/manifest.yaml")
        );
        assert_eq!(env("CUA_DRIVER_CAPABILITY_MANIFEST_APPROVED"), Some("1"));
        assert_eq!(env("CUA_DRIVER_RS_TELEMETRY_ENABLED"), Some("false"));
        assert_eq!(env("CUA_DRIVER_RS_UPDATE_CHECK"), Some("false"));
        assert_eq!(env("CUA_DRIVER_DISABLE_RUN_SCRIPT"), Some("1"));
        assert!(env("CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS").is_none());
    }

    #[test]
    fn a_debug_helper_has_its_own_bundle_id() {
        assert_eq!(
            HELPER_BUNDLE_ID,
            if cfg!(debug_assertions) {
                "com.monocode.computer-use-helper.dev"
            } else {
                "com.monocode.computer-use-helper"
            }
        );
    }

    #[test]
    fn runtime_ids_are_file_safe() {
        assert_eq!(runtime_id("rt-1_a"), Some("rt-1_a"));
        for bad in ["", "../x", "a/b", "a b", &"x".repeat(65)] {
            assert_eq!(runtime_id(bad), None, "{bad}");
        }
    }
}

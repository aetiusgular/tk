//! `tk-daemon` (lane B, wave 1): the OTLP receiver, the activity log and
//! the Unix socket server from `contracts/daemon-protocol.md`.
//!
//! The three module folders are copies from the fork (`NOTICE` at the
//! repository root names each source commit and path). They are behind
//! feature flags because `otel_receiver/mod.rs`, `activity/mod.rs`,
//! `activity/commands.rs` and `computer_use/mod.rs` still reference
//! `tauri::`. Lane B's first item replaces those references with the socket
//! and the channel, then removes the `cfg` lines below. Nothing else in the
//! copied files was changed except the `#[path]` lines that pointed at the
//! old sibling layout.

#[cfg(feature = "fork-modules")]
pub mod activity;
#[cfg(feature = "computer-use")]
pub mod computer_use;
#[cfg(feature = "fork-modules")]
pub mod otel_receiver;

pub mod protocol;
pub mod socket;

/// Where the daemon's socket lives, per OS (contract, section 1). The
/// function only computes the path; it creates nothing.
pub fn socket_path() -> Option<std::path::PathBuf> {
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        Some(std::path::PathBuf::from(home).join("Library/Application Support/tk/daemon.sock"))
    } else {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".tk/run"))
            })?;
        Some(dir.join("tk/daemon.sock"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn socket_path_ends_with_tk_daemon_sock() {
        let p = super::socket_path().expect("HOME or XDG_RUNTIME_DIR is set in tests");
        assert!(p.ends_with("tk/daemon.sock"), "{}", p.display());
    }
}

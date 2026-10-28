//! `tk-daemon` (lane B, wave 1): the OTLP receiver, the activity log and
//! the Unix socket server from `contracts/daemon-protocol.md`.
//!
//! The three module folders are copies from the fork (`NOTICE` at the
//! repository root names each source commit and path). Lane B took
//! `activity/` and `otel_receiver/` out of Tauri (each file's header says
//! what changed); `computer_use/` still references `tauri::` and stays
//! behind the `computer-use` feature until lane K wires it in wave 2.
//!
//! - `daemon`: the state and the protocol's ops.
//! - `socket`: the Unix socket server.
//! - `service`: the pid lock, the token and endpoint files, start-up.
//! - `protocol`: the contract's Rust types.

pub mod activity;
pub mod collector;
#[cfg(feature = "computer-use")]
pub mod computer_use;
pub mod otel_receiver;

pub mod daemon;
pub mod protocol;
pub mod service;
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

//! Starting the daemon as a process: the pid file and its lock, the
//! daemon's token file, the activity log, the receiver's endpoint file, the
//! socket, and the hourly retention pass. `main.rs` calls `start` and then
//! serves; the tests call it with temporary paths.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::{Daemon, Paths};
use crate::socket;

/// How often the daemon runs retention itself.
pub const RETENTION_EVERY: Duration = Duration::from_secs(3_600);

/// A started daemon. The pid file's lock lives as long as `lock`: a second
/// daemon cannot start while this one runs, and the lock goes with the
/// process however it ends.
pub struct Running {
    pub daemon: Arc<Daemon>,
    pub listener: UnixListener,
    pub paths: Paths,
    lock: File,
}

impl Running {
    pub fn pid_file_fd(&self) -> i32 {
        self.lock.as_raw_fd()
    }
}

/// Writes `text` to `path` through a new file beside it, mode 0600, never
/// following a link, then renames it into place.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

/// Opens and locks the pid file, and writes this pid into it. Refused when
/// another process holds the lock: the daemon already runs (contract,
/// section 1). The check is the lock, not the pid: a pid can be reused, a
/// lock dies with its process.
pub fn lock_pid_file(path: &Path) -> std::io::Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // SAFETY: flock on a descriptor this function owns; no memory involved.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked != 0 {
        let held = std::fs::read_to_string(path).unwrap_or_default();
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("tk-daemon already runs (pid {})", held.trim()),
        ));
    }
    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(file)
}

/// Starts everything but the accept loop. Order: the private folders, the
/// lock (so a second daemon stops before it touches any file), the token,
/// the log, the receiver, the endpoint file, then the socket last, so a
/// client that can connect finds the token and the endpoint already there.
pub fn start(paths: Paths, log_prompts: bool) -> Result<Running, String> {
    let fail = |what: &str, e: std::io::Error| format!("{what}: {e}");
    socket::private_dir(paths.runtime_dir()).map_err(|e| fail("socket folder", e))?;
    socket::private_dir(&paths.data).map_err(|e| fail("data folder", e))?;
    let lock = lock_pid_file(&paths.pid_file()).map_err(|e| fail("pid file", e))?;
    let token = tk_core::token::mint().map_err(|e| fail("token", e))?;
    write_private(&paths.token_file(), &format!("{token}\n")).map_err(|e| fail("token file", e))?;
    crate::activity::init(&paths.data, None);
    let log = crate::activity::log().ok_or("the activity log did not start")?;
    if log.dir() != paths.log_dir() {
        return Err("the activity log already runs in another folder".into());
    }
    let daemon = Daemon::new(&token, log, &paths.data, log_prompts)?;
    write_private(&paths.endpoint_file(), &format!("{}\n", daemon.endpoint()))
        .map_err(|e| fail("endpoint file", e))?;
    let listener = socket::bind(&paths.socket).map_err(|e| fail("socket", e))?;
    let retention = daemon.clone();
    let _ = std::thread::Builder::new()
        .name("tk-daemon-retention".into())
        .spawn(move || loop {
            retention.retention_pass(crate::activity::store::now_ms());
            std::thread::sleep(RETENTION_EVERY);
        });
    Ok(Running {
        daemon,
        listener,
        paths,
        lock,
    })
}

/// The default paths: the contract's socket and `daemon::default_data_dir`.
pub fn default_paths() -> Option<Paths> {
    Some(Paths {
        socket: crate::socket_path()?,
        data: crate::daemon::default_data_dir()?,
    })
}

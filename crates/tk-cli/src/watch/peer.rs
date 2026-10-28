//! Who is on the other end of a Unix socket, and who our ancestors are.
//!
//! Two uses here: `tk status` names the process that serves the daemon's
//! socket (its pid, its program, and whether it runs as you), and
//! `tk watch --approve` refuses to run under a `tk` process (a chat's shell).
//! The same calls are what the daemon needs for the approver check that
//! integration w2 owns (security finding S2; the lane J status doc has the
//! request and its sources).
//!
//! Linux: `SO_PEERCRED` gives the peer's pid, uid and gid as they were at
//! `connect(2)` (unix(7)); ancestry is read from `/proc/<pid>/stat`
//! (proc(5)). macOS: `LOCAL_PEERCRED` gives the uid and `LOCAL_PEERPID` the
//! pid (XNU `bsd/sys/un.h`); ancestry from `proc_pidinfo(PROC_PIDTBSDINFO)`
//! and the program from `proc_pidpath`. A pid can be reused once its process
//! exits, so a pid read here names a process only while it runs.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: Option<u32>,
    pub uid: u32,
}

/// The peer's credentials, or `None` where the OS call failed.
pub fn of(stream: &UnixStream) -> Option<Peer> {
    os::peer(stream.as_raw_fd())
}

/// This process's effective uid.
pub fn me() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// The program a process runs, when the OS says.
pub fn program(pid: u32) -> Option<PathBuf> {
    os::program(pid)
}

/// A process's parent, when it still runs.
pub fn parent(pid: u32) -> Option<u32> {
    os::parent(pid)
}

/// Ancestors of `pid`, nearest first, without `pid` itself, up to 256.
pub fn ancestors(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < 256 {
        match parent(cur) {
            Some(p) if p != 0 && p != cur && !out.contains(&p) => {
                out.push(p);
                if p == 1 {
                    break;
                }
                cur = p;
            }
            _ => break,
        }
    }
    out
}

/// The nearest ancestor of `pid` whose program is named `tk`. Only `tk run`
/// starts other programs that last (a harness and its shells), so such an
/// ancestor means `pid` runs inside a chat.
pub fn tk_run_ancestor(pid: u32) -> Option<u32> {
    ancestors(pid).into_iter().find(|p| {
        program(*p)
            .and_then(|path| {
                path.file_name()
                    .map(|n| n.to_string_lossy().trim_end_matches(" (deleted)") == "tk")
            })
            .unwrap_or(false)
    })
}

#[cfg(target_os = "linux")]
mod os {
    use super::Peer;
    use std::path::PathBuf;

    pub fn peer(fd: i32) -> Option<Peer> {
        // SAFETY: an all-zero ucred is valid; getsockopt writes at most `len`
        // bytes into it and sets `len`.
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        (r == 0 && len as usize == std::mem::size_of::<libc::ucred>()).then(|| Peer {
            pid: u32::try_from(cred.pid).ok().filter(|p| *p > 0),
            uid: cred.uid,
        })
    }

    pub fn program(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }

    /// Field 4 of `/proc/<pid>/stat`. The second field is the program name
    /// in parentheses and may itself hold spaces and parentheses, so the
    /// fields are read after the last `)` (proc(5)).
    pub fn parent(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_ppid(&stat)
    }

    pub fn parse_ppid(stat: &str) -> Option<u32> {
        let rest = &stat[stat.rfind(')')? + 1..];
        let mut fields = rest.split_ascii_whitespace();
        let _state = fields.next()?;
        fields.next()?.parse().ok()
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::Peer;
    use std::path::PathBuf;

    // XNU bsd/sys/un.h: `#define SOL_LOCAL 0`, `#define LOCAL_PEERCRED
    // 0x001`, `#define LOCAL_PEERPID 0x002`. Written here so the build does
    // not depend on which of them the libc crate exports for Apple targets.
    const SOL_LOCAL: libc::c_int = 0;
    const LOCAL_PEERCRED: libc::c_int = 0x001;
    const LOCAL_PEERPID: libc::c_int = 0x002;

    pub fn peer(fd: i32) -> Option<Peer> {
        // SAFETY: zeroed xucred and pid_t are valid; getsockopt writes at
        // most `len` bytes into each.
        let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(
                fd,
                SOL_LOCAL,
                LOCAL_PEERCRED,
                (&mut cred as *mut libc::xucred).cast(),
                &mut len,
            )
        };
        if r != 0 {
            return None;
        }
        let mut pid: libc::pid_t = 0;
        let mut plen = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let rp = unsafe {
            libc::getsockopt(
                fd,
                SOL_LOCAL,
                LOCAL_PEERPID,
                (&mut pid as *mut libc::pid_t).cast(),
                &mut plen,
            )
        };
        Some(Peer {
            pid: (rp == 0).then(|| u32::try_from(pid).ok()).flatten().filter(|p| *p > 0),
            uid: cred.cr_uid,
        })
    }

    pub fn program(pid: u32) -> Option<PathBuf> {
        let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is PROC_PIDPATHINFO_MAXSIZE bytes, as
        // proc_pidpath requires.
        let n = unsafe {
            libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32)
        };
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
    }

    pub fn parent(pid: u32) -> Option<u32> {
        // SAFETY: a zeroed proc_bsdinfo is valid; proc_pidinfo writes at most
        // `size` bytes.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        (n == size).then_some(info.pbi_ppid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peer_of_a_socket_pair_is_this_process() {
        let (a, _b) = UnixStream::pair().unwrap();
        let p = of(&a).expect("peer credentials");
        assert_eq!(p.uid, me());
        assert_eq!(p.pid, Some(std::process::id()));
    }

    #[test]
    fn this_process_has_a_parent_and_a_program() {
        let me = std::process::id();
        assert!(parent(me).is_some_and(|p| p > 0));
        assert!(program(me).is_some());
        let up = ancestors(me);
        assert!(!up.is_empty() && !up.contains(&me), "{up:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_under_a_program_named_tk_is_seen_inside_a_chat() {
        // A copy of `sleep` named `tk` stands in for `tk run`; its child
        // asks for its own tk ancestor through a shell that prints its pid.
        let dir = std::env::temp_dir().join(format!("tk-peer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = which("sh");
        let fake_tk = dir.join("tk");
        std::fs::copy(&sh, &fake_tk).unwrap();
        // `tk -c 'sleep 5 & wait'`: the copy of sh, named tk, is the parent
        // of a sleep.
        let mut child = std::process::Command::new(&fake_tk)
            .args(["-c", "sleep 5 & wait"])
            .spawn()
            .unwrap();
        let tk_pid = child.id();
        let mut sleep_pid = None;
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(40));
            sleep_pid = children(tk_pid).into_iter().next();
            if sleep_pid.is_some() {
                break;
            }
        }
        let found = sleep_pid.and_then(tk_run_ancestor);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Some(tk_pid));
        // And this test process itself is not under a `tk`.
        assert_eq!(tk_run_ancestor(std::process::id()), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_with_spaces_and_parens_in_the_name_parses() {
        assert_eq!(
            os::parse_ppid("4242 (a) b (c)) S 77 4242 4242 0 -1"),
            Some(77)
        );
        assert_eq!(os::parse_ppid("garbage"), None);
    }

    #[cfg(target_os = "linux")]
    fn which(name: &str) -> PathBuf {
        for dir in ["/bin", "/usr/bin"] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return p;
            }
        }
        panic!("no {name}");
    }

    #[cfg(target_os = "linux")]
    fn children(pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                if let Ok(p) = e.file_name().to_string_lossy().parse::<u32>() {
                    if parent(p) == Some(pid) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

//! What `tk run --boundary srt` checks before it starts srt (box test w1,
//! findings BX3 and BX4). On the box srt failed twice with its own errors:
//! first "ripgrep (rg) not found", then, with rg on PATH, its seccomp helper
//! could not write `/proc/self/setgroups` because Ubuntu 26.04 sets
//! `kernel.apparmor_restrict_unprivileged_userns = 1`.
//!
//! Sources, srt 0.0.81 README (the installed package, read 27 Oct):
//! lines 686 to 698 name bubblewrap, socat and ripgrep as the Linux
//! dependencies and ripgrep alone on macOS (732); line 701 is the Ubuntu
//! 24.04+ note: the sysctl "strips capabilities from the resulting
//! namespace", and the fix is the sysctl at 0 or an AppArmor profile that
//! grants `userns` to the binaries. Line 599: srt can be pointed at another
//! ripgrep with the `ripgrep` setting; tk does not point it at Pi's own
//! `~/.pi/agent/bin/rg`, because srt runs ripgrep outside the sandbox and
//! the agent can write that folder (it is one of Pi's state folders, which
//! srt leaves writable).

use std::path::Path;

/// The tools srt will start outside the sandbox, as absolute paths, for its
/// settings (`bwrapPath`, `socatPath`, `ripgrep.command`; srt 0.0.81
/// `dist/sandbox/sandbox-config.js:1025-1048`, all Linux only).
#[derive(Debug, PartialEq, Eq)]
pub struct Tools {
    pub bwrap: String,
    pub socat: String,
    pub rg: String,
}

/// A missing dependency stops the run before srt starts; so does one whose
/// first copy on PATH, the one srt's `which` finds (`dist/utils/which.js`),
/// sits in a relative PATH entry or in a folder the sandbox can write (fix
/// pass review R3: `PATH=.:/usr/bin` gave srt `./rg`, which runs outside the
/// sandbox). The userns sysctl only warns, since an AppArmor profile can
/// grant what the sysctl takes away and tk cannot read profiles.
pub fn srt(
    path_var: &str,
    linux: bool,
    userns_sysctl: Option<&str>,
    writable: &[String],
    cwd: &str,
) -> Result<(Vec<String>, Option<Tools>), String> {
    let needed: &[(&str, &str)] = if linux {
        &[
            ("bwrap", "bubblewrap"),
            ("socat", "socat"),
            ("rg", "ripgrep"),
        ]
    } else {
        &[("rg", "ripgrep")]
    };
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for (bin, pkg) in needed {
        match first_on_path(path_var, bin, cwd) {
            None => missing.push(format!("{bin} (package {pkg})")),
            Some(p) if !p.starts_with('/') => {
                return Err(format!(
                    "srt would run `{p}`, the first {bin} on PATH, from a relative PATH entry and outside the sandbox; take the relative entry out of PATH"
                ))
            }
            Some(p) => {
                let real = std::fs::canonicalize(&p)
                    .map(|r| r.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| p.clone());
                if let Some(w) = writable.iter().find(|w| under(&real, w)) {
                    return Err(format!(
                        "srt would run {real}, the first {bin} on PATH, outside the sandbox, and it sits in {w}, which the sandbox can write; put a system {bin} first on PATH"
                    ));
                }
                found.push(real);
            }
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "srt needs {} on PATH and will not start without it; install it, or use --boundary sbx or none{}",
            missing.join(", "),
            if missing.iter().any(|m| m.starts_with("rg ")) {
                ". Pi's own rg in ~/.pi/agent/bin is not used: srt runs rg outside the sandbox, and the agent can write that folder"
            } else {
                ""
            }
        ));
    }
    let mut notes = Vec::new();
    if linux && userns_sysctl.map(str::trim) == Some("1") {
        notes.push(
            "tk run: kernel.apparmor_restrict_unprivileged_userns is 1, which takes the capabilities srt's bubblewrap and seccomp layer need (srt README, Ubuntu 24.04+ note); unless an AppArmor profile grants `userns` to srt's binaries, srt will fail with \"Permission denied\" on /proc/self/setgroups. The fix is root's: `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`, or such a profile."
                .to_string(),
        );
    }
    let tools = linux.then(|| Tools {
        bwrap: found[0].clone(),
        socat: found[1].clone(),
        rg: found[2].clone(),
    });
    Ok((notes, tools))
}

/// The first executable `bin` along PATH, as `which` searches it: every
/// entry in order, an empty one meaning the current folder, which for srt is
/// the chat's folder (`tk run` starts it there). A relative entry's match is
/// returned relative, for the caller to refuse.
fn first_on_path(path_var: &str, bin: &str, cwd: &str) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    path_var.split(':').find_map(|d| {
        let d = if d.is_empty() { "." } else { d };
        let p = Path::new(d).join(bin);
        let at = if d.starts_with('/') {
            p.clone()
        } else {
            Path::new(cwd).join(&p)
        };
        std::fs::metadata(&at)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .then(|| p.to_string_lossy().into_owned())
    })
}

/// `path` is `dir` or inside it, both compared canonical where they exist.
fn under(path: &str, dir: &str) -> bool {
    let dir = std::fs::canonicalize(dir)
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dir.to_string());
    let dir = dir.trim_end_matches('/');
    path == dir || path.starts_with(&format!("{dir}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(name: &str, bins: &[&str]) -> String {
        let d = std::env::temp_dir().join(format!("tk-pre-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for b in bins {
            let p = d.join(b);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn a_missing_dependency_stops_the_run_and_names_it() {
        // BX4: the box had bwrap and socat, and rg only in ~/.pi/agent/bin.
        let d = dir_with("norg", &["bwrap", "socat"]);
        let e = srt(&d, true, Some("0"), &[], "/").unwrap_err();
        assert!(e.contains("rg (package ripgrep)"), "{e}");
        assert!(!e.contains("bwrap"), "{e}");
        assert!(e.contains("not used"), "{e}");
        // Relative PATH entries are not searched.
        let e = srt("bin:/nonexistent", false, None, &[], "/").unwrap_err();
        assert!(e.contains("rg"), "{e}");
    }

    #[test]
    fn the_userns_sysctl_warns_before_the_start() {
        // BX3.
        let d = dir_with("all", &["bwrap", "socat", "rg"]);
        let n = srt(&d, true, Some("1\n"), &[], "/").unwrap().0;
        assert_eq!(n.len(), 1);
        assert!(
            n[0].contains("apparmor_restrict_unprivileged_userns"),
            "{n:?}"
        );
        assert!(srt(&d, true, Some("0"), &[], "/").unwrap().0.is_empty());
        assert!(
            srt(&d, true, None, &[], "/").unwrap().0.is_empty(),
            "no such sysctl"
        );
        // macOS needs rg only, and has no such sysctl.
        let d = dir_with("mac", &["rg"]);
        assert!(srt(&d, false, Some("1"), &[], "/").unwrap().0.is_empty());
    }

    #[test]
    fn the_copy_srt_would_run_must_not_be_relative_or_in_a_writable_folder() {
        // Fix pass review R3.
        let sys = dir_with("sys", &["bwrap", "socat", "rg"]);
        let agent = dir_with("agentbin", &["rg"]);
        // The agent's folder first: refused, even though a system rg follows.
        let e = srt(&format!("{agent}:{sys}"), true, None, &[agent.clone()], "/").unwrap_err();
        assert!(e.contains("which the sandbox can write"), "{e}");
        // A folder inside a writable one counts too.
        let parent = Path::new(&agent)
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(srt(&format!("{agent}:{sys}"), true, None, &[parent], "/").is_err());
        // A relative entry that holds rg: refused.
        let e = srt(&format!(".:{sys}"), true, None, &[], &agent).unwrap_err();
        assert!(e.contains("relative PATH entry"), "{e}");
        let e = srt(&format!(":{sys}"), true, None, &[], &agent).unwrap_err();
        assert!(e.contains("relative PATH entry"), "an empty entry: {e}");
        // The system copies, as absolute paths for srt's settings.
        let (_, t) = srt(&format!("{sys}:{agent}"), true, None, &[agent.clone()], "/").unwrap();
        let t = t.unwrap();
        let real = std::fs::canonicalize(&sys)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(t.rg, format!("{real}/rg"));
        assert_eq!(t.bwrap, format!("{real}/bwrap"));
        assert!(
            srt(&format!("{sys}"), false, None, &[], "/")
                .unwrap()
                .1
                .is_none(),
            "macOS: no paths"
        );
    }

    #[test]
    fn a_file_that_is_not_executable_does_not_count() {
        let d = dir_with("noexec", &["bwrap", "socat"]);
        std::fs::write(Path::new(&d).join("rg"), "x").unwrap();
        assert!(srt(&d, true, None, &[], "/").is_err());
    }
}

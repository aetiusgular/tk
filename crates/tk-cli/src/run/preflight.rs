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

/// A missing dependency stops the run before srt starts; the userns sysctl
/// only warns, since an AppArmor profile can grant what the sysctl takes
/// away and tk cannot read profiles.
pub fn srt(
    path_var: &str,
    linux: bool,
    userns_sysctl: Option<&str>,
) -> Result<Vec<String>, String> {
    let needed: &[(&str, &str)] = if linux {
        &[
            ("bwrap", "bubblewrap"),
            ("socat", "socat"),
            ("rg", "ripgrep"),
        ]
    } else {
        &[("rg", "ripgrep")]
    };
    let missing: Vec<&(&str, &str)> = needed
        .iter()
        .filter(|(bin, _)| !on_path(path_var, bin))
        .collect();
    if !missing.is_empty() {
        let names: Vec<String> = missing
            .iter()
            .map(|(bin, pkg)| format!("{bin} (package {pkg})"))
            .collect();
        return Err(format!(
            "srt needs {} on PATH and will not start without it; install it, or use --boundary sbx or none{}",
            names.join(", "),
            if missing.iter().any(|(b, _)| *b == "rg") {
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
    Ok(notes)
}

fn on_path(path_var: &str, bin: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path_var
        .split(':')
        .filter(|d| d.starts_with('/'))
        .map(|d| Path::new(d).join(bin))
        .any(|p| {
            std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
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
        let e = srt(&d, true, Some("0")).unwrap_err();
        assert!(e.contains("rg (package ripgrep)"), "{e}");
        assert!(!e.contains("bwrap"), "{e}");
        assert!(e.contains("not used"), "{e}");
        // Relative PATH entries are not searched.
        let e = srt("bin:.", false, None).unwrap_err();
        assert!(e.contains("rg"), "{e}");
    }

    #[test]
    fn the_userns_sysctl_warns_before_the_start() {
        // BX3.
        let d = dir_with("all", &["bwrap", "socat", "rg"]);
        let n = srt(&d, true, Some("1\n")).unwrap();
        assert_eq!(n.len(), 1);
        assert!(
            n[0].contains("apparmor_restrict_unprivileged_userns"),
            "{n:?}"
        );
        assert!(srt(&d, true, Some("0")).unwrap().is_empty());
        assert!(srt(&d, true, None).unwrap().is_empty(), "no such sysctl");
        // macOS needs rg only, and has no such sysctl.
        let d = dir_with("mac", &["rg"]);
        assert!(srt(&d, false, Some("1")).unwrap().is_empty());
    }

    #[test]
    fn a_file_that_is_not_executable_does_not_count() {
        let d = dir_with("noexec", &["bwrap", "socat"]);
        std::fs::write(Path::new(&d).join("rg"), "x").unwrap();
        assert!(srt(&d, true, None).is_err());
    }
}

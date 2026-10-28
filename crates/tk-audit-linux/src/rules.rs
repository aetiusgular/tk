//! The audit rules file for one user. Printed, never installed: the install
//! runs as root, so it is shown and confirmed in the box test before it runs
//! (rules section 5, item 2, and section 10).
//!
//! Syntax and fields are auditctl's at 3.1.2 (man page, noble). Loaded and
//! checked in the lane's workspace on 27 Oct 2026: `auditctl -R` accepts
//! every line and `auditctl -l` prints `a2&0x3` and `a2&0x40` for the flag
//! filters. A `-F dir=` or `-F path=` rule for a path that does not exist
//! does not load, and without `-c` every rule after it is lost too (run:
//! "There was an error in line 2", then no rules). So the file starts with
//! `-c`, those lines are written only for paths that exist now, and they
//! come last: a path deleted before the next boot costs its own line only.
//!
//! What it records, for processes of `uid` only (the kernel cannot filter
//! by process tree; `tree.rs` does that):
//! - `tk_exec`: execve and execveat, plus those of root processes whose
//!   login uid is the user's (a harness that ran `sudo`);
//! - `tk_fork`: clone, fork, vfork, clone3, for the tree;
//! - `tk_write`: open and openat with O_WRONLY, O_RDWR or O_CREAT, creat,
//!   openat2 (flags not filterable), truncate, rename*, unlink*, link*,
//!   symlink*;
//! - `tk_net`: connect (and i386 socketcall with call 3, connect);
//! - `tk_sens`: opens under the sensitive folders and of the sensitive files
//!   that exist when the file is made.
//!
//! The write rules come before the sensitive ones: the first matching rule
//! sets the key, and a read of `~/.ssh/id_x` matches no write rule, so it
//! gets `tk_sens`.

use crate::sensitive::{HOME_DIRS, HOME_FILES, SYSTEM_DIRS, SYSTEM_FILES};
use crate::syscalls::{names, Call, ARCH_I386, ARCH_X86_64};

pub struct RulesInput<'a> {
    pub uid: u32,
    pub home: &'a str,
    /// `$XDG_RUNTIME_DIR`, where the daemon keeps its token when it is set.
    pub runtime_dir: Option<&'a str>,
    /// Whether a path exists; the caller passes `Path::exists`.
    pub exists: &'a dyn Fn(&str) -> bool,
}

/// The rules text, and the sensitive paths left out because they do not
/// exist (shown to the user with the file).
pub fn render(input: &RulesInput) -> (String, Vec<String>) {
    let uid = input.uid;
    let mut out = String::new();
    out.push_str("## tk: the Linux OS feed (crates/tk-audit-linux). Generated; do not edit.\n");
    out.push_str("## Install (root, shown before it runs): install -m 0640 this file\n");
    out.push_str("##   /etc/audit/rules.d/50-tk.rules && augenrules --load\n");
    out.push_str(&format!(
        "## Records processes of uid {uid}; the feed keeps only a tk run chat's process tree.\n"
    ));
    out.push_str(
        "## -c: keep loading past a rule that fails (a sensitive path deleted since).\n-c\n",
    );
    for (arch, tag) in [(ARCH_X86_64, "b64"), (ARCH_I386, "b32")] {
        let list = |kinds: &[Call]| names(arch, kinds).join(",");
        let a = format!("-a always,exit -F arch={tag}");
        out.push_str(&format!(
            "{a} -S {} -F uid={uid} -k tk_exec\n",
            list(&[Call::Exec])
        ));
        out.push_str(&format!(
            "{a} -S {} -F auid={uid} -F euid=0 -k tk_exec\n",
            list(&[Call::Exec])
        ));
        out.push_str(&format!(
            "{a} -S {} -F uid={uid} -k tk_fork\n",
            list(&[Call::Fork])
        ));
        out.push_str(&format!(
            "{a} -S openat -F a2&03 -F uid={uid} -k tk_write\n"
        ));
        out.push_str(&format!(
            "{a} -S openat -F a2&0100 -F uid={uid} -k tk_write\n"
        ));
        out.push_str(&format!("{a} -S open -F a1&03 -F uid={uid} -k tk_write\n"));
        out.push_str(&format!(
            "{a} -S open -F a1&0100 -F uid={uid} -k tk_write\n"
        ));
        out.push_str(&format!(
            "{a} -S {} -F uid={uid} -k tk_write\n",
            list(&[
                Call::Creat,
                Call::OpenAt2,
                Call::Truncate,
                Call::Rename,
                Call::Unlink,
                Call::Link,
                Call::Symlink
            ])
        ));
        out.push_str(&format!("{a} -S connect -F uid={uid} -k tk_net\n"));
        if arch == ARCH_I386 {
            out.push_str(&format!(
                "{a} -S socketcall -F a0=3 -F uid={uid} -k tk_net\n"
            ));
        }
    }
    let home = input.home.trim_end_matches('/');
    let mut skipped = Vec::new();
    let mut sens = |out: &mut String, field: &str, path: &str| {
        if (input.exists)(path) {
            for tag in ["b64", "b32"] {
                out.push_str(&format!(
                    "-a always,exit -F arch={tag} -S open,openat -F {field}={path} -F uid={uid} -k tk_sens\n"
                ));
            }
        } else {
            skipped.push(path.to_string());
        }
    };
    for d in HOME_DIRS {
        sens(&mut out, "dir", &format!("{home}/{d}"));
    }
    for d in SYSTEM_DIRS {
        sens(&mut out, "dir", d);
    }
    if let Some(rt) = input.runtime_dir.filter(|r| plain_path(r)) {
        sens(&mut out, "dir", &format!("{}/tk", rt.trim_end_matches('/')));
    }
    for f in HOME_FILES {
        sens(&mut out, "path", &format!("{home}/{f}"));
    }
    for f in SYSTEM_FILES {
        sens(&mut out, "path", f);
    }
    (out, skipped)
}

/// Whether a home path can go into a rule line unquoted: auditctl splits on
/// spaces, and the rules file has no quoting.
pub fn plain_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.split('/').any(|p| p == "..")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_both_abis_and_only_existing_sensitive_paths() {
        let exists = |p: &str| p.ends_with("/.ssh") || p == "/etc/shadow";
        let (text, skipped) = render(&RulesInput {
            uid: 1000,
            home: "/home/ubuntu",
            runtime_dir: Some("/run/user/1000"),
            exists: &exists,
        });
        assert!(text.contains("\n-c\n"), "keeps loading past a failed rule");
        assert!(skipped.contains(&"/run/user/1000/tk".to_string()));
        assert!(
            text.contains("-a always,exit -F arch=b64 -S execve,execveat -F uid=1000 -k tk_exec\n")
        );
        assert!(text.contains("-F arch=b32 -S socketcall -F a0=3 -F uid=1000 -k tk_net\n"));
        assert!(text.contains("-F dir=/home/ubuntu/.ssh -F uid=1000 -k tk_sens\n"));
        assert!(text.contains("-F path=/etc/shadow"));
        assert!(!text.contains(".netrc"));
        assert!(skipped.contains(&"/home/ubuntu/.netrc".to_string()));
        // The write rules come before the sensitive ones (first match keys).
        assert!(text.find("tk_write").unwrap() < text.find("tk_sens").unwrap());
        // No rule names a key longer than 31 bytes (auditctl's limit).
        for line in text.lines().filter(|l| l.starts_with("-a")) {
            let key = line.rsplit("-k ").next().unwrap();
            assert!(key.len() <= 31, "{line}");
        }
    }

    #[test]
    fn plain_paths_only() {
        assert!(plain_path("/home/ubuntu"));
        assert!(!plain_path("/home/a b"));
        assert!(!plain_path("/home/../etc"));
        assert!(!plain_path("relative"));
    }
}

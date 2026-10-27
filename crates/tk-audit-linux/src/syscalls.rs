//! The syscalls the rules audit, by architecture. Numbers checked with
//! `ausyscall <arch> <name> --exact` from auditd 3.1.2 on 27 Oct 2026
//! (lane D status doc, Research). Only x86_64 and its 32-bit ABI: the box is
//! a Ryzen; arm64 Linux has a different table and is not covered.

/// `arch=` values in a SYSCALL record (`AUDIT_ARCH_*`).
pub const ARCH_X86_64: &str = "c000003e";
pub const ARCH_I386: &str = "40000003";

/// What a syscall means for the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Exec,
    Fork,
    /// `open`: flags in a1.
    Open,
    /// `openat`: flags in a2.
    OpenAt,
    /// `openat2`: flags in a struct the record does not show.
    OpenAt2,
    Creat,
    Truncate,
    Rename,
    Unlink,
    Link,
    Symlink,
    Connect,
    /// i386 `socketcall`; a0 is the call (3 is connect).
    SocketCall,
}

const X86_64: &[(u32, &str, Call)] = &[
    (59, "execve", Call::Exec),
    (322, "execveat", Call::Exec),
    (56, "clone", Call::Fork),
    (57, "fork", Call::Fork),
    (58, "vfork", Call::Fork),
    (435, "clone3", Call::Fork),
    (2, "open", Call::Open),
    (257, "openat", Call::OpenAt),
    (437, "openat2", Call::OpenAt2),
    (85, "creat", Call::Creat),
    (76, "truncate", Call::Truncate),
    (82, "rename", Call::Rename),
    (264, "renameat", Call::Rename),
    (316, "renameat2", Call::Rename),
    (87, "unlink", Call::Unlink),
    (263, "unlinkat", Call::Unlink),
    (86, "link", Call::Link),
    (265, "linkat", Call::Link),
    (88, "symlink", Call::Symlink),
    (266, "symlinkat", Call::Symlink),
    (42, "connect", Call::Connect),
];

const I386: &[(u32, &str, Call)] = &[
    (11, "execve", Call::Exec),
    (358, "execveat", Call::Exec),
    (120, "clone", Call::Fork),
    (2, "fork", Call::Fork),
    (190, "vfork", Call::Fork),
    (435, "clone3", Call::Fork),
    (5, "open", Call::Open),
    (295, "openat", Call::OpenAt),
    (437, "openat2", Call::OpenAt2),
    (8, "creat", Call::Creat),
    (92, "truncate", Call::Truncate),
    (38, "rename", Call::Rename),
    (302, "renameat", Call::Rename),
    (353, "renameat2", Call::Rename),
    (10, "unlink", Call::Unlink),
    (301, "unlinkat", Call::Unlink),
    (9, "link", Call::Link),
    (303, "linkat", Call::Link),
    (83, "symlink", Call::Symlink),
    (304, "symlinkat", Call::Symlink),
    (362, "connect", Call::Connect),
    (102, "socketcall", Call::SocketCall),
];

fn table(arch: &str) -> Option<&'static [(u32, &'static str, Call)]> {
    match arch {
        ARCH_X86_64 => Some(X86_64),
        ARCH_I386 => Some(I386),
        _ => None,
    }
}

/// The call and its name for an `arch` and `syscall` number.
pub fn lookup(arch: &str, number: u32) -> Option<(Call, &'static str)> {
    table(arch)?
        .iter()
        .find(|(n, _, _)| *n == number)
        .map(|(_, name, call)| (*call, *name))
}

/// The names of the calls of a kind on an architecture, for the rules file.
pub fn names(arch: &str, kinds: &[Call]) -> Vec<&'static str> {
    table(arch)
        .unwrap_or(&[])
        .iter()
        .filter(|(_, _, c)| kinds.contains(c))
        .map(|(_, name, _)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recorded_numbers_resolve() {
        assert_eq!(lookup(ARCH_X86_64, 257), Some((Call::OpenAt, "openat")));
        assert_eq!(lookup(ARCH_X86_64, 316), Some((Call::Rename, "renameat2")));
        assert_eq!(lookup(ARCH_X86_64, 263), Some((Call::Unlink, "unlinkat")));
        assert_eq!(lookup(ARCH_X86_64, 56), Some((Call::Fork, "clone")));
        assert_eq!(lookup(ARCH_X86_64, 42), Some((Call::Connect, "connect")));
        assert_eq!(lookup(ARCH_I386, 2), Some((Call::Fork, "fork")));
        assert_eq!(lookup(ARCH_I386, 5), Some((Call::Open, "open")));
        assert_eq!(lookup(ARCH_X86_64, 0), None);
        assert_eq!(lookup("c00000b7", 221), None, "arm64 is not covered");
    }

    #[test]
    fn numbers_are_unique_per_arch() {
        for t in [X86_64, I386] {
            for (i, a) in t.iter().enumerate() {
                assert!(t[i + 1..].iter().all(|b| b.0 != a.0), "{}", a.1);
            }
        }
    }
}

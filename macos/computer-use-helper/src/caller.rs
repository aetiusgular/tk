//! Who may talk to the helper: MonoCode only, checked by code signature
//! (note W, section 4). Agents can start the helper with open(1) and connect
//! to its socket; the check is what refuses them.

/// The bundle id of the MonoCode the helper answers: `MONOCODE_APP_ID` at
/// build time, else `com.monocode.desktop`, or `com.monocode.desktop.dev` for
/// a debug helper (the user's decision 1). `bundle.sh` sets it the same way.
pub fn monocode_app_id() -> Option<&'static str> {
    let id = option_env!("MONOCODE_APP_ID").unwrap_or(if cfg!(debug_assertions) {
        "com.monocode.desktop.dev"
    } else {
        "com.monocode.desktop"
    });
    plain_id(id).then_some(id)
}

/// A reverse-DNS id that can go inside a quoted requirement as is.
fn plain_id(id: &str) -> bool {
    id.len() <= 255
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// The code requirement MonoCode's signature must meet, built in at build
/// time from the certificate both apps are signed with
/// (`MONOCODE_HELPER_CERT_SHA1`, set by `bundle.sh`; plan section 3, call 4).
/// Absent in a build without one.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "called only from the macOS `run` in main.rs")
)]
pub fn built_requirement() -> Option<&'static str> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let sha1 = option_env!("MONOCODE_HELPER_CERT_SHA1")?;
            requirement_for(monocode_app_id()?, sha1)
        })
        .as_deref()
}

/// The requirement for MonoCode (`app_id`) signed with one certificate,
/// pinned by its SHA-1. A name is not enough: any process of the user can
/// make a self-signed certificate with the same common name (security
/// review, finding 1). Anything but 40 hex digits, or an id that is not
/// plain reverse-DNS, is refused.
pub fn requirement_for(app_id: &str, sha1: &str) -> Option<String> {
    let sha1 = sha1.trim();
    if sha1.len() != 40 || !sha1.bytes().all(|b| b.is_ascii_hexdigit()) || !plain_id(app_id) {
        return None;
    }
    Some(format!(
        "identifier \"{app_id}\" and certificate leaf = H\"{}\"",
        sha1.to_ascii_uppercase()
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Refuse(&'static str),
}

/// The decision, from what the platform could tell about the peer.
///
/// - `signed`: whether the peer's code met the built requirement; None when
///   no requirement was built in or the platform could not check.
/// - `peer_pid` and `parent_pid`: the peer's pid, and the MonoCode pid the
///   helper was started for.
/// - `debug`: a debug build of the helper.
///
/// A release build with no requirement refuses everyone. A debug build with
/// no requirement takes only the pid it was started for, and says so in its
/// log: an agent that starts the helper itself can name its own pid.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "called only from the macOS `run` in main.rs")
)]
pub fn decide(
    signed: Option<bool>,
    peer_pid: Option<i32>,
    parent_pid: i32,
    debug: bool,
) -> Verdict {
    match signed {
        Some(true) if peer_pid == Some(parent_pid) => Verdict::Allow,
        Some(true) => {
            Verdict::Refuse("caller is signed but is not the MonoCode that started the helper")
        }
        Some(false) => Verdict::Refuse("caller's code signature does not match MonoCode's"),
        None if debug && peer_pid == Some(parent_pid) => Verdict::Allow,
        None if debug => Verdict::Refuse("caller is not the MonoCode that started the helper"),
        None => Verdict::Refuse("this build has no signing requirement, so it takes no caller"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_monocode_with_the_right_pid_is_let_in() {
        assert_eq!(decide(Some(true), Some(10), 10, false), Verdict::Allow);
    }

    #[test]
    fn everything_else_is_refused_in_a_release_build() {
        assert!(matches!(
            decide(Some(false), Some(10), 10, false),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(Some(true), Some(11), 10, false),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(None, Some(10), 10, false),
            Verdict::Refuse(_)
        ));
        assert!(matches!(
            decide(Some(true), None, 10, false),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn a_debug_build_without_a_requirement_takes_only_its_parent() {
        assert_eq!(decide(None, Some(10), 10, true), Verdict::Allow);
        assert!(matches!(
            decide(None, Some(11), 10, true),
            Verdict::Refuse(_)
        ));
        assert!(matches!(decide(None, None, 10, true), Verdict::Refuse(_)));
        // A failed signature check is never waved through, debug or not.
        assert!(matches!(
            decide(Some(false), Some(10), 10, true),
            Verdict::Refuse(_)
        ));
    }

    #[test]
    fn the_requirement_names_the_monocode_app_id_built_in() {
        // Debug helpers answer the dev build of MonoCode (the user's
        // decision 1); `MONOCODE_APP_ID` set at build time wins.
        let expected = option_env!("MONOCODE_APP_ID").unwrap_or(if cfg!(debug_assertions) {
            "com.monocode.desktop.dev"
        } else {
            "com.monocode.desktop"
        });
        assert_eq!(monocode_app_id(), Some(expected));
        assert_eq!(
            requirement_for("com.monocode.desktop.dev", "0123456789abcdef0123456789abcdef01234567").unwrap(),
            "identifier \"com.monocode.desktop.dev\" and certificate leaf = H\"0123456789ABCDEF0123456789ABCDEF01234567\""
        );
        for bad in [
            "",
            "com",
            "com.monocode.\"x",
            "com.mono code",
            "com..x",
            ".com.x",
        ] {
            assert!(
                requirement_for(bad, "0123456789abcdef0123456789abcdef01234567").is_none(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_built_requirement_pins_the_built_in_certificate() {
        match option_env!("MONOCODE_HELPER_CERT_SHA1") {
            // A plain `cargo test` builds none in, and so answers no one.
            None => assert_eq!(built_requirement(), None),
            Some(sha1) => assert_eq!(
                built_requirement().map(str::to_string),
                requirement_for(monocode_app_id().unwrap(), sha1)
            ),
        }
    }

    #[test]
    fn requirement_text() {
        assert_eq!(
            requirement_for("com.monocode.desktop", "0123456789abcdef0123456789abcdef01234567").unwrap(),
            "identifier \"com.monocode.desktop\" and certificate leaf = H\"0123456789ABCDEF0123456789ABCDEF01234567\""
        );
        // A name, a quote trick, a short hash: all refused.
        assert!(requirement_for("com.monocode.desktop", "MonoCode Dev").is_none());
        assert!(requirement_for("com.monocode.desktop", "x\" or true or \"").is_none());
        assert!(requirement_for(
            "com.monocode.desktop",
            "0123456789abcdef0123456789abcdef0123456"
        )
        .is_none());
        assert!(requirement_for("com.monocode.desktop", " ").is_none());
    }
}

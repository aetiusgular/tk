//! The bounded capability manifest for one chat's Cua runtime (C2).
//!
//! Format from Cua's "write a bounded manifest" guide, read 16 Oct 2026:
//! YAML, `version: 3`, required `expires_after` and `idle_timeout`, a deny-by-
//! default `allow.tools` list, apps under `resources.apps` by `bundle_id` on
//! macOS, and `resources.desktop.display`. The mode is not in the file: the
//! helper starts the runtime with `CUA_DRIVER_PERMISSION_MODE=bounded`.
//!
//! The YAML is written by hand from typed values. Every string goes out as a
//! double-quoted scalar with escapes, so a bundle id can never add a key.

use super::apps::is_never;
use super::grants::Grant;

/// The tools an agent may call. Everything else is denied by the runtime
/// (not in `allow.tools`) and refused before it reaches the runtime (the
/// facade and the helper check this list too).
///
/// Left out, with why:
/// - `get_desktop_state`: the whole display, MonoCode's windows included.
/// - `clipboard_read`, `clipboard_write`: data from apps not granted.
/// - `run_steps`, `run_script`: batches the card never showed.
/// - browser tools and `page`: a browser needs its own runtime and origins.
/// - agent cursor tools: the overlay is how you see the agent act.
/// - `list_sessions`, deprecated session tools: other runtimes' sessions.
/// - recording, replay and `install_ffmpeg`: the helper's, never the agent's.
/// - `get_config`, `set_config`, `check_permissions`, `health_report`,
///   `check_for_update`, `install_extension`: maintenance, and network.
pub const ALLOWED_TOOLS: &[&str] = &[
    "list_apps",
    "list_windows",
    "launch_app",
    "kill_app",
    "bring_to_front",
    "set_window_frame",
    "invoke_menu",
    "get_window_state",
    "get_accessibility_tree",
    "verify_state",
    "parse_visual_regions",
    "get_screen_size",
    "get_cursor_position",
    "zoom",
    "click",
    "double_click",
    "right_click",
    "drag",
    "scroll",
    "move_cursor",
    "type_text",
    "press_key",
    "hotkey",
    "set_value",
    "start_session",
    "get_session",
    "end_session",
];

/// Tools in the manifest that only the helper calls, on its own connection,
/// to record the runtime (C7). The facade and the helper refuse them from
/// MonoCode's side, so no agent reaches them.
pub const HELPER_ONLY_TOOLS: &[&str] =
    &["start_recording", "stop_recording", "get_recording_state"];

/// How long a runtime may live, and sit idle. A runtime that ends is started
/// again on the next call while the grants hold.
pub const EXPIRES_AFTER: &str = "8h";
pub const IDLE_TIMEOUT: &str = "30m";

pub fn tool_allowed(name: &str) -> bool {
    ALLOWED_TOOLS.contains(&name)
}

/// The manifest for these grants, or None when there is nothing to grant.
/// MonoCode and the other ids `apps::is_never` names are dropped even if a
/// grant somehow holds one.
pub fn manifest_yaml(grants: &[Grant]) -> Option<String> {
    let mut ids: Vec<&str> = Vec::new();
    for grant in grants {
        let id = grant.bundle_id.as_str();
        if is_never(id) || !plain_id(id) {
            continue;
        }
        if !ids.iter().any(|seen| seen.eq_ignore_ascii_case(id)) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str("# Written by MonoCode for one chat. Do not edit.\n");
    out.push_str("version: 3\n");
    out.push_str(&format!("expires_after: {}\n", quote(EXPIRES_AFTER)));
    out.push_str(&format!("idle_timeout: {}\n", quote(IDLE_TIMEOUT)));
    out.push_str("allow:\n  tools:\n");
    for tool in ALLOWED_TOOLS.iter().chain(HELPER_ONLY_TOOLS) {
        out.push_str(&format!("    - {}\n", quote(tool)));
    }
    out.push_str("resources:\n  apps:\n");
    for id in ids {
        out.push_str(&format!("    - bundle_id: {}\n", quote(id)));
        out.push_str("      launch: true\n");
        out.push_str("      windows: all\n");
        out.push_str("      terminate: driver_launched\n");
    }
    out.push_str("  desktop:\n    display: false\n");
    Some(out)
}

/// The same check `apps::parse_bundle_id` makes, for ids already granted.
fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// A YAML double-quoted scalar.
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(id: &str) -> Grant {
        Grant {
            chat_id: "c1".into(),
            bundle_id: id.into(),
            app_name: None,
            granted_at: 1,
            ended_at: None,
            approval_id: "a".into(),
        }
    }

    #[test]
    fn no_grants_no_manifest() {
        assert_eq!(manifest_yaml(&[]), None);
        assert_eq!(manifest_yaml(&[grant("com.monocode.desktop")]), None);
    }

    #[test]
    fn writes_the_bounded_shape_for_the_granted_apps() {
        let yaml =
            manifest_yaml(&[grant("com.apple.calculator"), grant("com.apple.TextEdit")]).unwrap();
        assert!(yaml.contains("version: 3\n"));
        assert!(yaml.contains("expires_after: \"8h\"\n"));
        assert!(yaml.contains("idle_timeout: \"30m\"\n"));
        assert!(yaml.contains("    - bundle_id: \"com.apple.calculator\"\n"));
        assert!(yaml.contains("    - bundle_id: \"com.apple.TextEdit\"\n"));
        assert!(yaml.contains("terminate: driver_launched"));
        assert!(yaml.ends_with("  desktop:\n    display: false\n"));
        assert!(
            !yaml.contains("mode"),
            "the mode is set at launch, not in the file"
        );
    }

    #[test]
    fn never_names_monocode_or_a_duplicate() {
        let yaml = manifest_yaml(&[
            grant("com.apple.calculator"),
            grant("COM.APPLE.CALCULATOR"),
            grant("com.monocode.desktop"),
            grant("com.monocode.computer-use-helper"),
            grant("com.trycua.driver"),
        ])
        .unwrap();
        assert_eq!(yaml.matches("bundle_id:").count(), 1);
        assert!(!yaml.to_lowercase().contains("monocode\""));
        assert!(!yaml.contains("trycua"));
    }

    #[test]
    fn a_bad_id_cannot_add_yaml() {
        let yaml = manifest_yaml(&[
            grant("com.apple.calculator"),
            grant("x\"\n  desktop:\n    display: true"),
        ])
        .unwrap();
        assert_eq!(yaml.matches("display:").count(), 1);
        assert!(yaml.contains("display: false"));
    }

    #[test]
    fn the_agent_list_leaves_out_what_the_card_never_showed() {
        for never in [
            "get_desktop_state",
            "clipboard_read",
            "clipboard_write",
            "run_script",
            "run_steps",
            "start_recording",
            "stop_recording",
            "replay_trajectory",
            "set_config",
            "check_for_update",
            "install_extension",
            "set_agent_cursor_enabled",
            "browser_navigate",
            "page",
        ] {
            assert!(!tool_allowed(never), "{never}");
        }
        for tool in ["click", "type_text", "get_window_state", "list_apps"] {
            assert!(tool_allowed(tool), "{tool}");
        }
    }

    #[test]
    fn the_manifest_lists_recording_only_for_the_helper() {
        let yaml = manifest_yaml(&[grant("com.apple.calculator")]).unwrap();
        for tool in HELPER_ONLY_TOOLS {
            assert!(yaml.contains(&format!("- \"{tool}\"")));
            assert!(!tool_allowed(tool));
        }
        assert!(!yaml.contains("get_desktop_state"));
    }

    #[test]
    fn quotes_escape() {
        assert_eq!(quote("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }
}

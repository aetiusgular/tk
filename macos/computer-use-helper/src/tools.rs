//! Which driver tools pass through the helper. MonoCode keeps the same list in
//! `src-tauri/src/computer_use/manifest.rs`, and a test there checks the two
//! match, name for name.

/// Tools MonoCode may call for an agent.
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

/// Tools the helper calls itself, on its own connection, and never relays.
pub const HELPER_ONLY_TOOLS: &[&str] =
    &["start_recording", "stop_recording", "get_recording_state"];

/// Relayed tools' arguments that make Cua write a file at a path the caller
/// names. The recording grant covers the recording folder for the whole
/// runtime, so either could write a forged `action.json` there (security
/// review X10). Checked against Cua 0.37.0's schemas: `click.debug_image_out`
/// (`platform-macos/src/tools/click.rs:356-359`, written by
/// `cua-driver-core/src/image_utils.rs:134`) and
/// `get_window_state.screenshot_out_file`
/// (`platform-macos/src/tools/get_window_state.rs:56-59`, checked against
/// `files.write` at `cua-driver-core/src/tool.rs:2875-2889`). No other
/// relayed tool takes a path that Cua writes to.
pub const OUTPUT_PATH_ARGUMENTS: &[(&str, &str)] = &[
    ("click", "debug_image_out"),
    ("get_window_state", "screenshot_out_file"),
];

/// The output-path argument a call sets, if any. Such a call is refused,
/// not stripped, so the agent learns why instead of seeing a silent change.
pub fn output_path_argument(name: &str, arguments: &serde_json::Value) -> Option<&'static str> {
    OUTPUT_PATH_ARGUMENTS
        .iter()
        .find(|(tool, argument)| *tool == name && arguments.get(*argument).is_some())
        .map(|(_, argument)| *argument)
}

/// A tool MonoCode may relay. The helper's own tools never are, even if a
/// later edit put one in both lists.
pub fn relayable(name: &str) -> bool {
    ALLOWED_TOOLS.contains(&name) && !HELPER_ONLY_TOOLS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_is_never_relayed() {
        for tool in HELPER_ONLY_TOOLS {
            assert!(!relayable(tool));
        }
        assert!(!relayable("get_desktop_state"));
        assert!(!relayable("set_config"));
        assert!(relayable("click"));
    }

    #[test]
    fn no_relayed_tool_is_one_that_writes_files() {
        // Cua 0.37.0 tools that write a file by design; none may be relayed.
        for tool in [
            "start_recording",
            "stop_recording",
            "replay_trajectory",
            "get_desktop_state",
            "browser_download",
            "browser_set_input_files",
            "clipboard_write",
            "install_ffmpeg",
            "install_extension",
            "set_config",
            "run_script",
        ] {
            assert!(!relayable(tool), "{tool}");
        }
        for tool in ALLOWED_TOOLS {
            let lower = tool.to_ascii_lowercase();
            for word in [
                "record", "file", "save", "write", "export", "download", "output",
            ] {
                assert!(!lower.contains(word), "{tool} looks like it writes files");
            }
        }
        for (tool, _) in OUTPUT_PATH_ARGUMENTS {
            assert!(
                relayable(tool),
                "{tool} is relayed, so its argument is checked"
            );
        }
    }

    #[test]
    fn output_path_arguments_are_found() {
        use serde_json::json;
        assert_eq!(
            output_path_argument("click", &json!({"x": 1, "debug_image_out": "/x.png"})),
            Some("debug_image_out")
        );
        assert_eq!(
            output_path_argument("get_window_state", &json!({"screenshot_out_file": ""})),
            Some("screenshot_out_file")
        );
        assert_eq!(output_path_argument("click", &json!({"x": 1})), None);
        assert_eq!(
            output_path_argument("drag", &json!({"debug_image_out": "/x"})),
            None
        );
    }
}

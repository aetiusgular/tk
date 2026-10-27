//! What a harness is started with so it exports to the receiver: pure
//! functions, tested here and mirrored by `tests/cli-contract/checks/otel.mjs`
//! (keep the two in step).
//!
//! The token never goes in an argument: any process on the Mac, of any
//! user, can read another process's arguments with `ps`. Claude Code gets
//! the path of a 0700 script that prints it (`otelHeadersHelper`), Codex
//! gets it in its environment, readable only by the same user. Everything
//! in the arguments here is free to read: the port, the switches, the
//! script's path.

/// The header that carries a chat's token. A plain name with no space in the
/// value, so the `OTEL_EXPORTER_OTLP_HEADERS` form (`key=value`, percent
/// decoded by some SDKs) cannot mangle it.
pub const TOKEN_HEADER: &str = "x-monocode-token";

/// How often Claude Code flushes its log batch, in milliseconds. Short, so a
/// tool call reaches the feed while its adapter event still waits to join.
pub const CLAUDE_LOGS_EXPORT_INTERVAL_MS: u32 = 1000;

/// Which harness, from MonoCode's provider name and the spawn's arguments.
/// Only an interactive Claude Code chat and a Codex app-server qualify:
/// helper prompts, catalogue probes and sign-ins never get telemetry.
pub fn telemetry_harness(binary_provider: Option<&str>, args: &[String]) -> Option<&'static str> {
    match binary_provider? {
        "claude" => {
            let interactive = args.iter().any(|a| a == "--input-format")
                && !args
                    .iter()
                    .any(|a| a == "--no-session-persistence" || a == "-p" || a == "--print");
            interactive.then_some("claude")
        }
        "codex" => (args.first().map(String::as_str) == Some("app-server")).then_some("codex"),
        _ => None,
    }
}

/// Variables that mean the user already sends Claude Code's telemetry
/// somewhere. MonoCode then leaves it alone.
pub const CLAUDE_OWN_TELEMETRY_VARS: &[&str] = &[
    "CLAUDE_CODE_ENABLE_TELEMETRY",
    "OTEL_LOGS_EXPORTER",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
];

/// True when the inherited environment already configures an export.
pub fn has_own_env_telemetry(env: &dyn Fn(&str) -> Option<String>) -> bool {
    CLAUDE_OWN_TELEMETRY_VARS
        .iter()
        .any(|name| env(name).is_some_and(|v| !v.trim().is_empty()))
}

/// True when a Codex `config.toml` exports logs or traces somewhere itself.
/// `exporter = "none"` (the local home's setting) does not count.
pub fn has_own_codex_otel(config_toml: Option<&str>) -> bool {
    let Some(text) = config_toml else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Table>(text) else {
        // An unreadable config: assume the user's, and keep out.
        return true;
    };
    let Some(otel) = value.get("otel").and_then(toml::Value::as_table) else {
        return false;
    };
    ["exporter", "trace_exporter"]
        .iter()
        .any(|key| match otel.get(*key) {
            None => false,
            Some(toml::Value::String(s)) => s != "none",
            Some(_) => true,
        })
}

/// Claude Code's environment for an export to `endpoint`
/// (`http://127.0.0.1:<port>`; Claude Code adds `/v1/logs`). Logs only:
/// every event the log uses is a log record. Tool details on; prompt text by
/// the switch. The returned `remove` list is unset in the child.
///
/// No token here: it goes through `otelHeadersHelper` (`headers_helper_script`,
/// `settings_with_helper`), as Claude Code's docs ask for a collector's
/// credential (https://code.claude.com/docs/en/monitoring-usage). The helper
/// runs only for the http protocols, so the protocol stays `http/json`.
pub fn claude_env(endpoint: &str, log_prompts: bool) -> (Vec<(String, String)>, Vec<String>) {
    let mut set = vec![
        ("CLAUDE_CODE_ENABLE_TELEMETRY", "1".to_string()),
        ("OTEL_LOGS_EXPORTER", "otlp".to_string()),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json".to_string()),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.to_string()),
        (
            "OTEL_LOGS_EXPORT_INTERVAL",
            CLAUDE_LOGS_EXPORT_INTERVAL_MS.to_string(),
        ),
        ("OTEL_LOG_TOOL_DETAILS", "1".to_string()),
    ];
    let mut remove = Vec::new();
    if log_prompts {
        set.push(("OTEL_LOG_USER_PROMPTS", "1".to_string()));
    } else {
        remove.push("OTEL_LOG_USER_PROMPTS".to_string());
    }
    (
        set.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        remove,
    )
}

/// Where no helper script can run (Windows), the token goes in the header
/// variable as before. Claude Code does not pass `OTEL_*` on to the
/// processes it starts.
pub fn claude_header_var(token: &str) -> (String, String) {
    (
        "OTEL_EXPORTER_OTLP_HEADERS".to_string(),
        format!("{TOKEN_HEADER}={token}"),
    )
}

/// The `otelHeadersHelper` script for one chat: prints the header as the
/// JSON object Claude Code expects. The token is two simple UUIDs (hex), so
/// it needs no quoting inside the single quotes.
pub fn headers_helper_script(token: &str) -> String {
    format!("#!/bin/sh\nprintf '%s\\n' '{{\"{TOKEN_HEADER}\":\"{token}\"}}'\n")
}

/// The `--settings` JSON to add after the spawn's own: the last `--settings`
/// on the command line is the one Claude Code reads (`Rv` returns
/// `.at(-1)` in the 2.1.304 bundle), so this is a copy of it with
/// `otelHeadersHelper` added. `None` when that value is a file path rather
/// than JSON, which MonoCode never passes; the header variable is used then.
pub fn settings_with_helper(args: &[String], helper: &str) -> Option<String> {
    let mut last = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--settings" {
            last = args.get(i + 1).map(String::as_str);
            i += 1;
        } else if let Some(v) = args[i].strip_prefix("--settings=") {
            last = Some(v);
        }
        i += 1;
    }
    let mut value = match last {
        None => serde_json::Map::new(),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text).ok()? {
            serde_json::Value::Object(map) => map,
            _ => return None,
        },
    };
    value.insert(
        "otelHeadersHelper".to_string(),
        serde_json::Value::String(helper.to_string()),
    );
    serde_json::to_string(&value).ok()
}

/// Codex's `-c` flags for an export to `endpoint`. Codex takes each signal's
/// full URL. Traces and metrics off: metrics would otherwise go to Codex's
/// default, Statsig, an outbound call MonoCode does not need.
pub fn codex_args(endpoint: &str, log_prompts: bool) -> Vec<String> {
    let logs = format!("{endpoint}/v1/logs");
    vec![
        "-c".into(),
        format!(
            "otel.exporter={{otlp-http={{endpoint={},protocol=\"json\"}}}}",
            toml_string(&logs)
        ),
        "-c".into(),
        "otel.trace_exporter=\"none\"".into(),
        "-c".into(),
        "otel.metrics_exporter=\"none\"".into(),
        "-c".into(),
        format!("otel.log_user_prompt={log_prompts}"),
    ]
}

/// Codex's environment: the token, read by the OTLP exporter beside the
/// headers in its config.
pub fn codex_env(token: &str) -> Vec<(String, String)> {
    let header = format!("{TOKEN_HEADER}={token}");
    vec![
        ("OTEL_EXPORTER_OTLP_HEADERS".to_string(), header.clone()),
        ("OTEL_EXPORTER_OTLP_LOGS_HEADERS".to_string(), header),
    ]
}

/// The token's variables. Codex's shell tools inherit its whole environment
/// by default (`codex-rs/config/src/shell_environment_policy.rs:135-136`), so
/// every command the agent ran would see the token without these excludes.
pub const CODEX_TOKEN_VARS: &[&str] = &[
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
];

/// Kept out of Codex's shell commands besides the telemetry token: the chat's
/// computer-use endpoint and token (`computer_use/bridge_cli.rs`). Codex
/// hands them to that MCP server through `env_vars`, and its shell inherits
/// everything by default (`shell_environment_policy.rs:136`, default
/// excludes off), so an agent could read the token with `env` (security
/// review X2). `MONOCODE_APP_*` and `MONOCODE_CONTROL_*` stay: the agent runs
/// the app and control CLIs in its shell and they read them there
/// (`control_cli.rs:536-553`).
pub const CODEX_SHELL_EXCLUDES: &[&str] = &["MONOCODE_CU_*"];

/// Codex's `-c` flags that keep the tokens out of its shell commands, written
/// so the user's own shell environment filters survive. A `-c` layer is
/// merged over `config.toml`: a legacy `exclude` array there replaces a lower
/// one whole and drops a lower `filters` table, and a `filters` table drops
/// lower `exclude`/`include_only` arrays (`config/src/merge.rs:160-185`). So
/// a user with `filters` gets ours as `filters` keys (merged by key), and
/// anyone else gets one `exclude` array holding theirs and ours. Patterns are
/// case-insensitive globs (`shell_environment_policy.rs:157-160`).
pub fn codex_shell_env_args(config_toml: Option<&str>) -> Vec<String> {
    let policy = config_toml
        .and_then(|text| toml::from_str::<toml::Table>(text).ok())
        .and_then(|t| t.get("shell_environment_policy").cloned());
    let policy = policy.as_ref().and_then(toml::Value::as_table);
    let names = CODEX_TOKEN_VARS.iter().chain(CODEX_SHELL_EXCLUDES);
    if let Some(filters) = policy
        .and_then(|p| p.get("filters"))
        .and_then(toml::Value::as_table)
    {
        // Codex refuses two filter keys that differ only in case
        // (`shell_environment_policy.rs:111-119`). A key the user already has
        // is set under the user's own spelling; one they already exclude is
        // left alone, and any other value is overridden (review Y5).
        return names
            .filter_map(
                |name| match filters.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)) {
                    Some((_, v)) if v.as_str() == Some("exclude") => None,
                    Some((k, _)) => Some(k.clone()),
                    None => Some((*name).to_string()),
                },
            )
            .flat_map(|name| {
                [
                    "-c".to_string(),
                    format!("shell_environment_policy.filters.{name}=\"exclude\""),
                ]
            })
            .collect();
    }
    let mut exclude: Vec<String> = policy
        .and_then(|p| p.get("exclude"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for name in names {
        if !exclude.iter().any(|e| e.eq_ignore_ascii_case(name)) {
            exclude.push((*name).to_string());
        }
    }
    let list = toml::Value::Array(exclude.into_iter().map(toml::Value::String).collect());
    vec![
        "-c".to_string(),
        format!("shell_environment_policy.exclude={list}"),
    ]
}

/// A TOML basic string. The endpoint is MonoCode's own loopback URL, but it
/// is quoted properly all the same.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn only_chats_get_telemetry() {
        let chat = s(&[
            "--output-format",
            "stream-json",
            "--input-format",
            "stream-json",
        ]);
        assert_eq!(telemetry_harness(Some("claude"), &chat), Some("claude"));
        let helper = s(&["--input-format", "stream-json", "--no-session-persistence"]);
        assert_eq!(telemetry_harness(Some("claude"), &helper), None);
        assert_eq!(telemetry_harness(Some("claude"), &s(&["-p", "hi"])), None);
        assert_eq!(
            telemetry_harness(Some("codex"), &s(&["app-server", "-c", "x=1"])),
            Some("codex")
        );
        assert_eq!(telemetry_harness(Some("codex"), &s(&["exec", "hi"])), None);
        assert_eq!(telemetry_harness(Some("cursor"), &s(&["acp"])), None);
        assert_eq!(telemetry_harness(None, &chat), None);
    }

    #[test]
    fn claude_env_carries_no_token() {
        let (set, remove) = claude_env("http://127.0.0.1:4318", false);
        let get = |k: &str| set.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("CLAUDE_CODE_ENABLE_TELEMETRY"), Some("1"));
        assert_eq!(
            get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            Some("http://127.0.0.1:4318")
        );
        assert_eq!(get("OTEL_LOG_TOOL_DETAILS"), Some("1"));
        assert_eq!(get("OTEL_LOG_USER_PROMPTS"), None);
        assert_eq!(remove, vec!["OTEL_LOG_USER_PROMPTS".to_string()]);
        let (set, remove) = claude_env("http://127.0.0.1:1", true);
        assert!(set
            .iter()
            .any(|(k, v)| k == "OTEL_LOG_USER_PROMPTS" && v == "1"));
        assert!(remove.is_empty());
        assert_eq!(
            claude_header_var("abc"),
            (
                "OTEL_EXPORTER_OTLP_HEADERS".to_string(),
                "x-monocode-token=abc".to_string()
            )
        );
    }

    #[test]
    fn codex_args_hold_no_token() {
        let args = codex_args("http://127.0.0.1:4318", false);
        assert_eq!(
            args,
            s(&[
                "-c",
                "otel.exporter={otlp-http={endpoint=\"http://127.0.0.1:4318/v1/logs\",protocol=\"json\"}}",
                "-c",
                "otel.trace_exporter=\"none\"",
                "-c",
                "otel.metrics_exporter=\"none\"",
                "-c",
                "otel.log_user_prompt=false",
            ])
        );
        // Each value parses as TOML, as Codex reads it.
        for flag in args.iter().skip(1).step_by(2) {
            let (key, value) = flag.split_once('=').unwrap();
            let parsed: toml::Table =
                toml::from_str(&format!("v = {value}")).unwrap_or_else(|e| panic!("{key}: {e}"));
            assert!(parsed.contains_key("v"));
        }
        assert!(codex_args("http://127.0.0.1:1", true)
            .contains(&"otel.log_user_prompt=true".to_string()));
        let env = codex_env("tok");
        assert!(env.iter().all(|(_, v)| v == "x-monocode-token=tok"));
    }

    #[test]
    fn users_own_export_is_left_alone() {
        let none = |_: &str| None;
        assert!(!has_own_env_telemetry(&none));
        let set =
            |k: &str| (k == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| "https://collector".to_string());
        assert!(has_own_env_telemetry(&set));
        let empty = |k: &str| (k == "CLAUDE_CODE_ENABLE_TELEMETRY").then(|| " ".to_string());
        assert!(!has_own_env_telemetry(&empty));

        assert!(!has_own_codex_otel(None));
        assert!(!has_own_codex_otel(Some("model = \"x\"\n")));
        assert!(!has_own_codex_otel(Some(
            "[otel]\nexporter = \"none\"\nmetrics_exporter = \"none\"\n"
        )));
        assert!(has_own_codex_otel(Some("[otel]\nexporter = { otlp-http = { endpoint = \"https://x/v1/logs\", protocol = \"binary\" } }\n")));
        assert!(has_own_codex_otel(Some(
            "[otel]\ntrace_exporter = \"statsig\"\n"
        )));
        assert!(has_own_codex_otel(Some("this is [not toml")));
    }

    #[test]
    fn codex_shell_commands_never_see_the_token() {
        // No policy of the user's: one exclude array with the four names and
        // the computer-use pattern (X2).
        let args = codex_shell_env_args(None);
        assert_eq!(
            args,
            s(&[
                "-c",
                "shell_environment_policy.exclude=[\"OTEL_EXPORTER_OTLP_HEADERS\", \"OTEL_EXPORTER_OTLP_LOGS_HEADERS\", \"OTEL_EXPORTER_OTLP_METRICS_HEADERS\", \"OTEL_EXPORTER_OTLP_TRACES_HEADERS\", \"MONOCODE_CU_*\"]",
            ])
        );
        let (_, value) = args[1].split_once('=').unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        assert_eq!(parsed["v"].as_array().unwrap().len(), 5);

        // The user's own exclude array is kept: ours replaces it whole.
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy]\nexclude = [\"AWS_*\", \"otel_exporter_otlp_headers\"]\n",
        ));
        let (_, value) = args[1].split_once('=').unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        let list: Vec<&str> = parsed["v"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(list[0], "AWS_*");
        assert_eq!(list.len(), 6);

        // The user's `filters` table: ours as its keys, never an array that
        // would drop it.
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy.filters]\n\"AWS_*\" = \"exclude\"\n",
        ));
        assert_eq!(args.len(), 10);
        assert!(args.iter().skip(1).step_by(2).all(|a| (a
            .starts_with("shell_environment_policy.filters.OTEL_EXPORTER_OTLP_")
            || a.starts_with("shell_environment_policy.filters.MONOCODE_CU_"))
            && a.ends_with("=\"exclude\"")));

        // A key the user has in another case is not added twice.
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy.filters]\n\"monocode_cu_*\" = \"exclude\"\n",
        ));
        assert_eq!(args.len(), 8);
    }

    #[test]
    fn a_users_include_filter_never_lets_a_token_back_in() {
        // Review Y5: a user filter naming our pattern as "include" was kept,
        // so the token reached Codex's shell. Ours overrides it, under the
        // user's own spelling so Codex sees one key.
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy.filters]\n\"monocode_cu_*\" = \"include\"\n",
        ));
        assert!(
            args.iter()
                .any(|a| a == "shell_environment_policy.filters.monocode_cu_*=\"exclude\""),
            "{args:?}"
        );
        // One the user already excludes is left alone.
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy.filters]\n\"MONOCODE_CU_*\" = \"exclude\"\n",
        ));
        assert!(
            !args.iter().any(|a| a.contains("MONOCODE_CU_*")),
            "{args:?}"
        );
    }

    #[test]
    fn codex_shell_commands_never_see_the_computer_use_token() {
        // Security review X2: the chat's computer-use server gets these
        // through `env_vars`; the agent's shell needs neither.
        let args = codex_shell_env_args(None);
        let (_, value) = args[1].split_once('=').unwrap();
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        let list: Vec<&str> = parsed["v"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(list.contains(&"MONOCODE_CU_*"), "{list:?}");
        // The app and control CLIs the agent runs in its shell read these.
        assert!(!list
            .iter()
            .any(|p| p.starts_with("MONOCODE_APP") || p.starts_with("MONOCODE_CONTROL")));
        let args = codex_shell_env_args(Some(
            "[shell_environment_policy.filters]\n\"AWS_*\" = \"exclude\"\n",
        ));
        assert!(args
            .iter()
            .any(|a| a == "shell_environment_policy.filters.MONOCODE_CU_*=\"exclude\""));
    }

    #[test]
    fn claude_token_goes_through_the_headers_helper_not_a_variable() {
        let (set, _) = claude_env("http://127.0.0.1:4318", false);
        assert!(set.iter().all(|(k, _)| !k.contains("HEADERS")));
        let get = |k: &str| set.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        // The helper runs only for the http protocols.
        assert_eq!(get("OTEL_EXPORTER_OTLP_PROTOCOL"), Some("http/json"));
        assert_eq!(
            headers_helper_script("abc"),
            "#!/bin/sh\nprintf '%s\\n' '{\"x-monocode-token\":\"abc\"}'\n"
        );
        let args = s(&[
            "--settings",
            r#"{"env":{"A":"1"}}"#,
            "--model",
            "m",
            "--settings",
            r#"{"env":{"B":"2"},"hooks":{}}"#,
        ]);
        let merged = settings_with_helper(&args, "/x y/h.sh").unwrap();
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["otelHeadersHelper"], "/x y/h.sh");
        assert_eq!(value["env"]["B"], "2");
        assert!(value["hooks"].is_object());
        assert!(settings_with_helper(&s(&["--model", "m"]), "/h").is_some());
        assert!(settings_with_helper(&s(&["--settings", "/a/file.json"]), "/h").is_none());
    }
}

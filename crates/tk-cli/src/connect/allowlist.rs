//! `tk connect <harness> --allowlist`: the managed fragment an admin puts
//! in the harness's managed file so that only this server (by command or
//! URL) is allowed. tk prints it and never writes it (case 65): a managed
//! file is root's, and the user who runs tk is not the admin of record.

use super::json::{self, Value};
use super::presets::Preset;
use super::spec::{ServerSpec, Transport};

fn managed_path_line(preset: &Preset, macos: bool) -> String {
    let os = if macos { "macos" } else { "linux" };
    let paths: Vec<String> = match preset.managed_files.as_ref().and_then(|m| m.get(os)) {
        Some(serde_json::Value::String(p)) => vec![p.clone()],
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    if paths.is_empty() {
        String::new()
    } else {
        format!(
            "# managed files for {} on this OS: {}\n",
            preset.name,
            paths.join(", ")
        )
    }
}

/// The fragment, with a comment naming the file it belongs in.
pub fn fragment(preset: &Preset, spec: &ServerSpec, macos: bool) -> Result<String, String> {
    let mut out = managed_path_line(preset, macos);
    match preset.id.as_str() {
        // Claude Code managed settings: `allowedMcpServers` entries match by
        // `serverName`, `serverCommand` (the exact argv) or `serverUrl`
        // (wildcards allowed). Sources in presets/claude-code.json.
        "claude-code" => {
            let entry = match &spec.transport {
                Transport::Stdio { command, args } => {
                    let mut argv = vec![command.clone()];
                    argv.extend(args.iter().cloned());
                    Value::object(vec![("serverCommand", Value::strings(&argv))])
                }
                Transport::Http { url, .. } => Value::object(vec![("serverUrl", Value::str(url))]),
            };
            let v = Value::object(vec![("allowedMcpServers", Value::Array(vec![entry]))]);
            out.push_str(
                "# merge into allowedMcpServers in managed-settings.json; with allowManagedMcpServersOnly: true\n# only the managed list applies. tk does not write it.\n",
            );
            out.push_str(&json::pretty(&v, "  ", ""));
            out.push('\n');
        }
        // Codex requirements.toml: an allowlisted server is pinned by its
        // name and identity. A stdio server is pinned by its executable and
        // every argument (`identity.command` as a matcher table): the string
        // form "doesn't inspect arguments" (Codex configuration reference,
        // `mcp_servers.<name>.identity.command`), so `command = "docker"`
        // let any docker-started server under that name through (Mac test
        // w1, M5). The table form came with openai/codex#29648, merged
        // 25 Jun 2026; Codex 0.143.0 (8 Jul) is the first release after it
        // (npm dates, inferred). Run on 0.161.0 and 0.163.0 with this
        // fragment in /etc/codex/requirements.toml: the entry with the same
        // arguments is enabled, the same name with other arguments and an
        // unlisted server are "disabled: requirements" (integration w1 fix
        // pass, `codex mcp list`).
        "codex" => {
            out.push_str("# add to requirements.toml (managed); tk does not write it\n");
            match &spec.transport {
                Transport::Stdio { command, args } => {
                    out.push_str("# needs Codex 0.143.0 or later (argument matchers)\n");
                    out.push_str(&format!(
                        "[mcp_servers.{}.identity.command]\nexecutable = {}\nargs = [\n",
                        spec.name,
                        json::quote(command)
                    ));
                    for a in args {
                        out.push_str(&format!(
                            "  {{ match = \"exact\", value = {} }},\n",
                            json::quote(a)
                        ));
                    }
                    out.push_str("]\n");
                }
                Transport::Http { url, .. } => {
                    out.push_str(&format!("[mcp_servers.{}.identity]\n", spec.name));
                    out.push_str(&format!("url = {}\n", json::quote(url)));
                }
            }
        }
        _ => {
            return Err(format!(
                "{}: no managed MCP allowlist file is known for this harness; nothing to print",
                preset.name
            ))
        }
    }
    Ok(out)
}

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
        // (wildcards allowed). Sources in contracts/harness-presets.json.
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
        // identity, a command or a URL. Sources in contracts/harness-presets.json.
        "codex" => {
            out.push_str("# add to requirements.toml (managed); tk does not write it\n");
            out.push_str(&format!("[mcp_servers.{}.identity]\n", spec.name));
            match &spec.transport {
                Transport::Stdio { command, .. } => {
                    out.push_str(&format!("command = {}\n", json::quote(command)));
                }
                Transport::Http { url, .. } => {
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

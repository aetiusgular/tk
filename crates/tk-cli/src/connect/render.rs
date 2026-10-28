//! Shapes one `ServerSpec` into one harness's entry. Key order follows what
//! the harness's own `mcp add` writes where it has one (recorded 27 Oct,
//! fixtures in `fixtures/recorded/`), so a dry run prints byte for byte
//! what that command would have written; elsewhere it follows the
//! harness's documented example (sources per row in `presets.json`).

use super::json::Value;
use super::presets::Preset;

use super::spec::{HeaderValue, HttpKind, ServerSpec, Transport};
use super::tomlfile::Field;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rendered {
    /// `value` goes at `preset.entry_path` + the server name.
    Json(Value),
    /// `[<container>.<name>]` with these fields.
    Toml {
        container: String,
        fields: Vec<(String, Field)>,
    },
    /// Printed only: a fragment to paste.
    Yaml(String),
}

/// `${VAR}` in the harness's own spelling (`presets.json` `envRef`, where
/// `VAR` is the placeholder).
fn env_ref(preset: &Preset, var: &str) -> Result<String, String> {
    let tpl = preset.env_ref.as_deref().ok_or_else(|| {
        format!(
            "{}: no known way to reference an environment variable; tk will not write a value",
            preset.name
        )
    })?;
    Ok(tpl.replace("VAR", var))
}

fn header_text(preset: &Preset, v: &HeaderValue) -> Result<String, String> {
    match v {
        HeaderValue::Literal(s) => Ok(s.clone()),
        HeaderValue::Ref { prefix, var } => {
            if preset.env_ref_whole_value && !prefix.is_empty() {
                return Err(format!(
                    "{} reads a variable only as a whole value; a header like `{prefix}${{{var}}}` cannot be written for it",
                    preset.name
                ));
            }
            Ok(format!("{prefix}{}", env_ref(preset, var)?))
        }
    }
}

fn env_object(preset: &Preset, spec: &ServerSpec) -> Result<Option<Value>, String> {
    if spec.env.is_empty() {
        return Ok(None);
    }
    let mut members = Vec::new();
    for e in &spec.env {
        members.push((e.key.clone(), Value::Str(env_ref(preset, &e.var)?)));
    }
    Ok(Some(Value::Object(members)))
}

fn headers_object(
    preset: &Preset,
    headers: &[super::spec::Header],
) -> Result<Option<Value>, String> {
    if headers.is_empty() {
        return Ok(None);
    }
    let mut members = Vec::new();
    for h in headers {
        members.push((h.name.clone(), Value::Str(header_text(preset, &h.value)?)));
    }
    Ok(Some(Value::Object(members)))
}

fn push_opt(members: &mut Vec<(String, Value)>, key: &str, v: Option<Value>) {
    if let Some(v) = v {
        members.push((key.to_string(), v));
    }
}

fn obj(members: Vec<(String, Value)>) -> Value {
    Value::Object(members)
}

fn s(key: &str, v: &str) -> (String, Value) {
    (key.to_string(), Value::str(v))
}

fn sse_refused(preset: &Preset) -> String {
    format!(
        "{} takes streamable HTTP only; this entry is SSE",
        preset.name
    )
}

/// Renders `spec` for `preset`.
pub fn render(preset: &Preset, spec: &ServerSpec) -> Result<Rendered, String> {
    let style = preset.entry_style.as_str();
    match (&spec.transport, style) {
        // Claude Code: `claude mcp add-json` writes the object as given; tk
        // gives `type` first, as `claude mcp add` does.
        (Transport::Stdio { command, args }, "claude-code") => {
            let mut m = vec![
                s("type", "stdio"),
                s("command", command),
                ("args".into(), Value::strings(args)),
            ];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "claude-code") => {
            let t = if *kind == HttpKind::Sse {
                "sse"
            } else {
                "http"
            };
            let mut m = vec![s("type", t), s("url", url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // Pi: `pi mcp add` order is command, args, env, exposure and url,
        // headers, exposure (recorded, Pi 1.4.0).
        (Transport::Stdio { command, args }, "pi") => {
            let mut m = vec![s("command", command), ("args".into(), Value::strings(args))];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            push_opt(
                &mut m,
                "exposure",
                spec.exposure_for(&preset.id).map(Value::str),
            );
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "pi") => {
            if *kind == HttpKind::Sse {
                return Err(sse_refused(preset));
            }
            let mut m = vec![s("url", url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            push_opt(
                &mut m,
                "exposure",
                spec.exposure_for(&preset.id).map(Value::str),
            );
            Ok(Rendered::Json(obj(m)))
        }
        // Codex: `codex mcp add` writes command and args, or url and
        // bearer_token_env_var (recorded, 0.163.0). Environment variables
        // pass through by name (`env_vars`), because a literal `env` value
        // would put the secret in the file.
        (Transport::Stdio { command, args }, "codex") => {
            let mut f = vec![
                ("command".to_string(), Field::Str(command.clone())),
                ("args".to_string(), Field::Strings(args.clone())),
            ];
            if !spec.env.is_empty() {
                if spec.env.iter().any(|e| e.key != e.var) {
                    return Err(
                        "Codex passes variables through by name only; this entry renames one"
                            .into(),
                    );
                }
                f.push((
                    "env_vars".to_string(),
                    Field::Strings(spec.env.iter().map(|e| e.var.clone()).collect()),
                ));
            }
            Ok(Rendered::Toml {
                container: preset.entry_path[0].clone(),
                fields: f,
            })
        }
        (Transport::Http { kind, url, headers }, "codex") => {
            if *kind == HttpKind::Sse {
                return Err(sse_refused(preset));
            }
            let mut f = vec![("url".to_string(), Field::Str(url.clone()))];
            let mut literal = Vec::new();
            let mut from_env = Vec::new();
            for h in headers {
                match &h.value {
                    HeaderValue::Ref { prefix, var } if prefix == "Bearer " && h.name.eq_ignore_ascii_case("authorization") => {
                        f.push(("bearer_token_env_var".to_string(), Field::Str(var.clone())));
                    }
                    HeaderValue::Ref { prefix, var } if prefix.is_empty() => from_env.push((h.name.clone(), var.clone())),
                    HeaderValue::Ref { .. } => {
                        return Err(format!(
                            "Codex cannot send header `{}` as text plus a variable; only `Bearer ${{VAR}}` or a whole variable",
                            h.name
                        ))
                    }
                    HeaderValue::Literal(v) => literal.push((h.name.clone(), v.clone())),
                }
            }
            if !literal.is_empty() {
                f.push(("http_headers".to_string(), Field::Table(literal)));
            }
            if !from_env.is_empty() {
                f.push(("env_http_headers".to_string(), Field::Table(from_env)));
            }
            Ok(Rendered::Toml {
                container: preset.entry_path[0].clone(),
                fields: f,
            })
        }
        // Gemini CLI: `gemini mcp add` writes command, args, env and url,
        // type, headers (recorded, 0.65.0).
        (Transport::Stdio { command, args }, "gemini")
        | (Transport::Stdio { command, args }, "qwen-code") => {
            let mut m = vec![s("command", command), ("args".into(), Value::strings(args))];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "gemini") => {
            let mut m = vec![
                s("url", url),
                s(
                    "type",
                    if *kind == HttpKind::Sse {
                        "sse"
                    } else {
                        "http"
                    },
                ),
            ];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // Qwen Code: `httpUrl` for streamable HTTP, `url` for SSE
        // (recorded, 0.26.1).
        (Transport::Http { kind, url, headers }, "qwen-code") => {
            let key = if *kind == HttpKind::Sse {
                "url"
            } else {
                "httpUrl"
            };
            let mut m = vec![s(key, url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // Copilot CLI: tools, type, command, args, env and tools, type,
        // url, headers (recorded, 1.0.104).
        (Transport::Stdio { command, args }, "copilot-cli") => {
            let mut m = vec![
                ("tools".into(), Value::strings(&["*".to_string()])),
                s("type", "local"),
                s("command", command),
                ("args".into(), Value::strings(args)),
            ];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "copilot-cli") => {
            let mut m = vec![
                ("tools".into(), Value::strings(&["*".to_string()])),
                s(
                    "type",
                    if *kind == HttpKind::Sse {
                        "sse"
                    } else {
                        "http"
                    },
                ),
                s("url", url),
            ];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // OpenCode: `type: local` with the command as one array and
        // `environment`, or `type: remote` with `url` and `headers`.
        (Transport::Stdio { command, args }, "opencode") => {
            let mut cmd = vec![command.clone()];
            cmd.extend(args.iter().cloned());
            let mut m = vec![s("type", "local"), ("command".into(), Value::strings(&cmd))];
            push_opt(&mut m, "environment", env_object(preset, spec)?);
            m.push(("enabled".into(), Value::Bool(true)));
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "opencode") => {
            if *kind == HttpKind::Sse {
                return Err(sse_refused(preset));
            }
            let mut m = vec![s("type", "remote"), s("url", url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            m.push(("enabled".into(), Value::Bool(true)));
            Ok(Rendered::Json(obj(m)))
        }
        // Cursor: `type`, `command`, `args`, `env` (the field table marks
        // `type` required, the examples omit it; tk writes it); remote is
        // `url` and `headers`, transport auto-detected.
        (Transport::Stdio { command, args }, "cursor") => {
            let mut m = vec![
                s("type", "stdio"),
                s("command", command),
                ("args".into(), Value::strings(args)),
            ];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        // Zed: flat and untagged; no env-reference syntax was found, so an
        // entry that needs one is refused by `env_ref` (no `envRef`).
        (Transport::Stdio { command, args }, "zed")
        | (Transport::Stdio { command, args }, "amp") => {
            let mut m = vec![s("command", command), ("args".into(), Value::strings(args))];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { url, headers, .. }, "cursor")
        | (Transport::Http { url, headers, .. }, "zed") => {
            let mut m = vec![s("url", url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // Amp: the schema allows `url`, `headers`, `transport` and nothing
        // else on an HTTP entry (no `type`).
        (Transport::Http { kind, url, headers }, "amp") => {
            let mut m = vec![s("url", url)];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            m.push(s(
                "transport",
                if *kind == HttpKind::Sse {
                    "sse"
                } else {
                    "http"
                },
            ));
            Ok(Rendered::Json(obj(m)))
        }
        // Crush (crush.json): `type` is required.
        (Transport::Stdio { command, args }, "crush") => {
            let mut m = vec![
                s("type", "stdio"),
                s("command", command),
                ("args".into(), Value::strings(args)),
            ];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(obj(m)))
        }
        (Transport::Http { kind, url, headers }, "crush") => {
            let mut m = vec![
                s(
                    "type",
                    if *kind == HttpKind::Sse {
                        "sse"
                    } else {
                        "http"
                    },
                ),
                s("url", url),
            ];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(obj(m)))
        }
        // OpenClaw (printed): a bare `url` means SSE there, so streamable
        // HTTP is named explicitly.
        (Transport::Stdio { command, args }, "openclaw") => {
            let mut m = vec![s("command", command), ("args".into(), Value::strings(args))];
            push_opt(&mut m, "env", env_object(preset, spec)?);
            Ok(Rendered::Json(wrap(&preset.entry_path, &spec.name, obj(m))))
        }
        (Transport::Http { kind, url, headers }, "openclaw") => {
            let mut m = vec![
                s("url", url),
                s(
                    "transport",
                    if *kind == HttpKind::Sse {
                        "sse"
                    } else {
                        "streamable-http"
                    },
                ),
            ];
            push_opt(&mut m, "headers", headers_object(preset, headers)?);
            Ok(Rendered::Json(wrap(&preset.entry_path, &spec.name, obj(m))))
        }
        (_, "hermes") => Ok(Rendered::Yaml(hermes_yaml(preset, spec)?)),
        (_, "goose") => Ok(Rendered::Yaml(goose_yaml(spec)?)),
        _ => Err(format!(
            "{}: no renderer for entry style `{style}`",
            preset.name
        )),
    }
}

/// `{a: {b: {name: entry}}}` for printing a whole fragment.
fn wrap(path: &[String], name: &str, entry: Value) -> Value {
    let mut v = Value::Object(vec![(name.to_string(), entry)]);
    for seg in path.iter().rev() {
        v = Value::Object(vec![(seg.clone(), v)]);
    }
    v
}

/// A YAML double-quoted scalar. JSON's escapes are valid in YAML's
/// double-quoted style.
fn yq(s: &str) -> String {
    super::json::quote(s)
}

fn yaml_list(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|i| yq(i)).collect::<Vec<_>>().join(", ")
    )
}

/// Hermes: `mcp_servers.<name>` with `command`, `args`, `env`, or `url`,
/// `headers` and `transport: sse` for SSE (hermes-agent v2026.9.24,
/// `website/docs/reference/mcp-config-reference.md`).
fn hermes_yaml(preset: &Preset, spec: &ServerSpec) -> Result<String, String> {
    let mut out = format!("mcp_servers:\n  {}:\n", spec.name);
    match &spec.transport {
        Transport::Stdio { command, args } => {
            out.push_str(&format!(
                "    command: {}\n    args: {}\n",
                yq(command),
                yaml_list(args)
            ));
            if !spec.env.is_empty() {
                out.push_str("    env:\n");
                for e in &spec.env {
                    out.push_str(&format!(
                        "      {}: {}\n",
                        e.key,
                        yq(&env_ref(preset, &e.var)?)
                    ));
                }
            }
        }
        Transport::Http { kind, url, headers } => {
            out.push_str(&format!("    url: {}\n", yq(url)));
            if *kind == HttpKind::Sse {
                out.push_str("    transport: sse\n");
            }
            if !headers.is_empty() {
                out.push_str("    headers:\n");
                for h in headers {
                    out.push_str(&format!(
                        "      {}: {}\n",
                        h.name,
                        yq(&header_text(preset, &h.value)?)
                    ));
                }
            }
        }
    }
    out.push_str("    enabled: true\n");
    Ok(out)
}

/// goose: `extensions.<key>` with `type: stdio`, `cmd`, `args`, `envs`,
/// `env_keys`, or `type: streamable_http` with `uri` (goose v1.56.0,
/// `crates/goose/src/agents/extension.rs#L156-L258`). Variables reach a
/// server through `env_keys` (read from the process environment or goose's
/// secret store) and are substituted into `uri` and header values only.
/// goose has no SSE variant.
fn goose_yaml(spec: &ServerSpec) -> Result<String, String> {
    let mut out = format!("extensions:\n  {}:\n    enabled: true\n", spec.name);
    let vars = spec.variables();
    match &spec.transport {
        Transport::Stdio { command, args } => {
            if spec.env.iter().any(|e| e.key != e.var) {
                return Err(
                    "goose passes variables through by name only; this entry renames one".into(),
                );
            }
            out.push_str(&format!(
                "    type: stdio\n    name: {}\n    cmd: {}\n    args: {}\n",
                yq(&spec.name),
                yq(command),
                yaml_list(args)
            ));
        }
        Transport::Http { kind, url, headers } => {
            if *kind == HttpKind::Sse {
                return Err("goose has no SSE transport (v1.56.0); this entry is SSE".into());
            }
            out.push_str(&format!(
                "    type: streamable_http\n    name: {}\n    uri: {}\n",
                yq(&spec.name),
                yq(url)
            ));
            if !headers.is_empty() {
                out.push_str("    headers:\n");
                for h in headers {
                    let v = match &h.value {
                        HeaderValue::Literal(l) => l.clone(),
                        HeaderValue::Ref { prefix, var } => format!("{prefix}${{{var}}}"),
                    };
                    out.push_str(&format!("      {}: {}\n", h.name, yq(&v)));
                }
            }
        }
    }
    if !vars.is_empty() {
        out.push_str(&format!("    env_keys: {}\n", yaml_list(&vars)));
    }
    out.push_str("    timeout: 300\n");
    Ok(out)
}

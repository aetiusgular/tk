//! Whether tk's gate is registered for an agent whose gate is built, read
//! from the agent's own user-level files. Nothing here runs the agent, and
//! nothing here writes. What each check reads, at the version in the
//! agent's preset row (lane J status doc, Research):
//!
//! - Claude Code 2.1.311: `enabledPlugins["tk@tk"]` in
//!   `<config>/settings.json`, where `<config>` is `CLAUDE_CONFIG_DIR` or
//!   `~/.claude`; a plugin with no entry is enabled by default, so the
//!   install record `<plugins>/installed_plugins.json` (`plugins.tk@tk`,
//!   `<plugins>` = `CLAUDE_CODE_PLUGIN_CACHE_DIR` or `<config>/plugins`)
//!   decides then. A project, local or managed settings file can still turn
//!   it off; this reads only the user file.
//! - Codex 0.163.0: `[plugins."tk@tk"]` in `$CODEX_HOME/config.toml`
//!   (`enabled` missing means true; `[plugins._default] enabled = false`
//!   turns unlisted plugins off), the installed copy
//!   `$CODEX_HOME/plugins/cache/tk/tk/<version>/`, and the two hook trust
//!   records `[hooks.state."tk@tk:hooks/hooks.json:<event>:0:0"]`. Codex
//!   skips an untrusted plugin hook without a word (rules case 70).
//! - Pi 1.4.0: an entry of `packages` in `<agent>/settings.json`
//!   (`PI_CODING_AGENT_DIR` or `~/.pi/agent`), a string or `{source}`;
//!   `pi install /abs/path` stores the path relative to `<agent>`, so each
//!   local entry is resolved from there and its `package.json` read.
//! - OpenCode 1.19.4: `plugin` in `config.json`, `opencode.json` or
//!   `opencode.jsonc` under `$XDG_CONFIG_HOME/opencode` (default
//!   `~/.config/opencode`), each entry a string or `[string, options]`; or a
//!   file in its `plugin/` or `plugins/` folder. A bare name is an npm
//!   package to OpenCode, which fetches `<name>@latest`; `tk-opencode-plugin`
//!   is not published, so only a path loads.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::status::Env;

/// Files read here are config files; anything bigger is not one.
const MAX_FILE: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registered {
    Yes(String),
    No(String),
    Unknown(String),
    NotBuilt,
}

impl Registered {
    pub fn word(&self) -> &'static str {
        match self {
            Registered::Yes(_) => "yes",
            Registered::No(_) => "no",
            Registered::Unknown(_) => "unknown",
            Registered::NotBuilt => "n/a",
        }
    }

    pub fn detail(&self) -> Option<String> {
        match self {
            Registered::Yes(d) | Registered::No(d) | Registered::Unknown(d) => Some(d.clone()),
            Registered::NotBuilt => None,
        }
    }
}

pub fn check(id: &str, env: &Env) -> Registered {
    let Some(home) = env.home.as_deref() else {
        return Registered::Unknown("HOME is not set".into());
    };
    match id {
        "claude-code" => claude(env, home),
        "codex" => codex(env, home),
        "pi" => pi(env, home),
        "opencode" => opencode(env, home),
        other => Registered::Unknown(format!("no registration check for {other} yet")),
    }
}

/// Reads a regular file of at most `MAX_FILE` bytes. `Ok(None)` when it
/// does not exist.
fn read(path: &Path) -> Result<Option<String>, String> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > MAX_FILE {
        return Err(format!("{} is over 1 MiB", path.display()));
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn read_json(path: &Path, jsonc: bool) -> Result<Option<Value>, String> {
    let Some(text) = read(path)? else {
        return Ok(None);
    };
    let text = if jsonc { strip_jsonc(&text) } else { text };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} does not parse: {e}", path.display()))
}

fn tilde(p: &str, home: &Path) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if p == "~" => home.to_path_buf(),
        None => PathBuf::from(p),
    }
}

fn claude(env: &Env, home: &Path) -> Registered {
    let config = env
        .var("CLAUDE_CONFIG_DIR")
        .map(|d| tilde(d, home))
        .unwrap_or_else(|| home.join(".claude"));
    let settings = config.join("settings.json");
    let enabled = match read_json(&settings, false) {
        Ok(v) => v.and_then(|v| {
            v.get("enabledPlugins")
                .and_then(|e| e.get("tk@tk"))
                .cloned()
        }),
        Err(e) => return Registered::Unknown(e),
    };
    if enabled == Some(Value::Bool(false)) {
        return Registered::No(format!(
            "tk@tk is turned off in {} (enabledPlugins)",
            settings.display()
        ));
    }
    let plugins = env
        .var("CLAUDE_CODE_PLUGIN_CACHE_DIR")
        .map(|d| tilde(d, home))
        .unwrap_or_else(|| config.join("plugins"));
    let installed = plugins.join("installed_plugins.json");
    let record = match read_json(&installed, false) {
        Ok(v) => v.and_then(|v| v.get("plugins").and_then(|p| p.get("tk@tk")).cloned()),
        Err(e) => return Registered::Unknown(e),
    };
    // Claude Code uses a `project` or `local` install record only when its
    // projectPath is the folder it started in, or that folder by its real
    // path; `user` and `managed` records apply everywhere (2.1.311's binary,
    // function `WO`: `if(e.scope==="user"||e.scope==="managed")return!0;let
    // n=ke();if(e.projectPath===n)return!0;...zM(e.projectPath)===r`, where
    // `ke()` is the session's original working folder; Mac test w2, MW3).
    // A record list with scoped entries and none that applies here is not a
    // registration in this folder.
    if let Some(Value::Array(installs)) = &record {
        let scoped: Vec<&str> = installs
            .iter()
            .filter(|r| {
                matches!(
                    r.get("scope").and_then(Value::as_str),
                    Some("project" | "local")
                )
            })
            .filter_map(|r| r.get("projectPath").and_then(Value::as_str))
            .collect();
        let anywhere = installs.iter().any(|r| {
            matches!(
                r.get("scope").and_then(Value::as_str),
                Some("user" | "managed")
            )
        });
        let here = scoped
            .iter()
            .any(|p| same_folder(Path::new(p), env.cwd.as_deref()));
        if !anywhere && !scoped.is_empty() && !here {
            let first: String = scoped[0].chars().take(200).collect();
            return Registered::No(format!(
                "tk@tk is installed for {}{} only (project or local scope); Claude Code loads it only when started there, and tk status runs in {}",
                first,
                if scoped.len() > 1 {
                    format!(" and {} other folder(s)", scoped.len() - 1)
                } else {
                    String::new()
                },
                env.cwd
                    .as_deref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_else(|| "an unknown folder".into())
            ));
        }
    }
    match (enabled, record) {
        (Some(Value::Bool(true)), Some(_)) => Registered::Yes(format!(
            "tk@tk installed and on in {}; a project or managed settings file can still turn it off",
            settings.display()
        )),
        (Some(Value::Bool(true)), None) => Registered::No(format!(
            "tk@tk is on in {} but not installed ({} has no tk@tk): claude plugin install tk@tk",
            settings.display(),
            installed.display()
        )),
        (None, Some(_)) => Registered::Yes(format!(
            "tk@tk installed ({}); no enabledPlugins entry, which Claude Code reads as on",
            installed.display()
        )),
        (None, None) => Registered::No(
            "tk@tk is not installed: claude plugin marketplace add <tk clone>, then claude plugin install tk@tk".into(),
        ),
        (Some(other), _) => Registered::Unknown(format!(
            "enabledPlugins[\"tk@tk\"] in {} is {other}, not true or false",
            settings.display()
        )),
    }
}

/// Whether `recorded` names the folder `cwd`, as written or by real path.
fn same_folder(recorded: &Path, cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    if recorded == cwd {
        return true;
    }
    match (std::fs::canonicalize(recorded), std::fs::canonicalize(cwd)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Codex's hook trust keys for tk's plugin (`tests/cli-contract/checks/tkCodex.mjs`).
pub const CODEX_HOOK_KEYS: [&str; 2] = [
    "tk@tk:hooks/hooks.json:pre_tool_use:0:0",
    "tk@tk:hooks/hooks.json:post_tool_use:0:0",
];

fn codex(env: &Env, home: &Path) -> Registered {
    let codex_home = env
        .var("CODEX_HOME")
        .map(|d| tilde(d, home))
        .unwrap_or_else(|| home.join(".codex"));
    let config = codex_home.join("config.toml");
    let text = match read(&config) {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Registered::No(format!(
                "no {}: codex plugin marketplace add <tk clone>, then codex plugin add tk@tk",
                config.display()
            ))
        }
        Err(e) => return Registered::Unknown(e),
    };
    let doc: toml_edit::DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(_) => {
            return Registered::Unknown(format!("{} does not parse as TOML", config.display()))
        }
    };
    let plugins = doc.get("plugins").and_then(|p| p.as_table_like());
    let entry = plugins
        .and_then(|p| p.get("tk@tk"))
        .and_then(|e| e.as_table_like());
    let enabled_of = |t: Option<&dyn toml_edit::TableLike>| {
        t.and_then(|t| t.get("enabled")).and_then(|v| v.as_bool())
    };
    let on = match entry {
        Some(e) => enabled_of(Some(e)).unwrap_or(true),
        None => {
            return Registered::No(format!(
                "{} has no [plugins.\"tk@tk\"]: codex plugin add tk@tk",
                config.display()
            ))
        }
    };
    if !on {
        return Registered::No(format!("tk@tk is turned off in {}", config.display()));
    }
    let cache = codex_home.join("plugins/cache/tk/tk");
    let installed = std::fs::read_dir(&cache)
        .map(|rd| rd.flatten().any(|e| e.path().is_dir()))
        .unwrap_or(false);
    if !installed {
        return Registered::No(format!(
            "tk@tk is listed but no copy is in {}; Codex reports it as not installed",
            cache.display()
        ));
    }
    let hooks = doc
        .get("hooks")
        .and_then(|h| h.as_table_like())
        .and_then(|h| h.get("state"))
        .and_then(|s| s.as_table_like());
    let trusted: Vec<&str> = CODEX_HOOK_KEYS
        .iter()
        .copied()
        .filter(|k| {
            hooks
                .and_then(|s| s.get(k))
                .and_then(|t| t.as_table_like())
                .and_then(|t| t.get("trusted_hash"))
                .and_then(|v| v.as_str())
                .is_some_and(|h| h.starts_with("sha256:"))
        })
        .collect();
    if trusted.len() < CODEX_HOOK_KEYS.len() {
        return Registered::No(format!(
            "tk@tk is installed but {} of its 2 hooks have a trust record; Codex skips an untrusted hook without a word: trust them in /hooks",
            trusted.len()
        ));
    }
    Registered::Yes(format!(
        "tk@tk installed, on and both hooks trusted in {}; whether each trusted hash still matches hooks.json is not checked here (Codex re-asks when it changes)",
        config.display()
    ))
}

fn pi(env: &Env, home: &Path) -> Registered {
    let agent = env
        .var("PI_CODING_AGENT_DIR")
        .map(|d| tilde(d, home))
        .unwrap_or_else(|| home.join(".pi/agent"));
    let settings = agent.join("settings.json");
    let v = match read_json(&settings, false) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Registered::No(format!(
                "no {}: pi install /abs/path/to/tk/packages/tk-pi",
                settings.display()
            ))
        }
        Err(e) => return Registered::Unknown(e),
    };
    let entries = v
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for e in &entries {
        let source = match e {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("source").and_then(Value::as_str).unwrap_or(""),
            _ => "",
        };
        if source == "npm:tk-pi" || source.starts_with("npm:tk-pi@") {
            return Registered::Yes(format!("{source} in {}", settings.display()));
        }
        if source.contains(':') && !source.starts_with('/') {
            continue; // git:, npm: of another package, a URL
        }
        let path = if Path::new(source).is_absolute() {
            PathBuf::from(source)
        } else {
            agent.join(tilde(source, home))
        };
        let name = read_json(&path.join("package.json"), false)
            .ok()
            .flatten()
            .and_then(|p| p.get("name").and_then(Value::as_str).map(str::to_string));
        if name.as_deref() == Some("tk-pi") {
            return Registered::Yes(format!(
                "{} (from {}) in {}",
                path.display(),
                source,
                settings.display()
            ));
        }
    }
    Registered::No(format!(
        "no tk-pi in packages of {}: pi install /abs/path/to/tk/packages/tk-pi",
        settings.display()
    ))
}

fn opencode(env: &Env, home: &Path) -> Registered {
    let base = env
        .var("XDG_CONFIG_HOME")
        .map(|d| tilde(d, home))
        .unwrap_or_else(|| home.join(".config"))
        .join("opencode");
    let mut bare = None;
    for (file, jsonc) in [
        ("config.json", false),
        ("opencode.json", true),
        ("opencode.jsonc", true),
    ] {
        let path = base.join(file);
        let v = match read_json(&path, jsonc) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(e) => return Registered::Unknown(e),
        };
        for e in v
            .get("plugin")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let spec = match &e {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.first().and_then(Value::as_str).unwrap_or("").to_string(),
                _ => continue,
            };
            let local = spec.starts_with('/')
                || spec.starts_with("./")
                || spec.starts_with("file://")
                || spec.starts_with("~/");
            if local {
                let p = spec.strip_prefix("file://").unwrap_or(&spec);
                let p = if p.starts_with("./") {
                    base.join(p)
                } else {
                    tilde(p, home)
                };
                if p.to_string_lossy().contains("tk-opencode-plugin") {
                    return Registered::Yes(format!("{} in {}", p.display(), path.display()));
                }
            } else if spec == "tk-opencode-plugin" || spec.starts_with("tk-opencode-plugin@") {
                bare = Some(path.clone());
            }
        }
    }
    for dir in ["plugin", "plugins"] {
        if let Ok(rd) = std::fs::read_dir(base.join(dir)) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with("tk") && (n.ends_with(".ts") || n.ends_with(".js")) {
                    return Registered::Yes(format!("{} (plugin folder)", e.path().display()));
                }
            }
        }
    }
    match bare {
        Some(p) => Registered::No(format!(
            "{} names tk-opencode-plugin by npm name; OpenCode fetches it from npm, where it is not published, so it does not load: name the folder's path instead",
            p.display()
        )),
        None => Registered::No(format!(
            "no tk plugin in {}: add the plugin's path to `plugin` (opencode.fragment.json)",
            base.display()
        )),
    }
}

/// JSONC to JSON: drops `//` and `/* */` comments outside strings, then a
/// comma before `}` or `]`.
pub fn strip_jsonc(text: &str) -> String {
    let no_comments = scan(text, |b, i, out| {
        if b[i] == '/' && b.get(i + 1) == Some(&'/') {
            let mut j = i;
            while j < b.len() && b[j] != '\n' {
                j += 1;
            }
            return Some(j);
        }
        if b[i] == '/' && b.get(i + 1) == Some(&'*') {
            let mut j = i + 2;
            while j + 1 < b.len() && !(b[j] == '*' && b[j + 1] == '/') {
                j += 1;
            }
            out.push(' ');
            return Some((j + 2).min(b.len()));
        }
        None
    });
    scan(&no_comments, |b, i, _| {
        if b[i] != ',' {
            return None;
        }
        let mut j = i + 1;
        while j < b.len() && b[j].is_whitespace() {
            j += 1;
        }
        matches!(b.get(j), Some('}') | Some(']')).then_some(i + 1)
    })
}

/// Copies `text`, leaving strings alone; outside strings `skip` may consume
/// from `i` (returning where to go on) or decline (`None`).
fn scan(text: &str, mut skip: impl FnMut(&[char], usize, &mut String) -> Option<usize>) -> String {
    let b: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            in_str = c != '"';
            i += 1;
        } else if c == '"' {
            in_str = true;
            out.push(c);
            i += 1;
        } else if let Some(next) = skip(&b, i, &mut out) {
            i = next;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home(name: &str) -> (Home, Env) {
        let dir = std::env::temp_dir().join(format!("tk-reg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let env = Env {
            home: Some(dir.clone()),
            path: String::new(),
            vars: Vec::new(),
            cwd: None,
        };
        (Home(dir), env)
    }

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn claude_code_needs_the_install_record_and_honours_a_turned_off_entry() {
        let (h, env) = home("claude");
        assert_eq!(check("claude-code", &env).word(), "no");
        put(
            &h.0.join(".claude/plugins/installed_plugins.json"),
            r#"{"version":2,"plugins":{"tk@tk":[{"scope":"user","installPath":"/x","version":"0.0.0"}]}}"#,
        );
        assert_eq!(
            check("claude-code", &env).word(),
            "yes",
            "no entry reads as on"
        );
        put(
            &h.0.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"tk@tk":false}}"#,
        );
        assert_eq!(check("claude-code", &env).word(), "no");
        put(
            &h.0.join(".claude/settings.json"),
            r#"{"enabledPlugins":{"tk@tk":true}}"#,
        );
        assert_eq!(check("claude-code", &env).word(), "yes");
    }

    #[test]
    fn a_local_install_counts_only_in_its_own_folder() {
        // Mac test w2, MW3: the only record was `scope: "local"` for one
        // project, and `tk status` said "yes" in /tmp and ~ too. Claude Code
        // 2.1.311 uses a project or local record only when its projectPath is
        // the folder it started in, or the same folder by its real path (its
        // binary, function `WO`; read in fix pass 1).
        let (h, mut env) = home("claudelocal");
        let project = h.0.join("scratch");
        std::fs::create_dir_all(&project).unwrap();
        put(
            &h.0.join(".claude/plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"tk@tk":[{{"scope":"local","projectPath":"{}","installPath":"/x","version":"0.0.0"}}]}}}}"#,
                project.display()
            ),
        );
        env.cwd = Some(h.0.clone());
        let elsewhere = check("claude-code", &env);
        assert_eq!(elsewhere.word(), "no", "{elsewhere:?}");
        assert!(
            elsewhere.detail().unwrap().contains("installed for"),
            "{elsewhere:?}"
        );
        env.cwd = Some(project.clone());
        assert_eq!(check("claude-code", &env).word(), "yes");
        // The same folder through a link counts too.
        let link = h.0.join("via-link");
        std::os::unix::fs::symlink(&project, &link).unwrap();
        env.cwd = Some(link);
        assert_eq!(check("claude-code", &env).word(), "yes");
        // A user-scope record beside it counts everywhere.
        put(
            &h.0.join(".claude/plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"tk@tk":[{{"scope":"local","projectPath":"{}","installPath":"/x"}},{{"scope":"user","installPath":"/y"}}]}}}}"#,
                project.display()
            ),
        );
        env.cwd = Some(h.0.clone());
        assert_eq!(check("claude-code", &env).word(), "yes");
    }

    #[test]
    fn claude_config_dir_moves_the_files() {
        let (h, mut env) = home("claudedir");
        let other = h.0.join("elsewhere");
        put(
            &other.join("plugins/installed_plugins.json"),
            r#"{"plugins":{"tk@tk":[]}}"#,
        );
        assert_eq!(check("claude-code", &env).word(), "no");
        env.vars
            .push(("CLAUDE_CONFIG_DIR".into(), other.display().to_string()));
        assert_eq!(check("claude-code", &env).word(), "yes");
    }

    #[test]
    fn codex_needs_the_entry_the_copy_and_both_trust_records() {
        let (h, env) = home("codex");
        let cfg = h.0.join(".codex/config.toml");
        assert_eq!(check("codex", &env).word(), "no");
        put(&cfg, "[plugins.\"tk@tk\"]\nenabled = true\n");
        let r = check("codex", &env);
        assert!(r.detail().unwrap().contains("no copy"), "{r:?}");
        std::fs::create_dir_all(h.0.join(".codex/plugins/cache/tk/tk/0.0.0")).unwrap();
        let r = check("codex", &env);
        assert!(r.detail().unwrap().contains("0 of its 2 hooks"), "{r:?}");
        put(&cfg, "[plugins.\"tk@tk\"]\nenabled = true\n\n[hooks.state.\"tk@tk:hooks/hooks.json:pre_tool_use:0:0\"]\ntrusted_hash = \"sha256:aa\"\n\n[hooks.state.\"tk@tk:hooks/hooks.json:post_tool_use:0:0\"]\ntrusted_hash = \"sha256:bb\"\n");
        assert_eq!(check("codex", &env).word(), "yes");
        put(&cfg, "[plugins.\"tk@tk\"]\nenabled = false\n");
        assert_eq!(check("codex", &env).word(), "no");
        put(&cfg, "not = [toml");
        assert_eq!(check("codex", &env).word(), "unknown");
    }

    #[test]
    fn pi_resolves_a_relative_package_path_from_the_agent_folder() {
        let (h, env) = home("pi");
        let pkg = h.0.join("src/tk/packages/tk-pi");
        put(&pkg.join("package.json"), r#"{"name":"tk-pi"}"#);
        let settings = h.0.join(".pi/agent/settings.json");
        put(
            &settings,
            r#"{"packages":["npm:other","../../src/tk/packages/tk-pi"]}"#,
        );
        assert_eq!(check("pi", &env).word(), "yes");
        put(
            &settings,
            r#"{"packages":[{"source":"../../src/tk/packages/tk-pi","extensions":[]}]}"#,
        );
        assert_eq!(check("pi", &env).word(), "yes");
        // A folder named tk-pi holding another package is not tk's.
        put(&pkg.join("package.json"), r#"{"name":"evil"}"#);
        assert_eq!(check("pi", &env).word(), "no");
        put(&settings, r#"{"packages":["npm:tk-pi@0.1.0"]}"#);
        assert_eq!(check("pi", &env).word(), "yes");
    }

    #[test]
    fn opencode_takes_a_path_and_says_why_the_npm_name_fails() {
        let (h, env) = home("opencode");
        let cfg = h.0.join(".config/opencode/opencode.json");
        put(
            &cfg,
            "{\n  // tk\n  \"plugin\": [\"tk-opencode-plugin\",],\n}\n",
        );
        let r = check("opencode", &env);
        assert_eq!(r.word(), "no");
        assert!(r.detail().unwrap().contains("not published"), "{r:?}");
        put(
            &cfg,
            r#"{"plugin": [["/home/u/tk/packages/tk-opencode-plugin", {}]]}"#,
        );
        assert_eq!(check("opencode", &env).word(), "yes");
        put(&cfg, "{}");
        put(
            &h.0.join(".config/opencode/plugins/tk.ts"),
            "export default {}",
        );
        assert_eq!(check("opencode", &env).word(), "yes");
    }

    #[test]
    fn a_huge_or_odd_file_is_not_read() {
        let (h, env) = home("big");
        let s = h.0.join(".pi/agent/settings.json");
        std::fs::create_dir_all(&s).unwrap(); // a folder where the file should be
        assert_eq!(check("pi", &env).word(), "unknown");
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_go_but_strings_stay() {
        let t = strip_jsonc("{\"a\": \"x//y /*z*/\", /* c */ \"b\": [1,2,], // end\n}");
        let v: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["a"], "x//y /*z*/");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }
}

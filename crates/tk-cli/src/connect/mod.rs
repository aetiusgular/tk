//! `tk connect` (lane C, T1.2): put one MCP server into one harness's own
//! config, or take it out, showing the exact change first.
//!
//! ```text
//! tk connect --list
//! tk connect <harness> --from <entry.json> [--name N] [--replace] [--accept-host]
//! tk connect <harness> --gateway docker --profile <id> [--url U [--bearer-env V]] [--check-profile]
//! tk connect <harness> --gateway toolhive --workload <w> [--url U]
//! tk connect <harness> --remove <name>
//! tk connect <harness> --allowlist (--from <entry.json> | --gateway ...)
//! tk connect <harness> --path
//! ... [--write [--expect-sha256 <hash>]]
//! ```
//!
//! Dry run by default: it prints the file, the runner, the variables to
//! export and the unified diff, and writes nothing. `--write` asks on the
//! terminal, re-reads the file and refuses if it changed since the diff
//! (case 55; `--expect-sha256` carries the hash across two invocations),
//! backs up, writes atomically, and records the write (`log.rs`). Only
//! user-level files are ever written: no flag reaches a project file or a
//! managed file (case 65), and a harness that is not installed gets the
//! entry printed and nothing written (case 54).
//!
//! Reads: `contracts/harness-presets.json` (lane C's verified copy is
//! `presets.json` here), `contracts/docker-args.md`,
//! `contracts/entry.schema.json`, `contracts/policy.md` section 3.

pub mod gateway;

mod allowlist;
mod diff;
mod json;
mod log;
mod presets;
mod render;
mod spec;
mod tomlfile;
mod write;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use presets::{Env, Format, Preset};
use render::Rendered;
use spec::{Origin, Runner, ServerSpec};
use write::Confirm;

/// Everything `run` needs from the outside, so tests can stand in a
/// scratch home, a fake environment and a scripted terminal.
pub struct Ctx<'a> {
    pub home: PathBuf,
    pub macos: bool,
    pub env: &'a dyn Env,
    pub confirm: &'a mut dyn Confirm,
    pub out: String,
    pub err: String,
    /// Print as it goes (the real command), so the diff is on screen
    /// before the terminal question; tests keep everything in `out`.
    pub live: bool,
}

impl Ctx<'_> {
    fn flush(&mut self) {
        if self.live {
            use std::io::Write;
            print!("{}", self.out);
            eprint!("{}", self.err);
            let _ = std::io::stdout().flush();
            self.out.clear();
            self.err.clear();
        }
    }
}

struct RealEnv;

impl Env for RealEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

pub fn main(args: &[String]) -> u8 {
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        eprintln!("tk connect: HOME is not set");
        return 1;
    };
    let mut tty = write::Tty;
    let mut ctx = Ctx {
        home: PathBuf::from(home),
        macos: cfg!(target_os = "macos"),
        env: &RealEnv,
        confirm: &mut tty,
        out: String::new(),
        err: String::new(),
        live: true,
    };
    let code = run(args, &mut ctx);
    print!("{}", ctx.out);
    eprint!("{}", ctx.err);
    code
}

const USAGE: &str = "usage: tk connect --list
       tk connect <harness> --from <entry.json> [--name N] [--replace] [--accept-host] [--write [--expect-sha256 H]]
       tk connect <harness> --gateway docker --profile <id> [--url U [--bearer-env VAR]] [--name N] [--check-profile] [--write ...]
       tk connect <harness> --gateway toolhive --workload <name> [--url U] [--name N]
       tk connect <harness> --remove <name> [--write ...]
       tk connect <harness> --allowlist (--from <entry.json> | --gateway ...)
       tk connect <harness> --path
Dry run unless --write. Writes user-level files only, after a terminal confirmation.
";

#[derive(Default, Debug)]
struct Opts {
    harness: Option<String>,
    from: Option<String>,
    gateway: Option<String>,
    profile: Option<String>,
    workload: Option<String>,
    url: Option<String>,
    bearer_env: Option<String>,
    name: Option<String>,
    remove: Option<String>,
    expect: Option<String>,
    write: bool,
    replace: bool,
    accept_host: bool,
    allowlist: bool,
    list: bool,
    path: bool,
    check_profile: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .filter(|v| !v.starts_with("--"))
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--list" => o.list = true,
            "--write" => o.write = true,
            "--replace" => o.replace = true,
            "--accept-host" => o.accept_host = true,
            "--allowlist" => o.allowlist = true,
            "--path" => o.path = true,
            "--check-profile" => o.check_profile = true,
            "--from" => o.from = Some(value(&mut i, a)?),
            "--gateway" => o.gateway = Some(value(&mut i, a)?),
            "--profile" => o.profile = Some(value(&mut i, a)?),
            "--workload" => o.workload = Some(value(&mut i, a)?),
            "--url" => o.url = Some(value(&mut i, a)?),
            "--bearer-env" => o.bearer_env = Some(value(&mut i, a)?),
            "--name" => o.name = Some(value(&mut i, a)?),
            "--remove" => o.remove = Some(value(&mut i, a)?),
            "--expect-sha256" => o.expect = Some(value(&mut i, a)?),
            "--scope" | "--project" | "-s" | "--local" | "-l" => {
                return Err("tk connect writes user-level files only; there is no project scope (an agent can write project files)".into())
            }
            "--yes" | "-y" => {
                return Err("there is no --yes: --write asks on the terminal, so a script or an agent cannot answer for you".into())
            }
            _ if a.starts_with('-') => return Err(format!("unknown option {a}")),
            _ => {
                if o.harness.is_some() {
                    return Err(format!("unexpected argument {a}"));
                }
                o.harness = Some(a.to_string());
            }
        }
        i += 1;
    }
    if let Some(h) = &o.expect {
        if h.len() != 64
            || !h
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("--expect-sha256 takes 64 lowercase hex characters".into());
        }
        if !o.write {
            return Err("--expect-sha256 goes with --write".into());
        }
    }
    Ok(o)
}

fn list(ctx: &mut Ctx) {
    ctx.out
        .push_str("harness        writes  user config (this OS)\n");
    for p in presets::table().harnesses {
        let path = p
            .user_path(&ctx.home, ctx.macos, ctx.env)
            .map_or("(none known)".to_string(), |p| p.display().to_string());
        let w = if p.writes() { "yes" } else { "print" };
        ctx.out
            .push_str(&format!("{:<14} {:<7} {}\n", p.id, w, path));
    }
}

/// Is the harness on this machine? Its config file, the folder that holds
/// it (when that is not the home folder itself), or its binary on PATH
/// (case 54). Looks only; runs nothing.
fn installed(preset: &Preset, path: &Path, home: &Path, env: &dyn Env) -> bool {
    if path.exists() {
        return true;
    }
    if let Some(dir) = path.parent() {
        if dir != home && dir.is_dir() {
            return true;
        }
    }
    let Some(cli) = &preset.cli else {
        return false;
    };
    env.var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| d.starts_with('/'))
        .any(|d| Path::new(d).join(cli).is_file())
}

fn origin_line(spec: &ServerSpec) -> String {
    match &spec.origin {
        Origin::Entry {
            file,
            sha256,
            registry_name,
            version,
        } => format!("entry {file} ({registry_name} {version}, sha256 {sha256})"),
        Origin::Gateway { backend, id } => format!("{backend} gateway, {id}"),
    }
}

fn spec_from(o: &Opts) -> Result<ServerSpec, String> {
    match (&o.from, o.gateway.as_deref()) {
        (Some(_), Some(_)) => {
            Err("--from and --gateway are two different sources; give one".into())
        }
        (Some(file), None) => {
            if o.url.is_some()
                || o.bearer_env.is_some()
                || o.profile.is_some()
                || o.workload.is_some()
            {
                return Err(
                    "--url, --bearer-env, --profile and --workload go with --gateway".into(),
                );
            }
            let p = Path::new(file);
            let meta = std::fs::symlink_metadata(p).map_err(|e| format!("{file}: {e}"))?;
            if !meta.is_file() || meta.len() > spec::MAX_ENTRY_BYTES as u64 {
                return Err(format!(
                    "{file}: not a regular file under {} bytes",
                    spec::MAX_ENTRY_BYTES
                ));
            }
            let bytes = std::fs::read(p).map_err(|e| format!("{file}: {e}"))?;
            spec::from_entry(file, &bytes, o.name.as_deref())
        }
        (None, Some("docker")) => {
            let profile = o
                .profile
                .as_deref()
                .ok_or("--gateway docker needs --profile <id>")?;
            gateway::docker_spec(
                profile,
                o.name.as_deref(),
                o.url.as_deref(),
                o.bearer_env.as_deref(),
            )
        }
        (None, Some("toolhive")) => {
            let w = o
                .workload
                .as_deref()
                .ok_or("--gateway toolhive needs --workload <name>")?;
            gateway::toolhive::toolhive_spec(w, o.name.as_deref(), o.url.as_deref())
        }
        (None, Some(other)) => Err(format!("unknown gateway `{other}` (docker or toolhive)")),
        (None, None) => Err(
            "say what to connect: --from <entry.json> or --gateway docker --profile <id>".into(),
        ),
    }
}

/// The text after the change, or why there is none.
enum Change {
    Text(String),
    Same(String),
}

const EXISTS: &str = "already exists with a different entry; --replace to overwrite it, or --name to add it under another name";

fn compute(
    preset: &Preset,
    current: &str,
    spec: Option<&ServerSpec>,
    remove: Option<&str>,
    replace: bool,
) -> Result<Change, String> {
    let path: Vec<&str> = preset.entry_path.iter().map(String::as_str).collect();
    let jsonc = preset.format == Format::Jsonc;
    let unparsable = |e: String| format!("{e}; tk will not rewrite a file it cannot parse");
    match (preset.format, spec, remove) {
        (Format::Json | Format::Jsonc, Some(spec), None) => {
            let Rendered::Json(value) = render::render(preset, spec)? else {
                return Err("internal: renderer gave a non-JSON entry for a JSON file".into());
            };
            let doc = json::parse(current, jsonc).map_err(unparsable)?;
            let new =
                match doc.set_member(&path, &spec.name, &value, replace, preset.trailing_newline) {
                    Ok(t) => t,
                    Err(json::EditError::Exists { same: true }) => {
                        return Ok(Change::Same(format!(
                            "`{}` is already connected with exactly this entry; nothing to change",
                            spec.name
                        )))
                    }
                    Err(json::EditError::Exists { same: false }) => {
                        return Err(format!("`{}` {EXISTS}", spec.name))
                    }
                    Err(json::EditError::NotObject(p)) => {
                        return Err(format!(
                            "`{p}` in the file is not an object; tk will not guess"
                        ))
                    }
                    Err(json::EditError::Missing) => return Err("internal: missing".into()),
                };
            verify_json(current, &new, jsonc, &path, &spec.name, Some(&value))?;
            Ok(Change::Text(new))
        }
        (Format::Json | Format::Jsonc, None, Some(name)) => {
            let doc = json::parse(current, jsonc).map_err(unparsable)?;
            let new = match doc.remove_member(&path, name) {
                Ok(t) => t,
                Err(json::EditError::Missing) => {
                    return Ok(Change::Same(format!(
                        "`{name}` is not in this file; nothing to remove"
                    )))
                }
                Err(json::EditError::NotObject(p)) => {
                    return Err(format!("`{p}` in the file is not an object"))
                }
                Err(e) => return Err(format!("internal: {e:?}")),
            };
            verify_json(current, &new, jsonc, &path, name, None)?;
            Ok(Change::Text(new))
        }
        (Format::Toml, Some(spec), None) => {
            let Rendered::Toml { container, fields } = render::render(preset, spec)? else {
                return Err("internal: renderer gave a non-TOML entry for a TOML file".into());
            };
            match tomlfile::set_entry(current, &container, &spec.name, &fields, replace) {
                Ok(t) => Ok(Change::Text(t)),
                Err(tomlfile::TomlError::Exists { same: true }) => Ok(Change::Same(format!(
                    "`{}` is already connected with exactly this entry; nothing to change",
                    spec.name
                ))),
                Err(tomlfile::TomlError::Exists { same: false }) => {
                    Err(format!("`{}` {EXISTS}", spec.name))
                }
                Err(tomlfile::TomlError::Parse(e)) => {
                    Err(unparsable(format!("not valid TOML: {e}")))
                }
                Err(tomlfile::TomlError::NotTable(p)) => {
                    Err(format!("`{p}` in the file is not a table"))
                }
                Err(tomlfile::TomlError::Missing) => Err("internal: missing".into()),
            }
        }
        (Format::Toml, None, Some(name)) => {
            match tomlfile::remove_entry(current, &preset.entry_path[0], name) {
                Ok(t) => Ok(Change::Text(t)),
                Err(tomlfile::TomlError::Missing) => Ok(Change::Same(format!(
                    "`{name}` is not in this file; nothing to remove"
                ))),
                Err(tomlfile::TomlError::Parse(e)) => {
                    Err(unparsable(format!("not valid TOML: {e}")))
                }
                Err(e) => Err(format!("internal: {e:?}")),
            }
        }
        _ => Err("internal: nothing to change".into()),
    }
}

/// Re-parses the new text and checks that it differs from the old one in
/// `path` + `key` only. A splice bug can then never reach the disk.
fn verify_json(
    old: &str,
    new: &str,
    jsonc: bool,
    path: &[&str],
    key: &str,
    value: Option<&json::Value>,
) -> Result<(), String> {
    let empty = || json::Value::Object(Vec::new());
    let before = json::parse(old, jsonc)
        .map_err(|e| format!("internal: {e}"))?
        .root
        .map_or_else(empty, |n| n.to_value());
    let after = json::parse(new, jsonc)
        .map_err(|e| format!("internal: the edited text does not parse ({e}); nothing written"))?
        .root
        .map_or_else(empty, |n| n.to_value());
    let mut expected = before;
    match value {
        Some(v) => expected
            .set_path(path, key, v.clone())
            .map_err(|e| format!("internal: {e}"))?,
        None => {
            expected.remove_path(path, key);
        }
    }
    if after != expected {
        return Err(
            "internal: the edit changed more than the entry; nothing written (please report this)"
                .into(),
        );
    }
    Ok(())
}

fn rendered_text(r: &Rendered) -> String {
    match r {
        Rendered::Json(v) => json::pretty(v, "  ", ""),
        Rendered::Toml { container, fields } => tomlfile::preview(container, fields),
        Rendered::Yaml(t) => t.clone(),
    }
}

pub fn run(args: &[String], ctx: &mut Ctx) -> u8 {
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            ctx.err.push_str(&format!("tk connect: {e}\n{USAGE}"));
            return 2;
        }
    };
    if o.list {
        list(ctx);
        return 0;
    }
    let Some(id) = o.harness.as_deref() else {
        ctx.err.push_str(USAGE);
        return 2;
    };
    let Some(preset) = presets::find(id) else {
        let ids: Vec<String> = presets::table()
            .harnesses
            .into_iter()
            .map(|p| p.id)
            .collect();
        ctx.err.push_str(&format!(
            "tk connect: unknown harness `{id}`; one of: {}\n",
            ids.join(", ")
        ));
        return 2;
    };
    match connect(args, &o, &preset, ctx) {
        Ok(code) => code,
        Err(e) => {
            ctx.err.push_str(&format!("tk connect: {e}\n"));
            1
        }
    }
}

fn connect(args: &[String], o: &Opts, preset: &Preset, ctx: &mut Ctx) -> Result<u8, String> {
    let path = preset
        .user_path(&ctx.home, ctx.macos, ctx.env)
        .ok_or_else(|| {
            format!(
                "{}: no user-level config file is known for this OS",
                preset.name
            )
        })?;
    if o.path {
        ctx.out.push_str(&format!("{}\n", path.display()));
        return Ok(0);
    }
    if o.allowlist {
        let spec = spec_from(o)?;
        ctx.out
            .push_str(&allowlist::fragment(preset, &spec, ctx.macos)?);
        return Ok(0);
    }
    let spec = match (&o.remove, o.from.is_some() || o.gateway.is_some()) {
        (Some(_), true) => return Err("--remove takes a server name alone".into()),
        (Some(name), false) => {
            if !spec::valid_server_name(name) {
                return Err(format!("`{name}` is not a server name tk writes"));
            }
            None
        }
        (None, _) => Some(spec_from(o)?),
    };
    if let Some(name) = spec.as_ref().map(|s| &s.name) {
        if preset.reserved_names.contains(name) {
            return Err(format!(
                "`{name}` is a server name {} keeps for itself; pick another with --name",
                preset.name
            ));
        }
    }
    if o.check_profile {
        let profile = o
            .profile
            .as_deref()
            .ok_or("--check-profile goes with --gateway docker --profile")?;
        if !gateway::docker_profile_exists(profile)? {
            return Err(format!(
                "Docker MCP profile `{profile}` was not found (docker mcp profile show)"
            ));
        }
    }

    // What the user is told, before any diff.
    ctx.out.push_str(&format!(
        "tk connect: {} ({}), user config {}\n",
        preset.name,
        preset.id,
        path.display()
    ));
    if let Some(spec) = &spec {
        ctx.out
            .push_str(&format!("source: {}\n", origin_line(spec)));
        ctx.out
            .push_str(&format!("runs:   {}\n", spec.runner.describe()));
        ctx.out.push_str(&format!("server: {}", spec.name));
        if preset.id == "pi" {
            if let Some(e) = spec.exposure_for("pi") {
                ctx.out.push_str(&format!(" (exposure {e})"));
            }
        }
        ctx.out.push('\n');
        let vars = spec.variables();
        if !vars.is_empty() {
            ctx.out.push_str(&format!(
                "export before starting {}: {} (the file holds references, never values)\n",
                preset.name,
                vars.join(", ")
            ));
        }
        for n in &spec.notes {
            ctx.out.push_str(&format!("note:   {n}\n"));
        }
    }

    // Print-only harnesses: the entry, never a write.
    if !preset.writes() {
        let spec = spec.as_ref().ok_or_else(|| {
            format!(
                "{}: tk only prints entries for this harness; remove the entry by hand",
                preset.name
            )
        })?;
        let text = rendered_text(&render::render(preset, spec)?);
        spec::reject_list(&text)?;
        ctx.out.push_str(&format!(
            "print only: {}\n\n{}\n",
            preset.write_note.as_deref().unwrap_or(""),
            text.trim_end()
        ));
        if o.write {
            ctx.err.push_str(&format!(
                "tk connect: {} is print-only; nothing written\n",
                preset.name
            ));
            return Ok(1);
        }
        return Ok(0);
    }

    // Never a managed file (case 65), and never outside the home folder:
    // a user-level config lives there, and anything else (an override that
    // points at /etc) is not tk's to write.
    if preset.managed_paths(&ctx.home, ctx.macos).contains(&path) {
        return Err(format!(
            "{} is a managed file; tk never writes it (print the fragment with --allowlist)",
            path.display()
        ));
    }
    if !path.starts_with(&ctx.home)
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{} is outside your home folder; tk writes user-level files under it only",
            path.display()
        ));
    }

    let current = write::read_current(&path)?;
    let text = current.text()?.to_string();
    if let Some(expect) = &o.expect {
        if *expect != current.sha256 {
            return Err(format!(
                "{} changed since the dry run (sha256 {} now, {expect} then); nothing written. Run the dry run again.",
                path.display(),
                current.sha256
            ));
        }
    }
    // The rendered entry goes through the reject list once more, whole.
    if let Some(spec) = &spec {
        spec::reject_list(&rendered_text(&render::render(preset, spec)?))?;
    }
    let existed = spec
        .as_ref()
        .is_some_and(|s| entry_exists(preset, &text, &s.name));
    if preset.format == Format::Jsonc && json::parse(&text, true).is_ok_and(|d| d.has_comments) {
        ctx.out.push_str(
            "note:   the file has comments; tk keeps them and changes only the entry's lines\n",
        );
    }
    let new = match compute(preset, &text, spec.as_ref(), o.remove.as_deref(), o.replace)? {
        Change::Same(msg) => {
            ctx.out.push_str(&format!("{msg}\n"));
            return Ok(0);
        }
        Change::Text(t) => t,
    };
    let label = path.display().to_string();
    ctx.out.push_str(&format!(
        "current file: {}\n\n",
        if current.bytes.is_none() {
            "absent (tk would create it)".to_string()
        } else {
            format!("sha256 {}", current.sha256)
        }
    ));
    ctx.out.push_str(&diff::unified(
        &text,
        &new,
        &label,
        &format!("{label} (after tk connect)"),
    ));
    if !installed(preset, &path, &ctx.home, ctx.env) {
        ctx.out.push_str(&format!(
            "\n{} does not look installed here (no `{}` on PATH, no {}); tk writes nothing for a harness that is not installed.\n",
            preset.name,
            preset.cli.as_deref().unwrap_or("binary"),
            path.display()
        ));
        if o.write {
            ctx.err.push_str("tk connect: nothing written\n");
            return Ok(1);
        }
        return Ok(0);
    }
    let is_host = matches!(spec.as_ref().map(|s| &s.runner), Some(Runner::Host));
    let is_toolhive = matches!(spec.as_ref().map(|s| &s.origin), Some(Origin::Gateway { backend, .. }) if backend == "toolhive");
    if !o.write {
        let again: Vec<&str> = args
            .iter()
            .map(String::as_str)
            .filter(|a| *a != "--write")
            .collect();
        ctx.out.push_str(&format!(
            "\ndry run: nothing written. To write it: tk connect {} --write --expect-sha256 {}\n",
            again.join(" "),
            current.sha256
        ));
        if is_host && !o.accept_host {
            ctx.out.push_str(
                "this entry runs as you, outside any sandbox: --write also needs --accept-host\n",
            );
        }
        if is_toolhive {
            ctx.out
                .push_str("the ToolHive backend prints only until the box test measures it\n");
        }
        return Ok(0);
    }
    if is_host && !o.accept_host {
        return Err(
            "this entry runs as you, outside any sandbox; add --accept-host to write it".into(),
        );
    }
    if is_toolhive {
        return Err(
            "the ToolHive backend prints only until the box test measures it; nothing written"
                .into(),
        );
    }
    let backups = ctx.home.join(".tk/backups");
    ctx.flush();
    let written = write::apply(
        &path,
        &preset.id,
        &current.sha256,
        &new,
        &backups,
        ctx.confirm,
    )?;
    let rec = log::Record {
        harness: preset.id.clone(),
        path: path.display().to_string(),
        server: spec
            .as_ref()
            .map_or_else(|| o.remove.clone().unwrap_or_default(), |s| s.name.clone()),
        action: if o.remove.is_some() {
            "remove".into()
        } else if existed {
            "replace".into()
        } else {
            "add".into()
        },
        sha256_before: current.sha256.clone(),
        sha256_after: written.sha256_after.clone(),
        backup: written.backup.as_ref().map(|b| b.display().to_string()),
        origin: spec
            .as_ref()
            .map_or_else(|| "remove".to_string(), origin_line),
    };
    let xdg = ctx
        .env
        .var("XDG_RUNTIME_DIR")
        .filter(|d| d.starts_with('/'))
        .map(PathBuf::from);
    let socket = log::socket_path(&ctx.home, xdg.as_deref(), ctx.macos);
    let logged = log::record(&socket, &ctx.home.join(".tk/connect.log"), &rec)
        .unwrap_or_else(|e| format!("nowhere ({e})"));
    ctx.out.push_str(&format!(
        "\nwritten: {} (sha256 {}).{} Logged to {logged}.\n",
        path.display(),
        written.sha256_after,
        written
            .backup
            .map_or(String::new(), |b| format!(" Backup: {}.", b.display()))
    ));
    Ok(0)
}

/// Whether the server is in the file already (for the log's `replace`).
fn entry_exists(preset: &Preset, text: &str, name: &str) -> bool {
    let path: Vec<&str> = preset.entry_path.iter().map(String::as_str).collect();
    match preset.format {
        Format::Json | Format::Jsonc => json::parse(text, preset.format == Format::Jsonc)
            .ok()
            .and_then(|d| d.lookup(&path, name))
            .is_some(),
        Format::Toml => tomlfile::has_entry(text, &preset.entry_path[0], name).unwrap_or(false),
        Format::Yaml => false,
    }
}

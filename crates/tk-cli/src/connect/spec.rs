//! What `tk connect` writes, before it is shaped for one harness: a server
//! name, how to reach it (a command or a URL), and which environment
//! variables it reads. Built from a library entry (`server.json` with the tk
//! `_meta` block, `contracts/entry.schema.json`) or from a gateway profile
//! (`gateway/`), then checked once here so every harness gets the same
//! refusals.
//!
//! Secrets never appear as values. An environment variable or a secret
//! header is written as a reference that the harness expands from its own
//! environment when it starts the server (`${VAR}` and its per-harness
//! spellings), so the config file holds no key (rules, section 5, item 1;
//! toolkit plan, section 9).

use serde_json::Value as J;
use sha2::{Digest, Sha256};

use tk_core::policy::{fold_text, ENDPOINT_KEYS, GIT_CONFIG_KEYS, GIT_CONTEXT_PATHS};

/// The `_meta` key that holds the tk block (`contracts/entry.schema.json`).
pub const META_PUBLISHER: &str = "io.modelcontextprotocol.registry/publisher-provided";
pub const META_KEY: &str = "io.github.aetiusgular.tk";

const MAX_ARGS: usize = 64;
const MAX_ARG_CHARS: usize = 1024;
const MAX_URL_CHARS: usize = 2048;
const MAX_HEADERS: usize = 16;
const MAX_ENV: usize = 32;
/// Library entries are small; anything bigger is not one.
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpKind {
    /// MCP streamable HTTP (`streamable-http` in `server.json`).
    Streamable,
    /// The older HTTP+SSE transport.
    Sse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderValue {
    Literal(String),
    /// `prefix` then the value of the environment variable `var`, such as
    /// `Bearer ${TOKEN}`.
    Ref {
        prefix: String,
        var: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: HeaderValue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
    },
    Http {
        kind: HttpKind,
        url: String,
        headers: Vec<Header>,
    },
}

/// An environment variable the server reads: `key` set from the caller's
/// `var`. tk only ever writes `key == var` today; the split exists so a
/// harness that can only pass variables through by name (Codex's
/// `env_vars`) can refuse what it cannot express.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvRef {
    pub key: String,
    pub var: String,
}

/// How the entry's code runs (roadblock R2), shown on every dry run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runner {
    /// A hosted endpoint; nothing runs here.
    Remote,
    /// A container image started with `docker run`.
    Image { image: String },
    /// A bare process on this machine, with your rights.
    Host,
    /// A gateway that runs each server in its own container.
    Gateway { backend: String },
}

impl Runner {
    pub fn describe(&self) -> String {
        match self {
            Runner::Remote => "remote endpoint (nothing runs on this machine)".into(),
            Runner::Image { image } => format!("container image {image} (docker run)"),
            Runner::Host => {
                "runs as you: a bare process with your full rights, outside any sandbox".into()
            }
            Runner::Gateway { backend } => {
                format!("{backend} gateway (each server in its own container)")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Entry {
        file: String,
        sha256: String,
        registry_name: String,
        version: String,
    },
    Gateway {
        backend: String,
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerSpec {
    pub name: String,
    pub transport: Transport,
    pub env: Vec<EnvRef>,
    pub runner: Runner,
    /// Per harness id, from the entry's `exposure` (Pi's values). The
    /// `default` key applies to harnesses not named.
    pub exposure: Vec<(String, String)>,
    pub origin: Origin,
    /// Shown on the dry run, never fatal: a deprecated entry, a variable
    /// the user must export.
    pub notes: Vec<String>,
}

impl ServerSpec {
    pub fn exposure_for(&self, harness: &str) -> Option<&str> {
        self.exposure
            .iter()
            .find(|(h, _)| h == harness)
            .or_else(|| self.exposure.iter().find(|(h, _)| h == "default"))
            .map(|(_, v)| v.as_str())
    }

    /// Every environment variable the user must export for this entry.
    pub fn variables(&self) -> Vec<String> {
        let mut out: Vec<String> = self.env.iter().map(|e| e.var.clone()).collect();
        if let Transport::Http { headers, .. } = &self.transport {
            for h in headers {
                if let HeaderValue::Ref { var, .. } = &h.value {
                    out.push(var.clone());
                }
            }
        }
        out.dedup();
        out
    }
}

/// `^[a-z0-9][a-z0-9-]{0,63}$`. Lowercase because the policy matcher
/// lowercases (`contracts/policy.md`, section 2); no `_` because Gemini and
/// Qwen split tool names at the first underscore and the matcher's step 3
/// would split there too; no `.` because the Docker gateway splits
/// `server.tool` at the first dot (`contracts/docker-args.md`, section 1).
pub fn valid_server_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`.
pub fn valid_env_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// `^[A-Za-z0-9_-]{1,64}$`: RFC 9110 tokens (section 5.6.2), narrowed to
/// letters, digits, `-` and `_`. `_` is in because real servers name headers
/// with it (Context7's `CONTEXT7_API_KEY`; integration w1).
fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Header names whose value is a credential, so it must be a reference.
fn secret_header(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    [
        "authorization",
        "token",
        "secret",
        "key",
        "password",
        "cookie",
        "auth",
    ]
    .iter()
    .any(|w| l.contains(w))
}

/// `X-API-Key` -> `X_API_KEY`.
fn env_name_for(header: &str) -> String {
    header
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads one library entry. `name` overrides the server name derived from
/// the entry's registry name.
pub fn from_entry(file: &str, bytes: &[u8], name: Option<&str>) -> Result<ServerSpec, String> {
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(format!(
            "{file}: larger than {MAX_ENTRY_BYTES} bytes; not a library entry"
        ));
    }
    let doc: J = serde_json::from_slice(bytes).map_err(|e| format!("{file}: not JSON: {e}"))?;
    let registry_name = doc
        .get("name")
        .and_then(J::as_str)
        .ok_or_else(|| format!("{file}: no `name` (server.json requires one)"))?
        .to_string();
    let version = doc
        .get("version")
        .and_then(J::as_str)
        .ok_or_else(|| format!("{file}: no `version` (server.json requires one)"))?
        .to_string();
    let meta = doc
        .get("_meta")
        .and_then(|m| m.get(META_PUBLISHER))
        .and_then(|m| m.get(META_KEY))
        .ok_or_else(|| {
            format!("{file}: no `_meta[\"{META_PUBLISHER}\"][\"{META_KEY}\"]` block; tk connects library entries only")
        })?;
    let kind = meta.get("kind").and_then(J::as_str).unwrap_or("");
    if kind != "server" {
        return Err(format!("{file}: entry kind is `{kind}`; tk connect writes server entries only (skills and bundles install through their own exports)"));
    }
    let mut notes = Vec::new();
    match meta.get("status").and_then(J::as_str) {
        Some("active") => {}
        Some("deprecated") => notes.push("the library marks this entry deprecated".to_string()),
        Some("deleted") => return Err(format!("{file}: the library marks this entry deleted")),
        other => return Err(format!("{file}: bad `status` {other:?}")),
    }
    let exposure = match meta.get("exposure").and_then(J::as_object) {
        Some(map) if map.contains_key("default") => {
            let mut out = Vec::new();
            for (h, v) in map {
                let v = v.as_str().unwrap_or("");
                if !["codemode", "deferred", "direct", "hidden"].contains(&v) {
                    return Err(format!("{file}: bad exposure `{v}` for `{h}`"));
                }
                out.push((h.clone(), v.to_string()));
            }
            out
        }
        _ => return Err(format!("{file}: `exposure` with a `default` is required")),
    };
    let runner = meta
        .get("runner")
        .and_then(|r| r.get("kind"))
        .and_then(J::as_str)
        .unwrap_or("");
    let derived;
    let name = match name {
        Some(n) => n,
        None => {
            derived = derive_name(&registry_name);
            derived.as_str()
        }
    };
    let (transport, env, runner) = match runner {
        "remote" => {
            let (t, e) = remote_transport(file, &doc)?;
            (t, e, Runner::Remote)
        }
        "image" => {
            let r = meta.get("runner").unwrap_or(&J::Null);
            let image = r.get("image").and_then(J::as_str).unwrap_or("");
            let digest = r.get("imageDigest").and_then(J::as_str);
            let (t, e) = image_transport(file, &doc, image, digest)?;
            (t, e, Runner::Image { image: image.to_string() })
        }
        "host" => {
            if meta.get("runner").and_then(|r| r.get("note")).and_then(J::as_str) != Some("runs as you") {
                return Err(format!("{file}: a host runner must carry note \"runs as you\""));
            }
            let (t, e) = package_transport(file, &doc)?;
            (t, e, Runner::Host)
        }
        "srt" => {
            return Err(format!(
                "{file}: the `srt` runner is not supported by tk connect yet (srt's command line was not verified in wave 1; see the lane C status doc)"
            ))
        }
        other => return Err(format!("{file}: bad runner kind `{other}`")),
    };
    let spec = ServerSpec {
        name: name.to_string(),
        transport,
        env,
        runner,
        exposure,
        origin: Origin::Entry {
            file: file.to_string(),
            sha256: sha256_hex(bytes),
            registry_name,
            version,
        },
        notes,
    };
    check(&spec)?;
    Ok(spec)
}

/// `io.github.owner/exa-search` -> `exa-search`; other characters become
/// `-` and the result is lowercased.
pub fn derive_name(registry_name: &str) -> String {
    let tail = registry_name.rsplit('/').next().unwrap_or(registry_name);
    let mut out: String = tail
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.starts_with('-') {
        out.remove(0);
    }
    out.truncate(64);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `Bearer {api_key}` -> `Ref { prefix: "Bearer ", var: "API_KEY" }`.
/// Only one placeholder, at the end, is accepted.
fn header_value(name: &str, value: Option<&str>, secret: bool) -> Result<HeaderValue, String> {
    match value {
        None => {
            if !secret {
                return Err(format!(
                    "header `{name}` has no value and is not marked secret"
                ));
            }
            Ok(HeaderValue::Ref {
                prefix: String::new(),
                var: env_name_for(name),
            })
        }
        Some(v) => {
            if let Some(open) = v.find('{') {
                let close = v.rfind('}').unwrap_or(0);
                if close + 1 != v.len() || close < open || v[open + 1..close].contains(['{', '}']) {
                    return Err(format!(
                        "header `{name}`: only one `{{variable}}` at the end is supported"
                    ));
                }
                let var = env_name_for(&v[open + 1..close]);
                if !valid_env_name(&var) {
                    return Err(format!("header `{name}`: bad variable name"));
                }
                Ok(HeaderValue::Ref {
                    prefix: v[..open].to_string(),
                    var,
                })
            } else if secret || secret_header(name) {
                Err(format!(
                    "header `{name}` carries a literal credential; tk writes credentials only as environment references"
                ))
            } else {
                Ok(HeaderValue::Literal(v.to_string()))
            }
        }
    }
}

fn remote_transport(file: &str, doc: &J) -> Result<(Transport, Vec<EnvRef>), String> {
    let remote = doc
        .get("remotes")
        .and_then(J::as_array)
        .and_then(|r| r.first())
        .ok_or_else(|| format!("{file}: a remote runner needs `remotes[0]`"))?;
    let kind = match remote.get("type").and_then(J::as_str) {
        Some("streamable-http") => HttpKind::Streamable,
        Some("sse") => HttpKind::Sse,
        other => {
            return Err(format!(
                "{file}: remote type {other:?} is not streamable-http or sse"
            ))
        }
    };
    let url = remote
        .get("url")
        .and_then(J::as_str)
        .ok_or_else(|| format!("{file}: remote has no `url`"))?
        .to_string();
    if url.contains('{') {
        return Err(format!(
            "{file}: URL templates (`{{var}}` in the URL) are not supported"
        ));
    }
    let mut headers = Vec::new();
    for h in remote
        .get("headers")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
    {
        let name = h
            .get("name")
            .and_then(J::as_str)
            .ok_or_else(|| format!("{file}: a header has no name"))?;
        let secret = h.get("isSecret").and_then(J::as_bool).unwrap_or(false);
        headers.push(Header {
            name: name.to_string(),
            value: header_value(name, h.get("value").and_then(J::as_str), secret)?,
        });
    }
    Ok((Transport::Http { kind, url, headers }, Vec::new()))
}

fn package_env(pkg: &J) -> Vec<EnvRef> {
    pkg.get("environmentVariables")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("name").and_then(J::as_str))
        .map(|n| EnvRef {
            key: n.to_string(),
            var: n.to_string(),
        })
        .collect()
}

/// Fixed package arguments only: `{type: positional, value}` and
/// `{type: named, name, value?}`. Anything with a `{variable}` is refused,
/// because tk would have to invent its value.
fn package_args(file: &str, pkg: &J, key: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for a in pkg.get(key).and_then(J::as_array).into_iter().flatten() {
        let value = a.get("value").and_then(J::as_str);
        let has_vars = a.get("variables").is_some() || value.is_some_and(|v| v.contains('{'));
        if has_vars {
            return Err(format!("{file}: `{key}` with variables are not supported"));
        }
        match a.get("type").and_then(J::as_str) {
            Some("positional") => out.push(
                value
                    .ok_or_else(|| format!("{file}: a positional argument has no value"))?
                    .to_string(),
            ),
            Some("named") => {
                let n = a
                    .get("name")
                    .and_then(J::as_str)
                    .ok_or_else(|| format!("{file}: a named argument has no name"))?;
                out.push(n.to_string());
                if let Some(v) = value {
                    out.push(v.to_string());
                }
            }
            other => return Err(format!("{file}: argument type {other:?} is not supported")),
        }
    }
    Ok(out)
}

fn package_transport(file: &str, doc: &J) -> Result<(Transport, Vec<EnvRef>), String> {
    let pkg = doc
        .get("packages")
        .and_then(J::as_array)
        .and_then(|p| p.first())
        .ok_or_else(|| format!("{file}: a host runner needs `packages[0]`"))?;
    if pkg
        .get("transport")
        .and_then(|t| t.get("type"))
        .and_then(J::as_str)
        .unwrap_or("stdio")
        != "stdio"
    {
        return Err(format!("{file}: only stdio packages are supported"));
    }
    let id = pkg
        .get("identifier")
        .and_then(J::as_str)
        .ok_or_else(|| format!("{file}: package has no identifier"))?;
    let version = pkg
        .get("version")
        .and_then(J::as_str)
        .ok_or_else(|| format!("{file}: package has no version (tk pins one)"))?;
    let runtime_args = package_args(file, pkg, "runtimeArguments")?;
    if !runtime_args.is_empty() {
        return Err(format!("{file}: `runtimeArguments` are not supported"));
    }
    let mut args = Vec::new();
    let command = match pkg.get("registryType").and_then(J::as_str) {
        Some("npm") => {
            args.push("-y".to_string());
            args.push(format!("{id}@{version}"));
            "npx"
        }
        Some("pypi") => {
            args.push(format!("{id}=={version}"));
            "uvx"
        }
        other => {
            return Err(format!(
            "{file}: registry type {other:?} is not supported for a host runner (npm and pypi are)"
        ))
        }
    };
    args.extend(package_args(file, pkg, "packageArguments")?);
    Ok((
        Transport::Stdio {
            command: command.to_string(),
            args,
        },
        package_env(pkg),
    ))
}

fn image_transport(
    file: &str,
    doc: &J,
    image: &str,
    digest: Option<&str>,
) -> Result<(Transport, Vec<EnvRef>), String> {
    let valid_image = !image.is_empty()
        && image.len() <= 300
        && !image.starts_with('-')
        && image
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"./:_-@".contains(&c));
    if !valid_image {
        return Err(format!("{file}: bad image reference"));
    }
    let oci = doc
        .get("packages")
        .and_then(J::as_array)
        .into_iter()
        .flatten()
        .find(|p| p.get("registryType").and_then(J::as_str) == Some("oci"));
    let env = oci.map(package_env).unwrap_or_default();
    let mut args = vec!["run".to_string(), "-i".to_string(), "--rm".to_string()];
    for e in &env {
        args.push("-e".to_string());
        args.push(e.key.clone());
    }
    match digest {
        Some(d) => args.push(format!("{}@{d}", image.split('@').next().unwrap_or(image))),
        None => args.push(image.to_string()),
    }
    if let Some(p) = oci {
        args.extend(package_args(file, p, "packageArguments")?);
    }
    Ok((
        Transport::Stdio {
            command: "docker".to_string(),
            args,
        },
        env,
    ))
}

fn is_loopback_http(url: &str) -> bool {
    let rest = &url["http://".len()..];
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// The refusals every harness shares. Run on the spec, before any harness
/// shape exists, and again by the caller on the rendered text.
pub fn check(spec: &ServerSpec) -> Result<(), String> {
    if !valid_server_name(&spec.name) {
        return Err(format!(
            "server name `{}` must match ^[a-z0-9][a-z0-9-]{{0,63}}$ (use --name)",
            spec.name
        ));
    }
    let mut texts: Vec<&str> = vec![&spec.name];
    match &spec.transport {
        Transport::Stdio { command, args } => {
            if args.len() > MAX_ARGS {
                return Err(format!("more than {MAX_ARGS} arguments"));
            }
            texts.push(command);
            texts.extend(args.iter().map(String::as_str));
        }
        Transport::Http { url, headers, .. } => {
            if url.len() > MAX_URL_CHARS {
                return Err("URL too long".into());
            }
            let ok_scheme = url.starts_with("https://")
                || (url.starts_with("http://") && is_loopback_http(url));
            if !ok_scheme {
                return Err(format!("URL `{url}` must be https, or http on loopback"));
            }
            if headers.len() > MAX_HEADERS {
                return Err(format!("more than {MAX_HEADERS} headers"));
            }
            texts.push(url);
            for h in headers {
                if !valid_header_name(&h.name) {
                    return Err(format!("bad header name `{}`", h.name));
                }
                texts.push(&h.name);
                match &h.value {
                    HeaderValue::Literal(v) => texts.push(v),
                    HeaderValue::Ref { prefix, var } => {
                        if !valid_env_name(var) {
                            return Err(format!("bad variable name `{var}`"));
                        }
                        texts.push(prefix);
                    }
                }
            }
        }
    }
    if spec.env.len() > MAX_ENV {
        return Err(format!("more than {MAX_ENV} environment variables"));
    }
    for e in &spec.env {
        if !valid_env_name(&e.key) || !valid_env_name(&e.var) {
            return Err(format!("bad environment variable name `{}`", e.key));
        }
        texts.push(&e.key);
    }
    for t in &texts {
        check_text(t)?;
    }
    // A command line is one line for the git-context test: `git -c x`.
    let mut lines: Vec<String> = Vec::new();
    if let Transport::Stdio { command, args } = &spec.transport {
        lines.push(format!("{command} {}", args.join(" ")));
    }
    lines.extend(texts.iter().map(|t| t.to_string()));
    reject_list(&lines.join("\n"))
}

/// One value: no control characters, bounded, and none of the forms some
/// harness reads as a command (`!cmd` is Pi's command form for secrets,
/// `pi mcp add --help` at 1.4.0; `$(` and backticks are shell substitution).
///
/// Nor any form a harness expands from the environment or the disk: `$`
/// (Claude Code, Gemini, Qwen and Crush expand `$VAR`/`${VAR}` in command,
/// args and url; Pi in env and headers), `{env:` and `{file:` (OpenCode
/// substitutes both in the raw file text, `config/variable.ts` at v1.19.4).
/// An entry could otherwise send `https://x/${GITHUB_TOKEN}` to a remote
/// host. tk adds its own references when it renders, after this check.
pub fn check_text(t: &str) -> Result<(), String> {
    if t.contains('$') {
        return Err(format!(
            "`{t}`: `$` is expanded from your environment by several harnesses; refused"
        ));
    }
    let lower = t.to_ascii_lowercase();
    if lower.contains("{env:") || lower.contains("{file:") {
        return Err(format!(
            "`{t}`: `{{env:}}` and `{{file:}}` are expanded by OpenCode; refused"
        ));
    }
    if t.len() > MAX_ARG_CHARS.max(MAX_URL_CHARS) {
        return Err("a value is too long".into());
    }
    if t.chars().any(|c| c.is_control()) {
        return Err("a value holds a control character".into());
    }
    if t.trim_start().starts_with('!') {
        return Err(format!(
            "`{t}`: a value starting with `!` is a command in some harnesses (Pi); refused"
        ));
    }
    if t.contains('`') {
        return Err(format!("`{t}`: shell substitution is refused"));
    }
    Ok(())
}

/// The reject list of `contracts/policy.md`, section 3, applied to what tk
/// would write: endpoint, sandbox and approval keys always; git config keys
/// in a git context. Folded first (case 30, case 33).
pub fn reject_list(text: &str) -> Result<(), String> {
    let folded = fold_text(text);
    for key in ENDPOINT_KEYS {
        if folded.contains(key) {
            return Err(format!("reject list: {key}"));
        }
    }
    if git_context(&folded) {
        for key in GIT_CONFIG_KEYS {
            if folded.contains(key) {
                return Err(format!("reject list: {key}"));
            }
        }
    }
    Ok(())
}

/// `contracts/policy.md`, section 3.2: a git config path, or the word `git`
/// followed later on the same line by `-c`, `config` or `--config-env`.
/// Lane B implements the same test in `tk-core`; this copy keeps lane C's
/// branch self-contained until the two are merged (request for integration).
fn git_context(folded: &str) -> bool {
    if GIT_CONTEXT_PATHS.iter().any(|p| folded.contains(p)) {
        return true;
    }
    for line in folded.lines() {
        let b = line.as_bytes();
        let mut i = 0;
        while let Some(pos) = line[i..].find("git") {
            let at = i + pos;
            let before_ok = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
            let after = at + 3;
            let after_ok =
                after >= b.len() || !(b[after].is_ascii_alphanumeric() || b[after] == b'_');
            if before_ok && after_ok {
                let rest = &line[after..];
                if rest.contains("-c") || rest.contains("config") || rest.contains("--config-env") {
                    return true;
                }
            }
            i = after;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../../contracts/fixtures/entry/server.json");

    #[test]
    fn reads_the_contract_fixture() {
        let s = from_entry("server.json", FIXTURE.as_bytes(), None).unwrap();
        assert_eq!(s.name, "exa-fixture");
        assert_eq!(s.runner, Runner::Remote);
        assert_eq!(
            s.transport,
            Transport::Http {
                kind: HttpKind::Streamable,
                url: "https://mcp.exa.ai/mcp".into(),
                headers: vec![]
            }
        );
        assert_eq!(s.exposure_for("pi"), Some("deferred"));
        assert_eq!(s.exposure_for("codex"), Some("deferred"));
        match &s.origin {
            Origin::Entry {
                sha256, version, ..
            } => {
                assert_eq!(sha256.len(), 64);
                assert_eq!(version, "0.0.1");
            }
            o => panic!("{o:?}"),
        }
    }

    fn entry(meta_runner: &str, body: &str) -> String {
        format!(
            r#"{{"name": "io.github.x/Demo_Server", "version": "1.0.0", {body}
            "_meta": {{"{META_PUBLISHER}": {{"{META_KEY}": {{
              "kind": "server", "tier": "own", "status": "active",
              "runner": {meta_runner}, "exposure": {{"default": "direct"}}, "trust": {{}}
            }}}}}}}}"#
        )
    }

    #[test]
    fn host_npm_package_becomes_npx_with_a_pinned_version() {
        let e = entry(
            r#"{"kind": "host", "note": "runs as you"}"#,
            r#""packages": [{"registryType": "npm", "identifier": "@x/srv", "version": "2.1.0",
              "transport": {"type": "stdio"},
              "packageArguments": [{"type": "positional", "value": "/data"}, {"type": "named", "name": "--ro"}],
              "environmentVariables": [{"name": "X_TOKEN", "isSecret": true}]}],"#,
        );
        let s = from_entry("e.json", e.as_bytes(), None).unwrap();
        assert_eq!(s.name, "demo-server");
        assert_eq!(s.runner, Runner::Host);
        assert_eq!(
            s.transport,
            Transport::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@x/srv@2.1.0".into(),
                    "/data".into(),
                    "--ro".into()
                ]
            }
        );
        assert_eq!(
            s.env,
            vec![EnvRef {
                key: "X_TOKEN".into(),
                var: "X_TOKEN".into()
            }]
        );
        assert_eq!(s.variables(), vec!["X_TOKEN".to_string()]);
    }

    #[test]
    fn host_runner_without_the_note_is_refused() {
        let e = entry(r#"{"kind": "host"}"#, r#""packages": [],"#);
        assert!(from_entry("e.json", e.as_bytes(), None)
            .unwrap_err()
            .contains("runs as you"));
    }

    #[test]
    fn image_runner_pins_the_digest_and_forwards_env_by_name() {
        let d = format!("sha256:{}", "a".repeat(64));
        let e = entry(
            &format!(r#"{{"kind": "image", "image": "mcp/github", "imageDigest": "{d}"}}"#),
            r#""packages": [{"registryType": "oci", "identifier": "mcp/github", "version": "1",
              "environmentVariables": [{"name": "GITHUB_TOKEN"}]}],"#,
        );
        let s = from_entry("e.json", e.as_bytes(), Some("github")).unwrap();
        assert_eq!(
            s.transport,
            Transport::Stdio {
                command: "docker".into(),
                args: vec![
                    "run",
                    "-i",
                    "--rm",
                    "-e",
                    "GITHUB_TOKEN",
                    &format!("mcp/github@{d}")
                ]
                .into_iter()
                .map(String::from)
                .collect()
            }
        );
    }

    #[test]
    fn secret_headers_become_references_and_literal_credentials_are_refused() {
        let e = entry(
            r#"{"kind": "remote"}"#,
            r#""remotes": [{"type": "streamable-http", "url": "https://h.example/mcp", "headers": [
              {"name": "Authorization", "value": "Bearer {api_key}", "isSecret": true},
              {"name": "X-Client", "value": "tk"}]}],"#,
        );
        let s = from_entry("e.json", e.as_bytes(), Some("h")).unwrap();
        let Transport::Http { headers, .. } = &s.transport else {
            panic!()
        };
        assert_eq!(
            headers[0].value,
            HeaderValue::Ref {
                prefix: "Bearer ".into(),
                var: "API_KEY".into()
            }
        );
        assert_eq!(headers[1].value, HeaderValue::Literal("tk".into()));
        let bad = entry(
            r#"{"kind": "remote"}"#,
            r#""remotes": [{"type": "streamable-http", "url": "https://h.example/mcp", "headers": [
              {"name": "Authorization", "value": "Bearer sk-live-123"}]}],"#,
        );
        assert!(from_entry("e.json", bad.as_bytes(), Some("h"))
            .unwrap_err()
            .contains("literal credential"));
    }

    #[test]
    fn refuses_deleted_srt_skills_and_missing_meta() {
        let del = entry(r#"{"kind": "remote"}"#, r#""remotes": [],"#)
            .replace("\"active\"", "\"deleted\"");
        assert!(from_entry("e", del.as_bytes(), None)
            .unwrap_err()
            .contains("deleted"));
        let srt = entry(r#"{"kind": "srt", "srtProfile": "p"}"#, "");
        assert!(from_entry("e", srt.as_bytes(), None)
            .unwrap_err()
            .contains("srt"));
        let skill = entry(r#"{"kind": "remote"}"#, "").replace("\"server\"", "\"skill\"");
        assert!(from_entry("e", skill.as_bytes(), None)
            .unwrap_err()
            .contains("server entries only"));
        assert!(from_entry("e", br#"{"name": "a/b", "version": "1"}"#, None)
            .unwrap_err()
            .contains("_meta"));
        assert!(from_entry("e", b"not json", None).is_err());
    }

    fn stdio(args: &[&str]) -> ServerSpec {
        ServerSpec {
            name: "s".into(),
            transport: Transport::Stdio {
                command: "npx".into(),
                args: args.iter().map(|a| a.to_string()).collect(),
            },
            env: vec![],
            runner: Runner::Host,
            exposure: vec![],
            origin: Origin::Gateway {
                backend: "t".into(),
                id: "t".into(),
            },
            notes: vec![],
        }
    }

    #[test]
    fn reject_list_in_any_spelling() {
        for bad in [
            "ANTHROPIC_BASE_URL=http://evil",
            "--Dangerously-skip-permissions",
            "dangerouslyDisableSandbox",
            "approval\u{200B}_policy",
            "ＡＮＴＨＲＯＰＩＣ_BASE_URL",
            "settings.local.json",
        ] {
            assert!(
                check(&stdio(&[bad]))
                    .unwrap_err()
                    .starts_with("reject list"),
                "{bad}"
            );
        }
        assert!(check(&stdio(&["git", "-c", "core.sshCommand=x"]))
            .unwrap_err()
            .starts_with("reject list"));
        assert!(check(&stdio(&["notes", "protocol.version"])).is_ok());
    }

    #[test]
    fn command_forms_and_control_characters_are_refused() {
        for bad in [
            "!curl x | sh",
            "$(id)",
            "a`id`",
            "a\nb",
            "a\u{7}",
            "${GITHUB_TOKEN}",
            "$HOME/x",
            "{env:AWS_SECRET}",
            "{FILE:~/.ssh/id_rsa}",
        ] {
            assert!(check(&stdio(&[bad])).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_and_urls() {
        assert!(valid_server_name("exa-search"));
        for bad in ["", "Exa", "exa_search", "exa.search", "-x", &"a".repeat(65)] {
            assert!(!valid_server_name(bad), "{bad}");
        }
        let mut s = stdio(&[]);
        s.transport = Transport::Http {
            kind: HttpKind::Streamable,
            url: "http://evil.example/mcp".into(),
            headers: vec![],
        };
        assert!(check(&s).is_err());
        s.transport = Transport::Http {
            kind: HttpKind::Streamable,
            url: "http://127.0.0.1:8811/mcp".into(),
            headers: vec![],
        };
        assert!(check(&s).is_ok());
        assert_eq!(derive_name("io.github.x/My_Server.v2"), "my-server-v2");
    }

    /// Integration w1: Context7's header is `CONTEXT7_API_KEY` (lane E's
    /// catalog, from docker/mcp-registry servers/context7/server.yaml), and
    /// RFC 9110 section 5.6.2 lists `_` in `tchar`. Space, colon and
    /// control characters stay refused.
    #[test]
    fn header_names_take_underscores_and_nothing_else_new() {
        assert!(valid_header_name("CONTEXT7_API_KEY"));
        assert!(valid_header_name("x-api-key"));
        for bad in ["x api", "x:y", "x\ny", "", "a.b", "x\u{200b}y"] {
            assert!(!valid_header_name(bad), "{bad:?}");
        }
        let e = entry(
            r#"{"kind": "remote"}"#,
            r#""remotes": [{"type": "streamable-http", "url": "https://h.example/mcp", "headers": [
              {"name": "CONTEXT7_API_KEY", "isSecret": true}]}],"#,
        );
        let s = from_entry("e.json", e.as_bytes(), Some("h")).unwrap();
        assert!(check(&s).is_ok());
        assert_eq!(s.variables(), vec!["CONTEXT7_API_KEY".to_string()]);
    }
}

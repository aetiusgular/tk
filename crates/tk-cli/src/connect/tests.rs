//! End-to-end tests for `tk connect`: byte comparisons against what each
//! harness's own `mcp add` wrote (recorded 27 Oct in a scratch HOME; how
//! each file was made is in `fixtures/recorded/README.md`), and the write
//! path through `run` with a scripted terminal.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::presets::{self, Env};
use super::spec::{EnvRef, Header, HeaderValue, HttpKind, Origin, Runner, ServerSpec, Transport};
use super::write::tests::{scratch, Answer};
use super::{compute, run, Change, Ctx};

const REC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/connect/fixtures/recorded/"
);

fn rec(name: &str) -> String {
    fs::read_to_string(format!("{REC}{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn stdio(name: &str, command: &str, args: &[&str], env: &[&str]) -> ServerSpec {
    ServerSpec {
        name: name.into(),
        transport: Transport::Stdio {
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        },
        env: env
            .iter()
            .map(|e| EnvRef {
                key: e.to_string(),
                var: e.to_string(),
            })
            .collect(),
        runner: Runner::Host,
        exposure: vec![],
        origin: Origin::Gateway {
            backend: "test".into(),
            id: "t".into(),
        },
        notes: vec![],
    }
}

fn http(name: &str, url: &str, headers: Vec<Header>) -> ServerSpec {
    let mut s = stdio(name, "x", &[], &[]);
    s.transport = Transport::Http {
        kind: HttpKind::Streamable,
        url: url.into(),
        headers,
    };
    s.runner = Runner::Remote;
    s
}

fn bearer(var: &str) -> Header {
    Header {
        name: "Authorization".into(),
        value: HeaderValue::Ref {
            prefix: "Bearer ".into(),
            var: var.into(),
        },
        optional: false,
    }
}

fn header(name: &str, var: &str) -> Header {
    Header {
        name: name.into(),
        value: HeaderValue::Ref {
            prefix: String::new(),
            var: var.into(),
        },
        optional: false,
    }
}

fn add(harness: &str, before: &str, spec: &ServerSpec) -> String {
    let p = presets::find(harness).unwrap();
    match compute(&p, before, Some(spec), None, false) {
        Ok(Change::Text(t)) => t,
        Ok(Change::Same(m)) => panic!("unexpected no-op: {m}"),
        Err(e) => panic!("{harness}: {e}"),
    }
}

fn fs_server() -> ServerSpec {
    stdio(
        "fs",
        "npx",
        &["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
        &["TOKEN"],
    )
}

// --- byte comparisons with each harness's own `mcp add` ---

#[test]
fn claude_code_matches_claude_mcp_add_json() {
    // `claude mcp add-json -s user fs '{"type":"stdio",...,"env":{"TOKEN":"${TOKEN}"}}'`
    assert_eq!(
        add("claude-code", &rec("claude-1-before.json"), &fs_server()),
        rec("claude-1-after.json")
    );
    // `claude mcp add-json -s user hdr '{"type":"http","url":...,"headers":{...}}'`
    let hdr = http(
        "hdr",
        "https://mcp.example.com/mcp",
        vec![bearer("EX_TOKEN")],
    );
    assert_eq!(
        add("claude-code", &rec("claude-2-after.json"), &hdr),
        rec("claude-3-after.json")
    );
}

#[test]
fn pi_matches_pi_mcp_add() {
    let mut fs_d = fs_server();
    fs_d.exposure = vec![("default".into(), "deferred".into())];
    let one = add("pi", "", &fs_d);
    assert_eq!(one, rec("pi-1-after.json"));
    let mut exa = http("exa", "https://mcp.exa.ai/mcp", vec![bearer("EXA_KEY")]);
    exa.exposure = vec![
        ("pi".into(), "deferred".into()),
        ("default".into(), "direct".into()),
    ];
    let two = add("pi", &one, &exa);
    assert_eq!(two, rec("pi-2-after.json"));
    let three = add(
        "pi",
        &two,
        &http("hdr", "https://x.example/mcp", vec![header("X-Key", "XK")]),
    );
    assert_eq!(three, rec("pi-3-after.json"));
    let four = add(
        "pi",
        &three,
        &stdio("plain", "uvx", &["mcp-server-time"], &[]),
    );
    assert_eq!(four, rec("pi-4-after.json"));
}

#[test]
fn codex_matches_codex_mcp_add_and_keeps_comments() {
    let exa = http("exa", "https://mcp.exa.ai/mcp", vec![bearer("EXA_KEY")]);
    let one = add("codex", &rec("codex-1-before.toml"), &exa);
    assert_eq!(one, rec("codex-1-after.toml"));
    let two = add(
        "codex",
        &one,
        &stdio("time", "uvx", &["mcp-server-time"], &[]),
    );
    assert_eq!(two, rec("codex-2-after.toml"));
}

#[test]
fn gemini_matches_gemini_mcp_add() {
    let one = add("gemini", "", &fs_server());
    let two = add(
        "gemini",
        &one,
        &http("exa", "https://mcp.exa.ai/mcp", vec![bearer("EXA_KEY")]),
    );
    assert_eq!(two, rec("gemini-2-after.json"));
}

#[test]
fn qwen_matches_qwen_mcp_add() {
    let one = add("qwen-code", "", &fs_server());
    let two = add(
        "qwen-code",
        &one,
        &http("exa", "https://mcp.exa.ai/mcp", vec![bearer("EXA_KEY")]),
    );
    // Qwen adds `"$version": 4` when it first writes the file; tk does not.
    let qwen = rec("qwen-2-after.json").replace("\n  },\n  \"$version\": 4\n}", "\n  }\n}");
    assert_eq!(two, qwen);
}

#[test]
fn copilot_matches_copilot_mcp_add() {
    let one = add("copilot-cli", "", &fs_server());
    let two = add(
        "copilot-cli",
        &one,
        &http("exa", "https://mcp.exa.ai/mcp", vec![header("X-Key", "XK")]),
    );
    assert_eq!(two, rec("copilot-2-after.json"));
}

#[test]
fn copilot_refuses_a_header_with_text_before_the_variable() {
    let p = presets::find("copilot-cli").unwrap();
    let spec = http("exa", "https://mcp.exa.ai/mcp", vec![bearer("EXA_KEY")]);
    let e = compute(&p, "", Some(&spec), None, false).err().unwrap();
    assert!(e.contains("whole value"), "{e}");
}

#[test]
fn opencode_refuses_sse_and_writes_env_references() {
    let p = presets::find("opencode").unwrap();
    let mut sse = http("s", "https://h.example/sse", vec![]);
    if let Transport::Http { kind, .. } = &mut sse.transport {
        *kind = HttpKind::Sse;
    }
    assert!(compute(&p, "", Some(&sse), None, false).is_err());
    match compute(&p, "", Some(&fs_server()), None, false) {
        Ok(Change::Text(t)) => {
            assert!(t.contains("\"TOKEN\": \"{env:TOKEN}\""), "{t}");
            assert!(t.contains("\"command\": [\n        \"npx\""), "{t}");
            assert!(!t.ends_with('\n'));
        }
        _ => panic!(),
    }
}

// --- the command line, end to end ---

struct MapEnv(HashMap<String, String>);
impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

struct Run {
    code: u8,
    out: String,
    err: String,
}

fn go(home: &Path, env: &MapEnv, answer: bool, args: &[&str]) -> Run {
    let mut a = Answer(answer, None);
    let mut ctx = Ctx {
        home: home.to_path_buf(),
        macos: false,
        env,
        confirm: &mut a,
        out: String::new(),
        err: String::new(),
        live: false,
    };
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let code = run(&args, &mut ctx);
    Run {
        code,
        out: ctx.out,
        err: ctx.err,
    }
}

fn no_env() -> MapEnv {
    MapEnv(HashMap::new())
}

fn entry_file(dir: &Path) -> PathBuf {
    let p = dir.join("exa.json");
    fs::write(
        &p,
        include_str!("../../../../contracts/fixtures/entry/server.json"),
    )
    .unwrap();
    p
}

fn sha_line(out: &str) -> String {
    out.lines()
        .find_map(|l| l.split("--expect-sha256 ").nth(1))
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn dry_run_writes_nothing_and_prints_the_diff_and_the_write_line() {
    let home = scratch("dry");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = entry_file(&home);
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", e.to_str().unwrap()],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("+    \"exa-fixture\": {"), "{}", r.out);
    assert!(r.out.contains("\"exposure\": \"deferred\""));
    assert!(r.out.contains("runs:   remote endpoint"));
    assert!(r.out.contains("dry run: nothing written"));
    assert!(!home.join(".pi/agent/mcp.json").exists());
}

#[test]
fn write_then_same_again_is_a_no_op_and_the_log_records_it() {
    let home = scratch("write");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = entry_file(&home);
    let ef = e.to_str().unwrap();
    let dry = go(&home, &no_env(), true, &["pi", "--from", ef]);
    let sha = sha_line(&dry.out);
    let w = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", ef, "--write", "--expect-sha256", &sha],
    );
    assert_eq!(w.code, 0, "{}", w.err);
    let file = fs::read_to_string(home.join(".pi/agent/mcp.json")).unwrap();
    assert!(file.contains("\"url\": \"https://mcp.exa.ai/mcp\""));
    let log = fs::read_to_string(home.join(".tk/connect.log")).unwrap();
    assert!(log.contains("\"action\":\"add\""), "{log}");
    assert!(
        !log.contains("mcp.exa.ai/mcp\""),
        "the log holds no config values"
    );
    let again = go(&home, &no_env(), true, &["pi", "--from", ef]);
    assert_eq!(again.code, 0);
    assert!(again.out.contains("already connected"), "{}", again.out);
}

#[test]
fn a_stale_expect_hash_is_refused() {
    // Case 55, across two invocations.
    let home = scratch("stale");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = entry_file(&home);
    let ef = e.to_str().unwrap();
    let sha = sha_line(&go(&home, &no_env(), true, &["pi", "--from", ef]).out);
    fs::write(home.join(".pi/agent/mcp.json"), "{\"mcpServers\": {}}\n").unwrap();
    let w = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", ef, "--write", "--expect-sha256", &sha],
    );
    assert_eq!(w.code, 1);
    assert!(w.err.contains("changed since the dry run"), "{}", w.err);
    assert_eq!(
        fs::read_to_string(home.join(".pi/agent/mcp.json")).unwrap(),
        "{\"mcpServers\": {}}\n"
    );
}

#[test]
fn not_confirmed_means_nothing_written() {
    let home = scratch("no");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = entry_file(&home);
    let w = go(
        &home,
        &no_env(),
        false,
        &["pi", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(w.code, 1);
    assert!(w.err.contains("not confirmed"));
    assert!(!home.join(".pi/agent/mcp.json").exists());
}

#[test]
fn a_harness_that_is_not_installed_gets_the_entry_and_no_write() {
    // Case 54.
    let home = scratch("absent");
    let e = entry_file(&home);
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(r.code, 1);
    assert!(r.out.contains("does not look installed"), "{}", r.out);
    assert!(r.out.contains("[mcp_servers.exa-fixture]"));
    assert!(!home.join(".codex/config.toml").exists());
}

#[test]
fn a_cli_on_path_counts_as_installed() {
    let home = scratch("onpath");
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("codex"), "").unwrap();
    let env = MapEnv(HashMap::from([(
        "PATH".to_string(),
        bin.display().to_string(),
    )]));
    let e = entry_file(&home);
    let r = go(
        &home,
        &env,
        true,
        &["codex", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let t = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert_eq!(
        t,
        "[mcp_servers.exa-fixture]\nurl = \"https://mcp.exa.ai/mcp\"\n"
    );
}

#[test]
fn a_different_entry_under_the_same_name_needs_replace() {
    let home = scratch("replace");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    fs::write(
        home.join(".pi/agent/mcp.json"),
        "{\n  \"mcpServers\": {\n    \"exa-fixture\": {\n      \"command\": \"evil\"\n    }\n  }\n}\n",
    )
    .unwrap();
    let e = entry_file(&home);
    let ef = e.to_str().unwrap();
    let r = go(&home, &no_env(), true, &["pi", "--from", ef, "--write"]);
    assert_eq!(r.code, 1);
    assert!(r.err.contains("--replace"), "{}", r.err);
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", ef, "--replace", "--write"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(!fs::read_to_string(home.join(".pi/agent/mcp.json"))
        .unwrap()
        .contains("evil"));
    let log = fs::read_to_string(home.join(".tk/connect.log")).unwrap();
    assert!(log.contains("\"action\":\"replace\""));
    // The old file was backed up.
    let backups: Vec<_> = fs::read_dir(home.join(".tk/backups")).unwrap().collect();
    assert_eq!(backups.len(), 1);
}

#[test]
fn remove_takes_the_entry_out() {
    let home = scratch("remove");
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(home.join(".codex/config.toml"), rec("codex-2-after.toml")).unwrap();
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--remove", "exa", "--write"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let t = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(!t.contains("mcp_servers.exa"));
    assert!(t.contains("[mcp_servers.time]"));
    assert!(t.starts_with("# my settings\n"));
    let r = go(&home, &no_env(), true, &["codex", "--remove", "exa"]);
    assert!(r.out.contains("nothing to remove"));
}

#[test]
fn project_scope_yes_flags_and_unknown_harnesses_are_usage_errors() {
    let home = scratch("usage");
    for args in [
        vec!["pi", "--scope", "project"],
        vec!["pi", "--yes"],
        vec!["pi", "-y"],
        vec!["nope", "--from", "x"],
        vec![],
        vec!["pi", "--expect-sha256", "abc"],
    ] {
        assert_eq!(go(&home, &no_env(), true, &args).code, 2, "{args:?}");
    }
}

#[test]
fn a_managed_path_is_never_written() {
    // Case 65: CODEX_HOME pointed at the folder of the managed file.
    let home = scratch("managed");
    let env = MapEnv(HashMap::from([(
        "CODEX_HOME".to_string(),
        "/etc/codex".to_string(),
    )]));
    let e = entry_file(&home);
    let r = go(
        &home,
        &env,
        true,
        &["codex", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("managed file") || r.err.contains("outside your home folder"),
        "{}",
        r.err
    );
    assert!(!Path::new("/etc/codex/config.toml").exists() || r.out.is_empty());
}

#[test]
fn a_file_with_comments_in_a_strict_json_harness_is_refused() {
    let home = scratch("comments");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    fs::write(
        home.join(".pi/agent/mcp.json"),
        "{\n  // mine\n  \"mcpServers\": {}\n}\n",
    )
    .unwrap();
    let e = entry_file(&home);
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", e.to_str().unwrap()],
    );
    assert_eq!(r.code, 1);
    assert!(r.err.contains("cannot parse"), "{}", r.err);
}

#[test]
fn zed_keeps_comments() {
    let home = scratch("zed");
    let dir = home.join(".config/zed");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("settings.json"),
        "// Zed settings\n{\n  \"theme\": \"One Dark\", // mine\n}\n",
    )
    .unwrap();
    let r = go(
        &home,
        &no_env(),
        true,
        &[
            "zed",
            "--gateway",
            "docker",
            "--profile",
            "tk_github",
            "--write",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    let t = fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(
        t.starts_with("// Zed settings\n{\n  \"theme\": \"One Dark\", // mine\n"),
        "{t}"
    );
    assert!(
        t.contains("\"context_servers\": {\n    \"github\": {\n      \"command\": \"docker\""),
        "{t}"
    );
}

#[test]
fn print_only_harnesses_print_and_refuse_write() {
    let home = scratch("print");
    for h in ["hermes", "goose", "openclaw"] {
        let r = go(
            &home,
            &no_env(),
            true,
            &[h, "--gateway", "docker", "--profile", "tk_github"],
        );
        assert_eq!(r.code, 0, "{h}: {}", r.err);
        assert!(r.out.contains("print only"), "{h}");
        assert!(r.out.contains("docker"), "{h}: {}", r.out);
        let w = go(
            &home,
            &no_env(),
            true,
            &[
                h,
                "--gateway",
                "docker",
                "--profile",
                "tk_github",
                "--write",
            ],
        );
        assert_eq!(w.code, 1, "{h}");
    }
}

#[test]
fn host_runner_needs_accept_host_to_write() {
    let home = scratch("host");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = home.join("host.json");
    fs::write(
        &e,
        r#"{"name": "io.github.x/time", "version": "1.0.0",
          "packages": [{"registryType": "pypi", "identifier": "mcp-server-time", "version": "1.2.0", "transport": {"type": "stdio"}}],
          "_meta": {"io.modelcontextprotocol.registry/publisher-provided": {"io.github.aetiusgular.tk": {
            "kind": "server", "tier": "own", "status": "active", "runner": {"kind": "host", "note": "runs as you"},
            "exposure": {"default": "direct"}, "trust": {}}}}}"#,
    )
    .unwrap();
    let ef = e.to_str().unwrap();
    let dry = go(&home, &no_env(), true, &["pi", "--from", ef]);
    assert!(dry.out.contains("runs as you"), "{}", dry.out);
    assert!(dry.out.contains("--accept-host"));
    let w = go(&home, &no_env(), true, &["pi", "--from", ef, "--write"]);
    assert_eq!(w.code, 1);
    let w = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", ef, "--write", "--accept-host"],
    );
    assert_eq!(w.code, 0, "{}", w.err);
    assert!(fs::read_to_string(home.join(".pi/agent/mcp.json"))
        .unwrap()
        .contains("mcp-server-time==1.2.0"));
}

#[test]
fn toolhive_prints_and_refuses_write() {
    let home = scratch("thv");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--gateway", "toolhive", "--workload", "fetch"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("\"thv\""));
    let w = go(
        &home,
        &no_env(),
        true,
        &[
            "pi",
            "--gateway",
            "toolhive",
            "--workload",
            "fetch",
            "--write",
        ],
    );
    assert_eq!(w.code, 1);
    assert!(!home.join(".pi/agent/mcp.json").exists());
}

#[test]
fn allowlist_fragments_print_and_write_nothing() {
    let home = scratch("allow");
    let r = go(
        &home,
        &no_env(),
        true,
        &[
            "claude-code",
            "--allowlist",
            "--gateway",
            "docker",
            "--profile",
            "tk_github",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(
        r.out.contains("\"serverCommand\": [\n        \"docker\""),
        "{}",
        r.out
    );
    let e = entry_file(&home);
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--allowlist", "--from", e.to_str().unwrap()],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(
        r.out.contains("[mcp_servers.exa-fixture.identity]"),
        "{}",
        r.out
    );
    assert!(r.out.contains("url = \"https://mcp.exa.ai/mcp\""));
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--allowlist", "--from", e.to_str().unwrap()],
    );
    assert_eq!(r.code, 1);
    assert_eq!(
        fs::read_dir(&home).unwrap().count(),
        1,
        "only the entry file"
    );
}

#[test]
fn list_and_path() {
    let home = scratch("list");
    let r = go(&home, &no_env(), true, &["--list"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.out.lines().count(), 15, "{}", r.out);
    let r = go(&home, &no_env(), true, &["claude-code", "--path"]);
    assert_eq!(
        r.out.trim(),
        home.join(".claude.json").display().to_string()
    );
}

#[test]
fn a_symlinked_config_is_refused() {
    let home = scratch("symlink");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let real = home.join("real.json");
    fs::write(&real, "{}").unwrap();
    std::os::unix::fs::symlink(&real, home.join(".pi/agent/mcp.json")).unwrap();
    let e = entry_file(&home);
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(r.code, 1);
    assert!(r.err.contains("symlink"));
    assert_eq!(fs::read_to_string(&real).unwrap(), "{}");
}

#[test]
fn a_reserved_claude_code_name_is_refused() {
    let home = scratch("reserved");
    let r = go(
        &home,
        &no_env(),
        true,
        &[
            "claude-code",
            "--gateway",
            "docker",
            "--profile",
            "tk_x",
            "--name",
            "computer-use",
        ],
    );
    assert_eq!(r.code, 1);
    assert!(r.err.contains("keeps for itself"), "{}", r.err);
}

#[test]
fn a_custom_codex_home_outside_home_is_refused() {
    let home = scratch("codexhome");
    let env = MapEnv(HashMap::from([(
        "CODEX_HOME".to_string(),
        "/srv/codex".to_string(),
    )]));
    let e = entry_file(&home);
    let r = go(
        &home,
        &env,
        true,
        &["codex", "--from", e.to_str().unwrap(), "--write"],
    );
    assert_eq!(r.code, 1);
    assert!(r.err.contains("outside your home folder"), "{}", r.err);
}

#[test]
fn an_entry_that_expands_a_variable_into_its_url_is_refused() {
    let home = scratch("exfil");
    let e = home.join("x.json");
    let text = include_str!("../../../../contracts/fixtures/entry/server.json").replace(
        "https://mcp.exa.ai/mcp",
        "https://mcp.exa.ai/mcp?k=$GITHUB_TOKEN",
    );
    fs::write(&e, text).unwrap();
    let r = go(
        &home,
        &no_env(),
        true,
        &["pi", "--from", e.to_str().unwrap()],
    );
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("expanded from your environment"),
        "{}",
        r.err
    );
}

// Mac test w1, finding M3: the dry run printed unchanged context lines of
// `~/.codex/config.toml` verbatim, and two of them were API keys. Every
// printed line (context, removed, added) goes through the redactor.
const FAKE_OPENAI: &str = "sk-proj-Q9vT2mX8LkR4nW7pZ3aY6bC1dE5fG0hJ";
const FAKE_ANTHROPIC: &str = "sk-ant-api03-Zk81mQ2xVb7Nc4LpRt9YwE3sDa6FgHj0";
const FAKE_OPAQUE: &str = "Zq3vX9mK2pL8rT4wY7nB1cF6hJ0sD5gA";

#[test]
fn the_dry_run_prints_no_secret_from_the_lines_around_the_change() {
    let home = scratch("m3-context");
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::write(
        home.join(".codex/config.toml"),
        format!(
            "model = \"gpt-5\"\n\n[shell_environment_policy.set]\nOPENAI_API_KEY = \"{FAKE_OPENAI}\"\nANTHROPIC_API_KEY = \"{FAKE_ANTHROPIC}\"\nSERVICE_SECRET = '{FAKE_OPAQUE}'\n"
        ),
    )
    .unwrap();
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--gateway", "docker", "--profile", "tk_exa"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("+[mcp_servers.exa]"), "{}", r.out);
    for s in [FAKE_OPENAI, FAKE_ANTHROPIC, FAKE_OPAQUE] {
        assert!(!r.out.contains(s), "{}", r.out);
    }
    // The context is still there, with its keys.
    assert!(r.out.contains(" OPENAI_API_KEY = \"[redacted]\""), "{}", r.out);
}

#[test]
fn removing_an_entry_prints_no_secret_from_its_lines() {
    let home = scratch("m3-remove");
    fs::write(
        home.join(".claude.json"),
        format!(
            "{{\n  \"mcpServers\": {{\n    \"supabase\": {{\n      \"command\": \"npx\",\n      \"args\": [\"-y\", \"@supabase/mcp-server-supabase\", \"--access-token\", \"sbp_{FAKE_OPAQUE}\"],\n      \"env\": {{\n        \"SUPABASE_ACCESS_TOKEN\": \"plain-but-named-{FAKE_OPAQUE}\",\n        \"DATABASE_URL\": \"postgresql://postgres:hunter2pass@db.example.supabase.co:5432/postgres\",\n        \"X_CUSTOM\": \"{FAKE_OPAQUE}\"\n      }},\n      \"headers\": {{\"Authorization\": \"Bearer opaqueBearerValue\"}}\n    }}\n  }}\n}}\n"
        ),
    )
    .unwrap();
    let r = go(&home, &no_env(), true, &["claude-code", "--remove", "supabase"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("-    \"supabase\": {"), "{}", r.out);
    for s in [FAKE_OPAQUE, "hunter2pass", "opaqueBearerValue"] {
        assert!(!r.out.contains(s), "{s} in {}", r.out);
    }
}

// Box test w1, BX11: the allowlist for the T0.1 HTTP gateway named the
// stdio command. C10 ran without `--url`; with the same flags as the write
// the fragment names the URL the entry uses.
#[test]
fn the_allowlist_for_an_http_gateway_names_its_url() {
    let home = scratch("bx11");
    let r = go(
        &home,
        &no_env(),
        true,
        &[
            "claude-code",
            "--allowlist",
            "--gateway",
            "docker",
            "--profile",
            "tk_box",
            "--url",
            "http://127.0.0.1:8811/mcp",
            "--bearer-env",
            "TK_GATEWAY_TOKEN",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("\"serverUrl\": \"http://127.0.0.1:8811/mcp\""), "{}", r.out);
    assert!(!r.out.contains("serverCommand"), "{}", r.out);
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--allowlist", "--gateway", "docker", "--profile", "tk_box", "--url", "http://127.0.0.1:8811/mcp"],
    );
    assert!(r.out.contains("url = \"http://127.0.0.1:8811/mcp\""), "{}", r.out);
}

// Mac test w1, M5: `identity.command = "docker"` matches any server named
// `exa` that docker starts (Codex's configuration reference: "The string
// form doesn't inspect arguments"). The fragment pins the executable and
// every argument with the matcher table (openai/codex#29648).
#[test]
fn the_codex_allowlist_pins_the_arguments_of_a_stdio_server() {
    let home = scratch("m5");
    let r = go(
        &home,
        &no_env(),
        true,
        &["codex", "--allowlist", "--gateway", "docker", "--profile", "tk_exa"],
    );
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("[mcp_servers.exa.identity.command]\nexecutable = \"docker\"\n"), "{}", r.out);
    for a in ["mcp", "gateway", "run", "--profile", "tk_exa"] {
        assert!(
            r.out.contains(&format!("  {{ match = \"exact\", value = \"{a}\" }},\n")),
            "{a}: {}",
            r.out
        );
    }
    assert!(!r.out.contains("command = \"docker\""), "{}", r.out);
    // The fragment is TOML a parser takes, with the shape Codex documents.
    let body: String = r.out.lines().filter(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n");
    let v: toml_edit::DocumentMut = body.parse().unwrap();
    let cmd = &v["mcp_servers"]["exa"]["identity"]["command"];
    assert_eq!(cmd["executable"].as_str(), Some("docker"));
    assert_eq!(cmd["args"].as_array().unwrap().len(), 5);
}

// Box test w1, BX6: Pi 1.4.0 refuses a server whose header variable is
// unset or empty ("Failed to resolve MCP server ... from environment
// variable", run on the box and in the cloud). Context7's key is optional
// (`isRequired: false`), so for Pi tk leaves an optional header out while
// its variable is unset, and says so; with the variable set it is kept.
#[test]
fn pi_leaves_out_an_optional_header_whose_variable_is_unset() {
    let home = scratch("bx6");
    fs::create_dir_all(home.join(".pi/agent")).unwrap();
    let e = home.join("context7.json");
    fs::write(&e, include_str!("../../../../library/catalog/context7/server.json")).unwrap();
    let ef = e.to_str().unwrap();
    let r = go(&home, &no_env(), true, &["pi", "--from", ef]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(!r.out.contains("+        \"CONTEXT7_API_KEY\""), "{}", r.out);
    assert!(r.out.contains("CONTEXT7_API_KEY is optional"), "{}", r.out);
    let set = MapEnv(HashMap::from([("CONTEXT7_API_KEY".to_string(), "k".to_string())]));
    let r = go(&home, &set, true, &["pi", "--from", ef]);
    assert!(r.out.contains("\"CONTEXT7_API_KEY\": \"${CONTEXT7_API_KEY}\""), "{}", r.out);
    // Another harness keeps the header: only Pi refuses an unset variable.
    fs::create_dir_all(home.join(".config/opencode")).unwrap();
    let r = go(&home, &no_env(), true, &["opencode", "--from", ef]);
    assert!(r.out.contains("CONTEXT7_API_KEY"), "{}", r.out);
    assert!(!r.out.contains("is optional"), "{}", r.out);
}

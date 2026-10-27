//! Gateway backends for `tk connect`: the harness gets one entry that
//! starts (or reaches) a gateway, and the gateway runs the servers.
//!
//! - `docker`: Docker's MCP gateway. `docker_mcp` is the fork's allowlist as
//!   copied; `host` replaces the fork helpers it imported. The harness entry
//!   is `docker mcp gateway run --profile <id>` (stdio, the shape the fork's
//!   `dockerArgs.ts:318-321` built and the app never ran itself), or the
//!   gateway's HTTP URL when it runs as its own service (T0.1, box plan
//!   section 5.5).
//! - `toolhive`: argument shapes only in wave 1 (`toolhive.rs`).

pub mod docker_mcp;
pub mod host;
pub mod toolhive;

use super::spec::{Header, HeaderValue, HttpKind, Origin, Runner, ServerSpec, Transport};

/// `tk_github` -> `github`, `my_profile` -> `my-profile`.
pub fn name_from_profile(profile: &str) -> String {
    profile
        .strip_prefix("tk_")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(profile)
        .replace('_', "-")
}

/// The entry for a Docker MCP gateway profile. With `url`, the gateway is
/// already running in HTTP mode and the entry reaches it, sending
/// `Authorization: Bearer ${bearer_env}` when given; else the harness starts
/// the gateway itself over stdio.
pub fn docker_spec(
    profile: &str,
    name: Option<&str>,
    url: Option<&str>,
    bearer_env: Option<&str>,
) -> Result<ServerSpec, String> {
    if !docker_mcp::is_profile_id(profile) {
        return Err(format!(
            "profile id `{profile}` must match ^[a-z0-9_]{{1,64}}$"
        ));
    }
    let name = name.map_or_else(|| name_from_profile(profile), str::to_string);
    let transport = match url {
        None => {
            if bearer_env.is_some() {
                return Err("--bearer-env needs --url (an HTTP gateway)".into());
            }
            Transport::Stdio {
                command: "docker".into(),
                args: ["mcp", "gateway", "run", "--profile", profile]
                    .into_iter()
                    .map(String::from)
                    .collect(),
            }
        }
        Some(u) => Transport::Http {
            kind: HttpKind::Streamable,
            url: u.to_string(),
            headers: bearer_env
                .map(|v| Header {
                    name: "Authorization".into(),
                    value: HeaderValue::Ref {
                        prefix: "Bearer ".into(),
                        var: v.to_string(),
                    },
                })
                .into_iter()
                .collect(),
        },
    };
    let spec = ServerSpec {
        name,
        transport,
        env: Vec::new(),
        runner: Runner::Gateway {
            backend: "Docker MCP".into(),
        },
        exposure: Vec::new(),
        origin: Origin::Gateway {
            backend: "docker".into(),
            id: profile.to_string(),
        },
        notes: Vec::new(),
    };
    super::spec::check(&spec)?;
    Ok(spec)
}

/// `docker mcp profile show <id> --format json` through the copied
/// allowlist, to confirm the profile exists before an entry names it.
pub fn docker_profile_exists(profile: &str) -> Result<bool, String> {
    profile_exists_with(profile, |args| {
        docker_mcp::docker_mcp(
            args.iter().map(|a| a.to_string()).collect(),
            None,
            None,
        )
        .map(|_| ())
    })
}

/// Any other failure of `profile show` is checked against `docker mcp
/// version` (on the copied allowlist): with no MCP plugin, docker reads
/// `--format` as its own and prints `unknown flag: --format` and its usage,
/// which told the box test nothing (BX5).
fn profile_exists_with(
    profile: &str,
    docker: impl Fn(&[&str]) -> Result<(), String>,
) -> Result<bool, String> {
    match docker(&["mcp", "profile", "show", profile, "--format", "json"]) {
        Ok(()) => Ok(true),
        Err(e) if e.contains("not found") => Ok(false),
        Err(e) => match docker(&["mcp", "version"]) {
            Ok(()) => Err(e),
            Err(v) => Err(format!(
                "the `docker mcp` plugin is not installed or does not run here (`docker mcp version`: {}); install Docker's MCP Toolkit plugin, or leave out --check-profile",
                v.lines().next().unwrap_or("failed")
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_docker_mcp_plugin_is_named_as_such() {
        // Box test w1, BX5: Docker Engine 29.9.2 with no `docker mcp`.
        let no_plugin = |args: &[&str]| -> Result<(), String> {
            Err(match args {
                ["mcp", "version"] => "docker: unknown command: docker mcp\n\nRun 'docker --help'".into(),
                _ => "unknown flag: --format\nSee 'docker --help'.".into(),
            })
        };
        let e = profile_exists_with("tk_box", no_plugin).unwrap_err();
        assert!(e.contains("`docker mcp` plugin is not installed"), "{e}");
        assert!(e.contains("unknown command: docker mcp"), "{e}");
        assert!(!e.contains("--help"), "only the first line: {e}");
        // The plugin present: a missing profile, an existing one, another error.
        let with = |show: Result<(), String>| {
            move |args: &[&str]| -> Result<(), String> {
                match args {
                    ["mcp", "version"] => Ok(()),
                    _ => show.clone(),
                }
            }
        };
        assert_eq!(profile_exists_with("p", with(Err("profile p not found".into()))), Ok(false));
        assert_eq!(profile_exists_with("p", with(Ok(()))), Ok(true));
        assert_eq!(
            profile_exists_with("p", with(Err("daemon not running".into()))),
            Err("daemon not running".into())
        );
    }

    #[test]
    fn stdio_gateway_entry() {
        let s = docker_spec("tk_github", None, None, None).unwrap();
        assert_eq!(s.name, "github");
        assert_eq!(
            s.transport,
            Transport::Stdio {
                command: "docker".into(),
                args: vec![
                    "mcp".into(),
                    "gateway".into(),
                    "run".into(),
                    "--profile".into(),
                    "tk_github".into()
                ]
            }
        );
    }

    #[test]
    fn http_gateway_entry_with_a_bearer_reference() {
        let s = docker_spec(
            "tk_all",
            Some("gateway"),
            Some("http://127.0.0.1:8811/mcp"),
            Some("TK_GATEWAY_TOKEN"),
        )
        .unwrap();
        assert_eq!(s.name, "gateway");
        assert_eq!(s.variables(), vec!["TK_GATEWAY_TOKEN".to_string()]);
    }

    #[test]
    fn refuses_bad_profiles_names_and_hosts() {
        assert!(docker_spec("Bad-Id", None, None, None).is_err());
        assert!(docker_spec("tk_x", Some("X_Y"), None, None).is_err());
        assert!(docker_spec("tk_x", None, Some("http://10.0.0.5:8811/mcp"), None).is_err());
        assert!(docker_spec("tk_x", None, None, Some("TOKEN")).is_err());
        assert_eq!(name_from_profile("tk_"), "tk-");
        assert_eq!(name_from_profile("my_profile"), "my-profile");
    }
}

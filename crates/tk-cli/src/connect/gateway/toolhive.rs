//! ToolHive (`thv`) backend: argument shapes only in wave 1 (lane card C:
//! "measured in the box test, not built past the argument shapes"). Nothing
//! here runs `thv`; `tk connect` prints the entry and refuses `--write`
//! until the box test measures it.
//!
//! Read in stacklok/toolhive at v0.58.0 (lane C research, V):
//! - `thv proxy stdio WORKLOAD-NAME` connects stdin and stdout to a running
//!   workload, no other flags (`docs/cli/thv_proxy_stdio.md`).
//! - `thv client register` writes a URL to ToolHive's local proxy into a
//!   client, never a command: `http://<host>:<port>/mcp` for streamable
//!   HTTP and stdio workloads (`pkg/client/config.go#L1353-L1405`,
//!   `pkg/transport/url.go#L21-L66`). Host defaults to 127.0.0.1; the port
//!   is `--proxy-port` or random in 10000 to 65535, so tk takes it as
//!   `--url` rather than guessing.

use super::super::spec::{check, HttpKind, Origin, Runner, ServerSpec, Transport};

/// ToolHive workload names: tk accepts `^[a-z0-9][a-z0-9_.-]{0,63}$`
/// (narrower than ToolHive may allow; NF for ToolHive's own rule).
fn valid_workload(w: &str) -> bool {
    let b = w.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(c))
}

pub fn toolhive_spec(
    workload: &str,
    name: Option<&str>,
    url: Option<&str>,
) -> Result<ServerSpec, String> {
    if !valid_workload(workload) {
        return Err(format!(
            "workload name `{workload}` must match ^[a-z0-9][a-z0-9_.-]{{0,63}}$"
        ));
    }
    let name = name.map_or_else(|| workload.replace(['_', '.'], "-"), str::to_string);
    let transport = match url {
        Some(u) => Transport::Http {
            kind: HttpKind::Streamable,
            url: u.to_string(),
            headers: Vec::new(),
        },
        None => Transport::Stdio {
            command: "thv".into(),
            args: vec!["proxy".into(), "stdio".into(), workload.to_string()],
        },
    };
    let spec = ServerSpec {
        name,
        transport,
        env: Vec::new(),
        runner: Runner::Gateway {
            backend: "ToolHive".into(),
        },
        exposure: Vec::new(),
        origin: Origin::Gateway {
            backend: "toolhive".into(),
            id: workload.to_string(),
        },
        notes: vec!["ToolHive backend: argument shapes only; measured in the box test".into()],
    };
    check(&spec)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_proxy_and_url_shapes() {
        let s = toolhive_spec("fetch", None, None).unwrap();
        assert_eq!(
            s.transport,
            Transport::Stdio {
                command: "thv".into(),
                args: vec!["proxy".into(), "stdio".into(), "fetch".into()]
            }
        );
        let u = toolhive_spec("fetch", Some("fetch"), Some("http://127.0.0.1:43210/mcp")).unwrap();
        assert!(matches!(u.transport, Transport::Http { .. }));
        assert!(toolhive_spec("-rm", None, None).is_err());
        assert!(toolhive_spec("fetch", None, Some("http://example.com/mcp")).is_err());
    }
}

// Exports of a catalog set (lane E, T3.4), in the formats harnesses already
// read (note AP section 8.2):
//   claude/      a Claude Code and Codex plugin marketplace root
//                (.claude-plugin/marketplace.json plus one generated plugin
//                folder per server), from code.claude.com marketplace-reference
//   pi/          a Pi package per bundle (package.json with the `pi` field,
//                docs/packages.md at v1.4.0) and an mcp.json fragment of the
//                servers (docs/mcp.md at v1.4.0)
//   docker/      a gateway profile, version 1 (docker/mcp-gateway at 7e0f342,
//                pkg/workingset/workingset.go), as YAML and JSON
//   toolhive/    a registry in ToolHive's upstream format (stacklok/toolhive-
//                core upstream-registry.schema.json, read 27 Oct 2026)
//   registry.json the official registry's list shape (openapi.yaml ServerList)
// Every export is a pure function of the catalog; nothing is fetched.
import type { Catalog, LoadedEntry, TkMeta } from "../validate/types.ts";
import { getMeta, isRecord } from "../validate/types.ts";
import { toYaml, type YamlValue } from "./yaml.ts";

export type ExportFile = { path: string; content: string };

export type ExportOptions = {
  /** The commit the exports pin git sources to (a full 40-character sha). */
  sha: string;
  /** The git URL of this repository. */
  repoUrl?: string;
  /** Timestamp for the fields that carry one. */
  now?: string;
};

const DEFAULT_REPO = "https://github.com/aetiusgular/tk.git";
export const PLACEHOLDER_SHA = "0".repeat(40);

function json(v: unknown): string {
  return JSON.stringify(v, null, 2) + "\n";
}

function setEntries(catalog: Catalog, setId: string): { set: { title: string; description: string; entries: string[] }; entries: LoadedEntry[] } {
  const set = catalog.sets.sets[setId];
  if (!set) throw new Error(`no set ${setId}`);
  const byId = new Map(catalog.entries.map((e) => [e.id, e]));
  const entries = set.entries.map((id) => {
    const e = byId.get(id);
    if (!e) throw new Error(`set ${setId} names ${id}, which is not in the catalog`);
    return e;
  });
  return { set, entries };
}

function metaOf(e: LoadedEntry): TkMeta {
  const m = getMeta(e.kind, e.data);
  if (!m) throw new Error(`${e.id}: no tk meta`);
  return m;
}

/**
 * The environment variable a placeholder or a valueless secret header reads,
 * named as tk connect names it (crates/tk-cli/src/connect/spec.rs
 * `env_name_for`): upper case, anything not a letter or digit becomes `_`.
 */
export function envNameFor(name: string): string {
  return name.replace(/[^A-Za-z0-9]/g, "_").toUpperCase();
}

/**
 * A header in a harness's reference form (`${VAR}`), from upstream's
 * placeholder form (integration w1): a value `prefix{var}` becomes
 * `prefix${VAR}`, a secret header with no value becomes `${NAME}`, a plain
 * literal stays. Anything else (more than one placeholder, a placeholder
 * mid-value) is left out, as tk connect refuses it.
 */
function headerValue(h: Record<string, unknown>): string | undefined {
  const name = h["name"] as string;
  const value = h["value"];
  if (value === undefined) return h["isSecret"] === true ? `\${${envNameFor(name)}}` : undefined;
  if (typeof value !== "string") return undefined;
  const m = /^([^{}$]*)\{([A-Za-z_][A-Za-z0-9_]*)\}$/.exec(value);
  if (m) return `${m[1]}\${${envNameFor(m[2]!)}}`;
  return value.includes("{") || value.includes("$") ? undefined : value;
}

/** The first remote of a server entry, or undefined. */
function remoteOf(e: LoadedEntry): { url: string; headers: { name: string; value: string }[] } | undefined {
  const remotes = e.data["remotes"];
  if (!Array.isArray(remotes) || !isRecord(remotes[0]) || typeof remotes[0]["url"] !== "string") return undefined;
  const headers = Array.isArray(remotes[0]["headers"])
    ? (remotes[0]["headers"] as unknown[])
        .filter(isRecord)
        .filter((h) => typeof h["name"] === "string")
        .flatMap((h) => {
          const value = headerValue(h);
          return value === undefined ? [] : [{ name: h["name"] as string, value }];
        })
    : [];
  return { url: remotes[0]["url"], headers };
}

/** The first package of a server entry as a command line, or undefined. */
function commandOf(e: LoadedEntry): { command: string; args: string[]; env: Record<string, string> } | undefined {
  const packages = e.data["packages"];
  if (!Array.isArray(packages) || !isRecord(packages[0])) return undefined;
  const p = packages[0];
  const identifier = String(p["identifier"] ?? "");
  const type = String(p["registryType"] ?? "");
  const hint = typeof p["runtimeHint"] === "string" ? p["runtimeHint"] : undefined;
  const args: string[] = [];
  const argList = (key: string) => (Array.isArray(p[key]) ? (p[key] as unknown[]).filter(isRecord) : []);
  for (const a of argList("runtimeArguments")) args.push(...argStrings(a));
  let command: string;
  if (type === "npm") {
    command = hint ?? "npx";
    args.push("-y", typeof p["version"] === "string" ? `${identifier}@${p["version"]}` : identifier);
  } else if (type === "pypi") {
    command = hint ?? "uvx";
    args.push(typeof p["version"] === "string" ? `${identifier}==${p["version"]}` : identifier);
  } else if (type === "oci") {
    command = "docker";
    args.push("run", "-i", "--rm", identifier);
  } else {
    // host: the identifier is the executable.
    command = identifier;
  }
  for (const a of argList("packageArguments")) args.push(...argStrings(a));
  const env: Record<string, string> = {};
  for (const v of argList("environmentVariables")) {
    const name = v["name"];
    if (typeof name !== "string") continue;
    // Only a default the library set, or a ${VAR} reference; never a literal secret.
    if (typeof v["default"] === "string" && !v["isSecret"]) env[name] = v["default"];
    else env[name] = `\${${name}}`;
  }
  return { command, args, env };
}

function argStrings(a: Record<string, unknown>): string[] {
  if (a["type"] === "named" && typeof a["name"] === "string") {
    return typeof a["value"] === "string" ? [a["name"], a["value"]] : [a["name"]];
  }
  if (typeof a["value"] === "string") return [a["value"]];
  return [];
}

// ---- Claude Code and Codex marketplace

export function exportMarketplace(catalog: Catalog, setId: string, opts: ExportOptions): ExportFile[] {
  const { set, entries } = setEntries(catalog, setId);
  const files: ExportFile[] = [];
  const plugins: Record<string, unknown>[] = [];
  for (const e of entries) {
    const meta = metaOf(e);
    if (e.kind === "bundle") {
      plugins.push({
        name: e.id,
        description: String(e.data["description"] ?? ""),
        version: String(e.data["version"] ?? "0.0.0"),
        source: { source: "git-subdir", url: opts.repoUrl ?? DEFAULT_REPO, path: `library/catalog/${e.id}`, ref: "main", sha: opts.sha },
        strict: true,
        metadata: { "io.github.aetiusgular.tk": { kind: meta.kind, tier: meta.tier, runner: meta.runner } },
      });
    } else if (e.kind === "server") {
      const server = pluginMcpServer(e, meta);
      if (!server) continue;
      const manifest = {
        name: e.id,
        version: String(e.data["version"] ?? "0.0.0"),
        description: String(e.data["description"] ?? "") + (meta.runner.kind === "host" ? " Runs as you, outside every sandbox." : ""),
        author: { name: "tk library export" },
        license: "MIT",
        mcpServers: { [e.id]: server },
        metadata: { "io.github.aetiusgular.tk": meta },
      };
      files.push({ path: `claude/plugins/${e.id}/.claude-plugin/plugin.json`, content: json(manifest) });
      plugins.push({
        name: e.id,
        description: manifest.description,
        version: manifest.version,
        source: `./plugins/${e.id}`,
        category: "connector",
        tags: [meta.runner.kind, meta.tier],
      });
    }
    // Skills travel inside their bundle; a skill entry on its own is not a plugin.
  }
  const marketplace = {
    // One marketplace per set: a user registers one marketplace per name.
    name: `tk-${setId}`,
    description: `${set.title}: ${set.description}`,
    owner: { name: "Aetius Gular", url: "https://github.com/aetiusgular" },
    metadata: { version: "0.1.0" },
    plugins,
  };
  files.unshift({ path: "claude/.claude-plugin/marketplace.json", content: json(marketplace) });
  return files;
}

/** The mcpServers entry Claude Code's plugin.json takes (code.claude.com/docs/en/plugins/manifest-reference, as session 0 read it). */
function pluginMcpServer(e: LoadedEntry, meta: TkMeta): Record<string, unknown> | undefined {
  const remote = remoteOf(e);
  if (remote) {
    const headers: Record<string, string> = {};
    for (const h of remote.headers) headers[h.name] = h.value;
    return Object.keys(headers).length ? { type: "http", url: remote.url, headers } : { type: "http", url: remote.url };
  }
  const cmd = commandOf(e);
  if (!cmd) return undefined;
  if (meta.runner.kind !== "host" && meta.runner.kind !== "image" && meta.runner.kind !== "srt") return undefined;
  return Object.keys(cmd.env).length ? { command: cmd.command, args: cmd.args, env: cmd.env } : { command: cmd.command, args: cmd.args };
}

// ---- Pi

export function exportPi(catalog: Catalog, setId: string, opts: ExportOptions): ExportFile[] {
  const { set, entries } = setEntries(catalog, setId);
  const files: ExportFile[] = [];
  const mcpServers: Record<string, unknown> = {};
  for (const e of entries) {
    const meta = metaOf(e);
    if (e.kind === "bundle") {
      const pkg = {
        name: `@aetiusgular/${e.id}`,
        version: String(e.data["version"] ?? "0.0.0"),
        description: String(e.data["description"] ?? ""),
        license: "MIT",
        keywords: ["pi-package"],
        pi: { skills: ["./skills"] },
      };
      files.push({ path: `pi/${e.id}/package.json`, content: json(pkg) });
      files.push({
        path: `pi/${e.id}/README.md`,
        content: `# ${e.id}\n\nA Pi package of the skills in the tk bundle \`${e.id}\`. Install with \`pi install git:github.com/aetiusgular/tk@${opts.sha.slice(0, 12)}\` once the package sits at a repository root, or copy this folder beside the bundle's \`skills/\` and run \`pi install ./${e.id}\`.\n\nThe skills come from \`library/catalog/${e.id}/skills/\` at ${opts.sha}.\n`,
      });
    } else if (e.kind === "server") {
      const exposure = meta.exposure["pi"] ?? meta.exposure.default;
      const remote = remoteOf(e);
      const base: Record<string, unknown> = { description: String(e.data["description"] ?? ""), exposure };
      if (remote) {
        const headers: Record<string, string> = {};
        for (const h of remote.headers) headers[h.name] = h.value;
        mcpServers[e.id] = Object.keys(headers).length ? { url: remote.url, headers, ...base } : { url: remote.url, ...base };
      } else {
        const cmd = commandOf(e);
        if (!cmd) continue;
        mcpServers[e.id] = Object.keys(cmd.env).length ? { command: cmd.command, args: cmd.args, env: cmd.env, ...base } : { command: cmd.command, args: cmd.args, ...base };
      }
    }
  }
  files.push({ path: "pi/mcp.json", content: json({ mcpServers }) });
  files.push({
    path: "pi/README.md",
    content: `# Pi export of the set \`${setId}\` (${set.title})\n\n\`mcp.json\` is a fragment of \`~/.pi/agent/mcp.json\` (Pi docs/mcp.md at v1.4.0): every server of the set with its description and exposure. \`tk connect pi\` (lane C) is what writes it; by hand, merge the \`mcpServers\` entries and run \`pi mcp list\`. Secrets stay \`\${VAR}\` references. Each bundle folder is a Pi package (\`pi install ./<bundle>\` beside the bundle's skills).\n`,
  });
  return files;
}

// ---- Docker gateway profile

export function exportDockerProfile(catalog: Catalog, setId: string): ExportFile[] {
  const { set, entries } = setEntries(catalog, setId);
  const servers: Record<string, YamlValue>[] = [];
  const skipped: string[] = [];
  for (const e of entries) {
    if (e.kind !== "server") continue;
    const meta = metaOf(e);
    const toolNames = e.tools?.tools.map((t) => t.name) ?? [];
    const remote = remoteOf(e);
    if (meta.runner.kind === "image") {
      const server: Record<string, YamlValue> = { type: "image", image: meta.runner.imageDigest ? `${meta.runner.image.split("@")[0]}@${meta.runner.imageDigest}` : meta.runner.image };
      if (toolNames.length) server["tools"] = toolNames;
      server["snapshot"] = { server: { name: e.id, type: "server", image: meta.runner.image, description: String(e.data["description"] ?? ""), allowHosts: hostsWithPorts(meta) } };
      servers.push(server);
    } else if (remote) {
      const server: Record<string, YamlValue> = { type: "remote", endpoint: remote.url };
      if (toolNames.length) server["tools"] = toolNames;
      servers.push(server);
    } else {
      skipped.push(e.id);
    }
  }
  const profile: Record<string, YamlValue> = { version: 1, id: `tk-${setId}`, name: `tk: ${set.title}`, servers };
  const readme = `# Docker MCP Gateway profile for the set \`${setId}\`\n\nImport: \`docker mcp profile import ./profile.yaml\` (or \`profile.json\`), then \`docker mcp gateway run --profile tk-${setId}\` or \`docker mcp client connect <client> --profile tk-${setId}\`. The profiles feature must be on (\`docker mcp feature enable profiles\`; Docker Desktop: the MCPWorkingSets flag). Format: version 1 of docker/mcp-gateway pkg/workingset at 7e0f342 (27 Oct 2026): image servers carry a snapshot so the import needs no pull; remote servers import without one; \`tools\` lists the recorded tool names so the gateway exposes nothing else.\n\n${skipped.length ? `Not in the profile (they run as you, outside the gateway): ${skipped.join(", ")}.\n` : ""}Secrets are not in the profile: the gateway's secret store holds them (\`docker mcp secret set\`).\n`;
  return [
    { path: "docker/profile.yaml", content: toYaml(profile) },
    { path: "docker/profile.json", content: json(profile) },
    { path: "docker/README.md", content: readme },
  ];
}

function hostsWithPorts(meta: TkMeta): string[] {
  const out = meta.permissions?.network?.outbound;
  if (!out?.allow_host) return [];
  const ports = out.allow_port && out.allow_port.length ? out.allow_port : [443];
  return out.allow_host.flatMap((h) => ports.map((p) => `${h}:${p}`));
}

// ---- ToolHive-shaped registry

export function exportToolhive(catalog: Catalog, setId: string, opts: ExportOptions): ExportFile[] {
  const { entries } = setEntries(catalog, setId);
  const servers = entries.filter((e) => e.kind === "server").map((e) => e.data);
  const skills = entries.filter((e) => e.kind === "skill").map((e) => e.data);
  const registry = {
    $schema: "https://raw.githubusercontent.com/stacklok/toolhive-core/main/registry/types/data/upstream-registry.schema.json",
    version: "1.0.0",
    meta: { last_updated: opts.now ?? new Date().toISOString().replace(/\.\d{3}Z$/, "Z") },
    data: { servers, skills },
  };
  return [{ path: "toolhive/registry.json", content: json(registry) }];
}

// ---- The official registry's list shape

export function exportRegistry(catalog: Catalog, setId: string, opts: ExportOptions): ExportFile[] {
  const { entries } = setEntries(catalog, setId);
  const now = opts.now ?? new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
  const servers = entries
    .filter((e) => e.kind === "server")
    .map((e) => ({
      server: e.data,
      _meta: { "io.modelcontextprotocol.registry/official": { status: metaOf(e).status, publishedAt: now, updatedAt: now, isLatest: true } },
    }));
  return [{ path: "registry.json", content: json({ servers, metadata: { count: servers.length } }) }];
}

/** All five exports of a set, under `<set>/`. */
export function exportSet(catalog: Catalog, setId: string, opts: ExportOptions = { sha: PLACEHOLDER_SHA }): ExportFile[] {
  const files = [...exportMarketplace(catalog, setId, opts), ...exportPi(catalog, setId, opts), ...exportDockerProfile(catalog, setId), ...exportToolhive(catalog, setId, opts), ...exportRegistry(catalog, setId, opts)];
  return files.map((f) => ({ path: `${setId}/${f.path}`, content: f.content }));
}

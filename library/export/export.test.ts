// Exports of the seed catalog (lane E, T3.4): each format validates against
// the target's own shape where one exists, pins what must be pinned, carries
// no literal secret, and round-trips.
import { parse as parseYaml } from "yaml";
import { describe, expect, it } from "vitest";
import { Ajv2020 } from "ajv/dist/2020.js";
import { exportDockerProfile, exportMarketplace, exportPi, exportRegistry, exportSet, exportToolhive, PLACEHOLDER_SHA, type ExportFile } from "./index.ts";
import { loadBrands, loadCatalog, validateEntry, type LoadedEntry } from "../validate/index.ts";
import { checkMarketplaceShape, checkTkSchema, checkUpstreamServerSchema, CONTRACTS_DIR, readJson, SCHEMA_DIR } from "../validate/schemas.ts";
import { join } from "node:path";

const catalog = loadCatalog();
const brands = loadBrands();
const SHA = "a".repeat(40);
const NOW = "2026-10-27T00:00:00Z";
const file = (files: ExportFile[], suffix: string) => {
  const f = files.find((x) => x.path.endsWith(suffix));
  if (!f) throw new Error(`no export ${suffix} in ${files.map((x) => x.path).join(", ")}`);
  return f;
};
const parse = (files: ExportFile[], suffix: string) => JSON.parse(file(files, suffix).content) as Record<string, unknown>;

/** Session 0's plugin.json subset (contracts/shapes/plugin.schema.json), for the generated plugin folders. */
function pluginShape(data: unknown): string[] {
  const ajv = new Ajv2020({ allErrors: true, strict: true, validateFormats: false });
  ajv.addSchema(readJson(join(CONTRACTS_DIR, "entry.schema.json")) as object);
  const v = ajv.compile(readJson(join(CONTRACTS_DIR, "shapes/plugin.schema.json")) as object);
  return v(data) ? [] : (v.errors ?? []).map((e) => `${e.instancePath} ${e.message}`);
}

describe("marketplace export (Claude Code and Codex)", () => {
  const files = exportMarketplace(catalog, "tony-connectors", { sha: SHA });
  const m = parse(files, ".claude-plugin/marketplace.json") as { name: string; plugins: { name: string; source: unknown }[] };

  it("validates against the contract's marketplace shape and names no reserved marketplace", () => {
    expect(checkMarketplaceShape(m)).toEqual([]);
    expect(m.name).toBe("tk-tony-connectors");
    expect(["claude-code-marketplace", "claude-plugins-official", "claude-community", "npm", "github", "gh"]).not.toContain(m.name);
  });
  it("pins every git source to a full sha and lists the bundle and the six servers", () => {
    const bundle = m.plugins.find((p) => p.name === "tk-connectors")!;
    expect(bundle.source).toEqual({ source: "git-subdir", url: "https://github.com/aetiusgular/tk.git", path: "library/catalog/tk-connectors", ref: "main", sha: SHA });
    expect(m.plugins.map((p) => p.name).sort()).toEqual(["context7", "exa", "granola", "hugging-face", "shopify-dev", "tk-connectors", "vercel"]);
    for (const p of m.plugins) if (typeof p.source === "string") expect(p.source).toMatch(/^\.\/plugins\/[a-z0-9-]+$/);
  });
  it("refuses an unpinned source: the contract shape requires sha", () => {
    const unpinned = { ...m, plugins: [{ name: "x", source: { source: "github", repo: "a/b" } }] };
    expect(checkMarketplaceShape(unpinned)).not.toEqual([]);
  });
  it("every generated plugin folder is a plugin.json with one mcpServers entry and no literal secret", () => {
    const plugins = files.filter((f) => f.path.includes("/plugins/"));
    expect(plugins).toHaveLength(6);
    for (const p of plugins) {
      const data = JSON.parse(p.content) as { name: string; description: string; mcpServers: Record<string, { type?: string; url?: string; command?: string; headers?: Record<string, string>; env?: Record<string, string> }> };
      expect(pluginShape(data)).toEqual([]);
      const [name, server] = Object.entries(data.mcpServers)[0]!;
      expect(name).toBe(data.name);
      for (const v of Object.values({ ...server.headers, ...server.env })) expect(v).toMatch(/^(Bearer )?\$\{[A-Z0-9_]+\}$|^(true|1)$/);
      if (server.command) expect(data.description).toContain("Runs as you");
    }
  });
  // Integration w1: the catalog holds upstream placeholders; each export
  // writes the target's reference, named as tk connect names it (C's
  // env_name_for: upper case, anything else `_`), so a key set once serves both.
  it("upstream placeholders become ${VAR} references with tk connect's names", () => {
    const h = (id: string) => (parse(files, `plugins/${id}/.claude-plugin/plugin.json`) as { mcpServers: Record<string, { headers?: Record<string, string> }> }).mcpServers[id]!.headers;
    expect(h("exa")).toEqual({ "x-api-key": "${EXA_API_KEY}" });
    expect(h("hugging-face")).toEqual({ Authorization: "Bearer ${HF_TOKEN}" });
    expect(h("context7")).toEqual({ CONTEXT7_API_KEY: "${CONTEXT7_API_KEY}" });
  });
  it("the Shopify plugin keeps the telemetry opt-out", () => {
    const shopify = parse(files, "plugins/shopify-dev/.claude-plugin/plugin.json") as { mcpServers: Record<string, { env: Record<string, string> }> };
    expect(shopify.mcpServers["shopify-dev"]!.env).toEqual({ OPT_OUT_INSTRUMENTATION: "true", DO_NOT_TRACK: "1" });
  });
});

describe("Pi export", () => {
  const files = exportPi(catalog, "tony-connectors", { sha: SHA });
  it("one package per bundle with the pi-package keyword and the pi field (docs/packages.md at v1.4.0)", () => {
    const pkg = parse(files, "pi/tk-connectors/package.json") as { keywords: string[]; pi: { skills: string[] } };
    expect(pkg.keywords).toContain("pi-package");
    expect(pkg.pi.skills).toEqual(["./skills"]);
  });
  it("an mcp.json fragment with exposure per server and references only (docs/mcp.md at v1.4.0)", () => {
    const mcp = parse(files, "pi/mcp.json") as { mcpServers: Record<string, { url?: string; command?: string; args?: string[]; exposure: string; headers?: Record<string, string>; description: string }> };
    expect(Object.keys(mcp.mcpServers).sort()).toEqual(["context7", "exa", "granola", "hugging-face", "shopify-dev", "vercel"]);
    for (const [name, s] of Object.entries(mcp.mcpServers)) {
      expect(name).toMatch(/^[A-Za-z0-9_-]+$/);
      expect(["codemode", "deferred", "direct", "hidden"]).toContain(s.exposure);
      expect(s.description.length).toBeGreaterThan(0);
      expect(Boolean(s.url) !== Boolean(s.command)).toBe(true);
      for (const v of Object.values(s.headers ?? {})) expect(v).toMatch(/\$\{[A-Z0-9_]+\}/);
    }
    expect(mcp.mcpServers["shopify-dev"]).toMatchObject({ command: "npx", args: ["-y", "@shopify/dev-mcp@1.17.0"] });
  });
});

describe("Docker profile export", () => {
  const files = exportDockerProfile(catalog, "tony-connectors");
  const yamlText = file(files, "docker/profile.yaml").content;
  const jsonProfile = parse(files, "docker/profile.json");

  it("is version 1 and validates against the working-set shape of mcp-gateway 7e0f342", () => {
    expect(jsonProfile["version"]).toBe(1);
    expect(checkTkSchema("docker-profile", jsonProfile)).toEqual([]);
  });
  it("the YAML and the JSON are one profile (round trip through yaml 2.9.1)", () => {
    expect(parseYaml(yamlText)).toEqual(jsonProfile);
  });
  it("remotes become type remote with the endpoint; host runners stay out and are named in the README", () => {
    const servers = jsonProfile["servers"] as { type: string; endpoint?: string; tools?: string[] }[];
    expect(servers.map((s) => s.type)).toEqual(["remote", "remote", "remote", "remote", "remote"]);
    expect(servers.map((s) => s.endpoint)).toContain("https://mcp.exa.ai/mcp");
    expect(file(files, "docker/README.md").content).toContain("shopify-dev");
  });
  it("an image runner carries a snapshot so import needs no pull", () => {
    const withImage = loadCatalog();
    const exa = withImage.entries.find((e) => e.id === "exa")!;
    const meta = (exa.data["_meta"] as Record<string, Record<string, Record<string, unknown>>>)["io.modelcontextprotocol.registry/publisher-provided"]!["io.github.aetiusgular.tk"]!;
    meta["runner"] = { kind: "image", image: "mcp/exa", imageDigest: "sha256:" + "b".repeat(64) };
    const p = JSON.parse(file(exportDockerProfile(withImage, "tony-connectors"), "docker/profile.json").content) as { servers: { type: string; image?: string; snapshot?: { server: { name: string; type: string; allowHosts: string[] } } }[] };
    const image = p.servers.find((s) => s.type === "image")!;
    expect(image.image).toBe("mcp/exa@sha256:" + "b".repeat(64));
    expect(image.snapshot?.server).toMatchObject({ name: "exa", type: "server", allowHosts: ["mcp.exa.ai:443"] });
    expect(checkTkSchema("docker-profile", p)).toEqual([]);
  });
});

describe("ToolHive-shaped registry and the official list shape", () => {
  const thv = parse(exportToolhive(catalog, "tony-connectors", { sha: SHA, now: NOW }), "toolhive/registry.json") as { $schema: string; version: string; meta: { last_updated: string }; data: { servers: Record<string, unknown>[]; skills: Record<string, unknown>[] } };
  const reg = parse(exportRegistry(catalog, "tony-connectors", { sha: SHA, now: NOW }), "registry.json") as { servers: { server: Record<string, unknown>; _meta: Record<string, { status: string; isLatest: boolean }> }[]; metadata: { count: number } };

  it("the ToolHive envelope has the fields its schema requires, every server passes upstream's schema, every skill ToolHive's skill schema", () => {
    const envelope = readJson(join(SCHEMA_DIR, "thv-registry.schema.json")) as { required: string[] };
    for (const k of envelope.required) expect(thv).toHaveProperty(k);
    expect(thv.version).toBe("1.0.0");
    expect(thv.meta.last_updated).toBe(NOW);
    expect(thv.data.servers).toHaveLength(6);
    expect(thv.data.skills).toHaveLength(5);
    for (const s of thv.data.servers) expect(checkUpstreamServerSchema(s)).toEqual([]);
    for (const s of thv.data.skills) expect(checkTkSchema("thv-skill", s)).toEqual([]);
  });
  it("the official list shape carries status and isLatest per server (openapi.yaml ServerList)", () => {
    expect(reg.metadata.count).toBe(6);
    for (const s of reg.servers) {
      expect(checkUpstreamServerSchema(s.server)).toEqual([]);
      expect(s._meta["io.modelcontextprotocol.registry/official"]).toMatchObject({ status: "active", isLatest: true });
    }
  });
  it("round trip: every exported server re-validates as a catalog entry", () => {
    for (const s of reg.servers) {
      const id = (s.server["name"] as string).split("/")[1]!;
      const original = catalog.entries.find((e) => e.id === id)!;
      const e: LoadedEntry = { id, kind: "server", dir: original.dir, file: "server.json", data: s.server };
      if (original.tools) e.tools = original.tools;
      expect(validateEntry(e, { brands }).filter((f) => f.severity === "error")).toEqual([]);
    }
  });
});

describe("exportSet", () => {
  it("writes every format under the set's folder and uses the placeholder sha only when none is given", () => {
    const files = exportSet(catalog, "tk-own");
    const paths = files.map((f) => f.path);
    for (const p of ["tk-own/claude/.claude-plugin/marketplace.json", "tk-own/pi/mcp.json", "tk-own/docker/profile.yaml", "tk-own/docker/profile.json", "tk-own/toolhive/registry.json", "tk-own/registry.json"]) expect(paths).toContain(p);
    expect(file(files, "pi/README.md").content).not.toContain(PLACEHOLDER_SHA);
    expect(() => exportSet(catalog, "no-such-set")).toThrow(/no set/);
  });
  it("tk's own servers run as you: the Docker profile lists none of them and the plugins say so", () => {
    const files = exportSet(catalog, "tk-own");
    expect((parse(files, "docker/profile.json")["servers"] as unknown[]).length).toBe(0);
    expect(file(files, "docker/README.md").content).toContain("tk-computer-use, docker-mcp-gateway");
    expect(file(files, "plugins/tk-computer-use/.claude-plugin/plugin.json").content).toContain("Runs as you");
  });
});

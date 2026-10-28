#!/usr/bin/env node
// Generates library/trust/brands.directory.json and brands.docker.json (lane H, T4.3).
//
//   node library/trust/gen/gen-brands.mjs --directory <servers.json> --docker <mcp-registry clone> [--out <dir>]
//
// <servers.json>: every page of
//   GET https://api.anthropic.com/mcp-registry/v0/servers?version=latest&visibility=commercial&limit=100
// joined into one JSON array (Anthropic's connector directory as a registry;
// read 28 Oct 2026, 327 entries). <mcp-registry clone>: docker/mcp-registry,
// whose HEAD commit is recorded.
//
// A record becomes a brand only when its owner and its name belong together,
// so a provider listing another organisation's data, or a community server
// under a company's name, does not hold that name:
//   - directory: the namespace's owner label (`com.atlassian` -> `atlassian`)
//     appears in the title's skeleton, or the title's in the label;
//   - Docker: the `source.project` GitHub owner and the server name contain one
//     another (`stripe/agent-toolkit` for `stripe`; not `sooperset/mcp-atlassian`
//     for `atlassian`), or a remote's host carries the name
//     (`mcp.atlassian.com` for `atlassian-remote`).
// Docker's catalog marks no owner; this pairing is the only ownership signal
// in it (research, status doc). Everything skipped is counted in the output.
//
// Reads files only; no network, no program run. Input is data: parsed as
// JSON and YAML, never evaluated.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import YAML from "yaml";

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const opt = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const directoryPath = opt("--directory");
const dockerDir = opt("--docker");
const outDir = resolve(opt("--out") ?? join(here, ".."));
if (!directoryPath || !dockerDir) {
  console.error("usage: gen-brands.mjs --directory <servers.json> --docker <mcp-registry clone> [--out <dir>]");
  process.exit(2);
}

/** Lowercase letters and digits only: enough to pair names here (the
 *  lookalike check itself uses the gate's fold, library/trust/fold.ts). */
const flat = (s) => s.normalize("NFKD").toLowerCase().replace(/[^a-z0-9]/g, "");
const slug = (s) => s.normalize("NFKD").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
const together = (a, b) => a.length >= 3 && b.length >= 3 && (a.includes(b) || b.includes(a));

/** Public suffixes common in these namespaces; the label after one is the owner. */
const SUFFIX = new Set(["com", "io", "ai", "app", "co", "dev", "net", "org", "so", "us", "uk", "de", "fr", "cloud", "tech", "inc", "xyz", "me", "sh"]);
function ownerLabel(namespace) {
  const parts = namespace.toLowerCase().split(".");
  if (parts[0] === "io" && parts[1] === "github") return parts[2] ?? "";
  let i = 0;
  while (i < parts.length - 1 && SUFFIX.has(parts[i])) i++;
  return parts[i] ?? "";
}

function hostOf(url) {
  if (typeof url !== "string" || url.includes("{")) return undefined;
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return undefined;
  }
}

// --- Anthropic's directory ---------------------------------------------------
const directory = JSON.parse(readFileSync(directoryPath, "utf8"));
if (!Array.isArray(directory)) throw new Error("--directory must be a JSON array of registry server responses");
const dirBrands = [];
const dirSkipped = [];
for (const item of directory) {
  const s = item?.server ?? item;
  if (typeof s?.name !== "string") continue;
  const namespace = s.name.split("/")[0];
  const owner = ownerLabel(namespace);
  const title = typeof s.title === "string" ? s.title : s.name.split("/")[1] ?? "";
  const hosts = [...new Set((s.remotes ?? []).map((r) => hostOf(r?.url)).filter(Boolean))].sort();
  if (!together(flat(owner), flat(title))) {
    dirSkipped.push(`${s.name} (${title})`);
    continue;
  }
  const source = "Anthropic connector directory registry, api.anthropic.com/mcp-registry/v0/servers?visibility=commercial, read 28 Oct 2026";
  const name = slug(title);
  if (name.length >= 2) dirBrands.push({ name, namespaces: [namespace], hosts, source });
  if (slug(owner) !== name && owner.length >= 3) dirBrands.push({ name: slug(owner), namespaces: [namespace], hosts, source });
}

// --- Docker's catalog -----------------------------------------------------------
const commit = execFileSync("git", ["-C", dockerDir, "rev-parse", "HEAD"], { encoding: "utf8" }).trim();
const dockerBrands = [];
const dockerSkipped = [];
const serversDir = join(dockerDir, "servers");
for (const folder of readdirSync(serversDir).sort()) {
  const file = join(serversDir, folder, "server.yaml");
  if (!existsSync(file)) continue;
  const y = YAML.parse(readFileSync(file, "utf8"), { maxAliasCount: 0 }) ?? {};
  const name = slug(String(y.name ?? folder).replace(/-(official|remote)$/, ""));
  const project = typeof y.source?.project === "string" ? y.source.project : "";
  const ghOwner = /^https?:\/\/github\.com\/([^/]+)\//i.exec(project)?.[1]?.toLowerCase();
  const host = hostOf(y.remote?.url);
  const image = typeof y.image === "string" ? y.image : undefined;
  const source = `docker/mcp-registry at ${commit}, servers/${folder}/server.yaml`;
  if (ghOwner && together(flat(ghOwner), flat(name))) {
    dockerBrands.push({ name, namespaces: [`io.github.${ghOwner}`], ...(image ? { images: [image] } : {}), ...(host ? { hosts: [host] } : {}), source });
  } else if (host && flat(host).includes(flat(name)) && flat(name).length >= 3) {
    dockerBrands.push({ name, namespaces: [], hosts: [host], source });
  } else {
    dockerSkipped.push(folder);
  }
}

const write = (file, comment, brands, skipped) => {
  brands.forEach((b) => (b.fuzzy = false));
  const body = { $comment: comment, generated: { by: "library/trust/gen/gen-brands.mjs", kept: brands.length, skipped: skipped.length, skippedNames: skipped }, brands };
  writeFileSync(join(outDir, file), JSON.stringify(body, null, 2) + "\n");
  console.log(`${file}: ${brands.length} brands, ${skipped.length} records skipped`);
};
write("brands.directory.json", "Brands from Anthropic's connector directory (plan T4.3). Generated; do not edit by hand. A record is kept when its namespace's owner label and its title belong together; see gen-brands.mjs.", dirBrands, dirSkipped);
write("brands.docker.json", `Brands from Docker's MCP catalog, docker/mcp-registry at ${commit} (plan T4.3). Generated; do not edit by hand. Docker's catalog marks no owner: a record is kept when its source project's GitHub owner and its name belong together, or its remote host carries the name; see gen-brands.mjs.`, dockerBrands, dockerSkipped);

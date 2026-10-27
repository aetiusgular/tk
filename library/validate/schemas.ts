// Schema loading for the tk library (lane E, T3.1).
//
// Two Ajv instances: the vendored upstream server.schema.json (2025-12-11)
// is draft-07 with `example` keywords, so it gets a draft-07 Ajv with strict
// mode off; everything tk writes is 2020-12 and strict. The contract's
// entry.schema.json is loaded by its $id so the tk schemas can $ref it.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Ajv } from "ajv";
import { Ajv2020 } from "ajv/dist/2020.js";
import type { ValidateFunction } from "ajv";
import type { Finding } from "./types.ts";
import { finding } from "./types.ts";

const here = dirname(fileURLToPath(import.meta.url));
/** library/ */
export const LIBRARY_DIR = join(here, "..");
/** The repository root (library/..). */
export const REPO_ROOT = join(LIBRARY_DIR, "..");
export const SCHEMA_DIR = join(LIBRARY_DIR, "schema");
export const CONTRACTS_DIR = join(REPO_ROOT, "contracts");

export const UPSTREAM_SCHEMA_URL = "https://static.modelcontextprotocol.io/schemas/2025-12-11/server.schema.json";
const TK_BASE = "https://github.com/aetiusgular/tk/library/schema/";

export function readJson(path: string): unknown {
  return JSON.parse(readFileSync(path, "utf8"));
}

let tkAjv: Ajv2020 | undefined;
let upstreamAjv: Ajv | undefined;

function tk(): Ajv2020 {
  if (!tkAjv) {
    tkAjv = new Ajv2020({ allErrors: true, strict: true, validateFormats: false });
    tkAjv.addSchema(readJson(join(CONTRACTS_DIR, "entry.schema.json")) as object);
    for (const f of ["tk-server", "tk-skill", "tk-bundle", "tools", "docker-profile", "sets", "thv-skill", "thv-plugin"]) {
      // The key is the tk URL for the stem; ToolHive's two schemas keep their own $id as well.
      tkAjv.addSchema(readJson(join(SCHEMA_DIR, `${f}.schema.json`)) as object, TK_BASE + `${f}.schema.json`);
    }
    // thv-registry.schema.json is not loaded: it $refs the upstream URL
    // (draft-07) and ToolHive's skill and plugin schemas by URL. The exporter
    // writes the envelope and export.test.ts checks its required fields, each
    // server against upstream's schema and each skill against thv-skill.
    tkAjv.addSchema(readJson(join(CONTRACTS_DIR, "shapes", "marketplace.schema.json")) as object);
  }
  return tkAjv;
}

function upstream(): Ajv {
  if (!upstreamAjv) {
    upstreamAjv = new Ajv({ allErrors: true, strict: false, validateFormats: false });
    upstreamAjv.addSchema(readJson(join(SCHEMA_DIR, "server.schema.json")) as object);
  }
  return upstreamAjv;
}

function errorsToFindings(rule: string, validate: ValidateFunction, data: unknown): Finding[] {
  if (validate(data)) return [];
  return (validate.errors ?? []).map((e) => finding(rule, "error", e.instancePath, `${e.message ?? "invalid"}${e.params && "additionalProperty" in e.params ? ` (${String(e.params["additionalProperty"])})` : ""}`));
}

/** The vendored upstream schema, draft-07. Rule `schema/upstream`. */
export function checkUpstreamServerSchema(data: unknown): Finding[] {
  const v = upstream().getSchema(UPSTREAM_SCHEMA_URL);
  if (!v) throw new Error("upstream schema not loaded");
  return errorsToFindings("schema/upstream", v, data);
}

/** One of tk's own 2020-12 schemas by file stem, e.g. "tk-server". Rule `schema/<stem>`. */
export function checkTkSchema(stem: string, data: unknown): Finding[] {
  const v = tk().getSchema(TK_BASE + `${stem}.schema.json`);
  if (!v) throw new Error(`schema ${stem} not loaded`);
  return errorsToFindings(`schema/${stem}`, v, data);
}

/** The contract's marketplace shape (contracts/shapes/marketplace.schema.json). Rule `schema/marketplace`. */
export function checkMarketplaceShape(data: unknown): Finding[] {
  const v = tk().getSchema("https://github.com/aetiusgular/tk/contracts/shapes/marketplace.schema.json");
  if (!v) throw new Error("marketplace shape not loaded");
  return errorsToFindings("schema/marketplace", v, data);
}

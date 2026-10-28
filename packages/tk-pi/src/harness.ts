// Which of the two Pi-family harnesses loaded this package, and what differs
// between them for the gate (plans/parallel/tk_lane_I.md, Research).
//
// - oh-my-pi (omp) 18.13.3 is a Bun program (`#!/usr/bin/env bun`, engines
//   bun >=1.4.4) and loads this package from its `pi.extensions` field when it
//   has no `omp` field (extensibility/plugins/loader.ts:176-198). Pi 1.4.0 runs
//   on Node. So Bun in the process means omp: the harness id is the preset
//   row's, `oh-my-pi`.
// - omp names an MCP tool `mcp__<server>_<tool>`, single `_` between, and drops
//   a tool's own `<server>_` prefix (mcp/tool-bridge.ts:464-535). The generic
//   normalizer splits that at the first `_`, which misreads a server with `_`
//   in its name; with the policy's `servers` list, the longest known server
//   after `mcp__` is taken instead (note AQ section 6).

export const PI = "pi";
export const OMP = "oh-my-pi";

export function harnessId(globals: Record<string, unknown> = globalThis as unknown as Record<string, unknown>): string {
  return typeof globals.Bun === "object" && globals.Bun !== null ? OMP : PI;
}

/** omp's `mcp__<server>_<tool>` as `mcp__<server>__<tool>` when a known server matches; anything else as it came. */
export function ompToolName(raw: string, servers: readonly string[]): string {
  if (!raw.startsWith("mcp__")) return raw;
  const rest = raw.slice(5);
  if (rest.includes("__")) return raw;
  // omp lowercases and turns runs of other characters into `_` (sanitize).
  const sanitize = (s: string) => s.toLowerCase().replace(/[^a-z0-9_]+/g, "_").replace(/_+/g, "_").replace(/^_|_$/g, "");
  let best: string | undefined;
  for (const s0 of servers) {
    const s = sanitize(s0);
    if (s !== "" && rest.length > s.length + 1 && rest.startsWith(s) && rest[s.length] === "_" && (best === undefined || s.length > best.length)) best = s;
  }
  return best === undefined ? raw : `mcp__${best}__${rest.slice(best.length + 1)}`;
}

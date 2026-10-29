#!/usr/bin/env node
// Lane scope gate for plans/parallel_build_tk.md, section 2, point 6.
//
//   node scripts/lane-scope.mjs <lane> [--base <commit>]
//
// Checks the lane's committed work (merge-base of <base> and HEAD to HEAD)
// against scripts/lane-ownership.json:
//   - every added, changed or deleted path is inside the lane's folders;
//   - a shared file (crates/tk-cli/Cargo.toml) changes only inside the
//     lane's own `# [lane-X]` section, and no marker moves or disappears;
//   - no new test file under packages/ or library/ that vitest would not
//     collect (*.test.tsx, *.test.js, *.test.mjs, *.spec.*).
// Exit 0 clean, 1 violations, 2 usage error. Reads git only; writes nothing.
// Without --base it uses origin/main.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const ownership = JSON.parse(readFileSync(new URL("./lane-ownership.json", import.meta.url), "utf8"));

function usage(message) {
  console.error(`lane-scope: ${message}`);
  console.error("usage: node scripts/lane-scope.mjs <A-R> [--base <commit>]");
  process.exit(2);
}

function git(args) {
  return execFileSync("git", args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
}

const args = process.argv.slice(2);
const lane = (args[0] ?? "").toUpperCase();
if (!ownership.lanes[lane]) usage(`unknown lane ${JSON.stringify(args[0] ?? "")}`);
let base = "origin/main";
for (let i = 1; i < args.length; i++) {
  if (args[i] === "--base" && args[i + 1]) base = args[++i];
  else usage(`unexpected argument ${JSON.stringify(args[i])}`);
}

let mergeBase;
try {
  git(["rev-parse", "--verify", "--quiet", `${base}^{commit}`]);
  mergeBase = git(["merge-base", base, "HEAD"]).trim();
} catch {
  usage(`base ${JSON.stringify(base)} is not a commit here`);
}

const spec = ownership.lanes[lane];
const owned = (path) =>
  spec.owns.some((p) => (p.endsWith("/") ? path.startsWith(p) : path === p)) &&
  !spec.except.some((p) => (p.endsWith("/") ? path.startsWith(p) : path === p));
const ownerOf = (path) => Object.keys(ownership.lanes).filter((l) => {
  const s = ownership.lanes[l];
  return s.owns.some((p) => (p.endsWith("/") ? path.startsWith(p) : path === p)) &&
    !s.except.some((p) => (p.endsWith("/") ? path.startsWith(p) : path === p));
});
const noLane = (path) => ownership.noLane.some((p) => (p.endsWith("/") ? path.startsWith(p) : path === p));

const changes = git(["diff", "--name-status", "--no-renames", "-z", mergeBase, "HEAD"])
  .split("\0")
  .filter(Boolean);
const errors = [];
for (let i = 0; i + 1 < changes.length; i += 2) {
  const status = changes[i];
  const path = changes[i + 1];
  if (ownership.shared[path]) {
    if (!ownership.shared[path].includes(lane)) errors.push(`${path}: shared file, lane ${lane} has no section in it`);
    else if (status !== "M") errors.push(`${path}: shared file ${status === "D" ? "deleted" : "added"}`);
    else checkSections(path);
    continue;
  }
  if (noLane(path)) {
    errors.push(`${path}: no lane edits this during a wave (write it under "Requests for integration")`);
    continue;
  }
  if (!owned(path)) {
    const others = ownerOf(path);
    errors.push(`${path}: outside lane ${lane}${others.length ? ` (owned by lane ${others.join(", ")})` : " (owned by no lane)"}`);
    continue;
  }
  if (status === "A" && /^(packages|library)\//.test(path) && /\.(test|spec)\.(tsx|js|mjs|cjs|jsx)$|\.spec\.ts$/.test(path)) {
    errors.push(`${path}: vitest collects only *.test.ts; this file would never run`);
  }
}

function sections(text) {
  // Returns { outside: string[], byLane: Map<lane, string[]> } where a
  // section starts at a `# [lane-X]` line and ends at the next marker or
  // the next TOML table header.
  const outside = [];
  const byLane = new Map();
  let current = null;
  for (const line of text.split("\n")) {
    const marker = /^# \[lane-([A-Z])\]\s*$/.exec(line);
    if (marker) {
      current = marker[1];
      if (!byLane.has(current)) byLane.set(current, []);
      outside.push(line);
      continue;
    }
    if (/^\[/.test(line)) current = null;
    if (current) byLane.get(current).push(line);
    else outside.push(line);
  }
  return { outside, byLane };
}

function checkSections(path) {
  const before = sections(git(["show", `${mergeBase}:${path}`]));
  const after = sections(git(["show", `HEAD:${path}`]));
  if (before.outside.join("\n") !== after.outside.join("\n")) {
    errors.push(`${path}: changed outside the # [lane-${lane}] sections, or a marker moved`);
  }
  for (const [l, lines] of before.byLane) {
    if (l === lane) continue;
    if ((after.byLane.get(l) ?? []).join("\n") !== lines.join("\n")) errors.push(`${path}: lane ${l}'s section changed`);
  }
  for (const l of after.byLane.keys()) {
    if (!before.byLane.has(l)) errors.push(`${path}: new section # [lane-${l}]`);
  }
}

if (errors.length) {
  console.error(`lane-scope: lane ${lane}, ${errors.length} problem(s) since ${mergeBase.slice(0, 12)}:`);
  for (const e of errors) console.error(`  - ${e}`);
  process.exit(1);
}
console.log(`lane-scope: lane ${lane} clean since ${mergeBase.slice(0, 12)} (${changes.length / 2} path(s))`);

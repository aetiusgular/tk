#!/usr/bin/env node
// Self-test for lane-scope.mjs: one throwaway commit per rule, in a scratch
// repository under the OS temp folder. Run: node scripts/lane-scope.test.mjs
// Exit 0 when every case gets the expected exit code and message.

import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, unlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const script = join(dirname(fileURLToPath(import.meta.url)), "lane-scope.mjs");
const repo = mkdtempSync(join(tmpdir(), "tk-scope-"));
const git = (...a) => execFileSync("git", a, { cwd: repo, encoding: "utf8" });
const write = (p, text) => {
  mkdirSync(dirname(join(repo, p)), { recursive: true });
  writeFileSync(join(repo, p), text);
};

const cargo = `[package]
name = "tk-cli"

[features]
default = []
# [lane-C]
fork-docker-mcp = []
# [lane-D]

[dependencies]
tk-core = { path = "../tk-core" }
# [lane-C]
# [lane-D]

[dev-dependencies]
# [lane-C]
# [lane-D]
`;

git("init", "-q", "-b", "main");
git("config", "user.name", "scope test");
git("config", "user.email", "scope@test.invalid");
for (const [p, t] of Object.entries({
  "crates/tk-cli/Cargo.toml": cargo,
  "crates/tk-cli/src/main.rs": "fn main() {}\n",
  "crates/tk-cli/src/connect/mod.rs": "// c\n",
  "crates/tk-cli/src/run/mod.rs": "// d\n",
  "crates/tk-daemon/src/lib.rs": "// b\n",
  "crates/tk-daemon/src/computer_use/mod.rs": "// k\n",
  "packages/tk-pi/extensions/gate.ts": "// a\n",
  "packages/tk-pi/extensions/memory.ts": "// k\n",
  "library/src/validate.ts": "// e\n",
  "contracts/policy.md": "# p\n",
  "package.json": "{}\n",
  "README.md": "# r\n",
})) write(p, t);
git("add", "-A");
git("commit", "-q", "-m", "base");
const base = git("rev-parse", "HEAD").trim();

const cases = [
  ["own file edited (A)", "A", () => write("packages/tk-pi/extensions/gate.ts", "// a2\n"), 0, "clean"],
  ["new file in own folder (B)", "B", () => write("crates/tk-daemon/src/socket.rs", "// s\n"), 0, "clean"],
  ["new .test.ts in own folder (E)", "E", () => write("library/src/x.test.ts", "//\n"), 0, "clean"],
  ["new .test.tsx refused (A)", "A", () => write("packages/tk-pi/extensions/x.test.tsx", "//\n"), 1, "vitest collects only"],
  ["new .test.mjs refused (F)", "F", () => write("packages/tk-claude-plugin/x.test.mjs", "//\n"), 1, "vitest collects only"],
  ["another lane's file (C edits run/)", "C", () => write("crates/tk-cli/src/run/mod.rs", "// x\n"), 1, "owned by lane D"],
  ["excepted file (A edits memory.ts)", "A", () => write("packages/tk-pi/extensions/memory.ts", "// x\n"), 1, "owned by lane K"],
  ["excepted folder (B edits computer_use)", "B", () => write("crates/tk-daemon/src/computer_use/mod.rs", "// x\n"), 1, "owned by lane K"],
  ["new file outside every lane", "D", () => write("docs/x.md", "x\n"), 1, "owned by no lane"],
  ["no-lane file: contracts", "B", () => write("contracts/policy.md", "# changed\n"), 1, "no lane edits this"],
  ["no-lane file: root package.json", "E", () => write("package.json", "{\"x\":1}\n"), 1, "no lane edits this"],
  ["no-lane file: main.rs", "C", () => write("crates/tk-cli/src/main.rs", "fn main() { }\n"), 1, "no lane edits this"],
  ["deleted file of another lane", "A", () => unlinkSync(join(repo, "library/src/validate.ts")), 1, "owned by lane E"],
  ["shared Cargo.toml, own section (C)", "C", () => write("crates/tk-cli/Cargo.toml", cargo.replace("# [lane-C]\n# [lane-D]\n\n[dev", "# [lane-C]\nregex = \"=1.11.1\"\n# [lane-D]\n\n[dev")), 0, "clean"],
  ["shared Cargo.toml, C drops its feature flag", "C", () => write("crates/tk-cli/Cargo.toml", cargo.replace("fork-docker-mcp = []\n", "")), 0, "clean"],
  ["shared Cargo.toml, D drops C's feature flag", "D", () => write("crates/tk-cli/Cargo.toml", cargo.replace("fork-docker-mcp = []\n", "")), 1, "lane C's section changed"],
  ["shared Cargo.toml, own dev section (D)", "D", () => write("crates/tk-cli/Cargo.toml", cargo.replace(/# \[lane-D\]\n$/, "# [lane-D]\ntempfile = \"=3.20.0\"\n")), 0, "clean"],
  ["shared Cargo.toml, other lane's section (C writes under D)", "C", () => write("crates/tk-cli/Cargo.toml", cargo.replace("# [lane-D]\n\n[dev", "# [lane-D]\nregex = \"=1.11.1\"\n\n[dev")), 1, "lane D's section changed"],
  ["shared Cargo.toml, outside sections", "D", () => write("crates/tk-cli/Cargo.toml", cargo.replace("tk-core =", "serde = \"1\"\ntk-core =")), 1, "changed outside"],
  ["shared Cargo.toml, marker removed", "C", () => write("crates/tk-cli/Cargo.toml", cargo.replace("# [lane-C]\n# [lane-D]\n\n[dev", "# [lane-D]\n\n[dev")), 1, "changed outside"],
  ["shared Cargo.toml, lane without a section (A)", "A", () => write("crates/tk-cli/Cargo.toml", cargo.replace("# [lane-C]\n# [lane-D]\n\n[dev", "# [lane-C]\nx = \"1\"\n# [lane-D]\n\n[dev")), 1, "has no section"],
  ["shared Cargo.toml, new section marker", "C", () => write("crates/tk-cli/Cargo.toml", cargo.replace("# [lane-C]\n# [lane-D]\n\n[dev", "# [lane-C]\n# [lane-E]\n# [lane-D]\n\n[dev")), 1, "changed outside"],
];

let failed = 0;
for (const [name, lane, change, code, text] of cases) {
  git("checkout", "-q", "-B", "case", base);
  change();
  git("add", "-A");
  git("commit", "-q", "-m", name);
  const r = spawnSync("node", [script, lane, "--base", base], { cwd: repo, encoding: "utf8" });
  const out = r.stdout + r.stderr;
  const ok = r.status === code && out.includes(text);
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: exit ${r.status} (want ${code})${ok ? "" : `\n${out}`}`);
}

for (const [name, argv, code] of [
  ["bad lane letter", ["Z", "--base", base], 2],
  ["bad base", ["A", "--base", "no-such-commit"], 2],
  ["stray argument", ["A", "--frob"], 2],
]) {
  const r = spawnSync("node", [script, ...argv], { cwd: repo, encoding: "utf8" });
  const ok = r.status === code;
  if (!ok) failed++;
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: exit ${r.status} (want ${code})`);
}

rmSync(repo, { recursive: true, force: true });
const total = cases.length + 3;
console.log(`${total - failed} of ${total} as expected`);
process.exit(failed ? 1 : 0);

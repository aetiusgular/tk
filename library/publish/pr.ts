// The pull request `tk publish` opens against tk's library (lane G, T3.3).
//
// In the publisher's own clone of the library (a fork, or the repository
// itself for its owner): a branch, the entry copied to library/catalog/<id>/
// with its proof file, one commit in the publisher's own git identity, a
// push of that branch, and `gh pr create`. Each step is an argument list,
// never a shell string (rules section 5, item 3). The default prints the
// steps; `--open-pr` runs them. Nothing here talks to the official registry:
// its repository says publishing never happens by pull request there.
import { randomUUID } from "node:crypto";
import { cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { stableText } from "../mirror/store.ts";
import { PROOF_FILE, type PublishProof } from "./publish.ts";

export const LIBRARY_REPO = "aetiusgular/tk";

export type Step = { argv: string[]; why: string } | { copy: { from: string; to: string }; why: string } | { write: { path: string; text: string; exclusive?: boolean }; why: string };

export function branchFor(proof: PublishProof): string {
  const clean = (s: string) => s.toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "").replace(/\.{2,}/g, ".").slice(0, 60);
  return `publish/${clean(proof.entry)}-${clean(proof.entryVersion || "0")}`;
}

export function prBody(proof: PublishProof, findings: { severity: string; rule: string; message: string }[]): string {
  const lines = [
    `Publishes \`${proof.name}\` ${proof.entryVersion} into \`library/catalog/${proof.entry}/\`.`,
    "",
    `- Content SHA-256: \`${proof.contentSha256}\` over ${Object.keys(proof.files).length + 1} files`,
    `- Namespace proof: ${proof.namespace.method}${proof.namespace.subject ? ` (${proof.namespace.subject})` : ""}`,
    ...proof.packages.map((p) => `- Package ${p.registryType} \`${p.identifier}\`${p.version ? ` ${p.version}` : ""}: provenance ${p.provenance.kind}${p.provenance.repository_uri ? ` from ${p.provenance.repository_uri}` : ""}${p.checked ? "" : " (not checked)"}`),
    "",
    "The library's CI re-checks the hash, the namespace and the packages from its own base branch. These are records, not a verdict that the entry is safe.",
  ];
  const warnings = findings.filter((f) => f.severity === "warning");
  if (warnings.length) lines.push("", "Warnings:", ...warnings.map((w) => `- [${w.rule}] ${w.message}`));
  return lines.join("\n") + "\n";
}

/** The steps, in order. `manifestFile` is the entry's manifest path inside its folder. */
export function prSteps(o: { clone: string; entryDir: string; manifestFile: string; manifest: Record<string, unknown>; proof: PublishProof; body: string; headOwner?: string }): Step[] {
  const clone = resolve(o.clone);
  const dest = join(clone, "library", "catalog", o.proof.entry);
  const branch = branchFor(o.proof);
  // Outside the clone, so it is never committed; a fresh name, opened exclusively.
  const bodyPath = join(tmpdir(), `tk-publish-${randomUUID()}.md`);
  return [
    { argv: ["git", "-C", clone, "checkout", "-b", branch], why: "a branch for this version" },
    { copy: { from: resolve(o.entryDir), to: dest }, why: "the entry into the catalog" },
    { write: { path: join(dest, o.manifestFile), text: stableText(o.manifest) }, why: "the manifest with its trust record" },
    { write: { path: join(dest, PROOF_FILE), text: stableText(o.proof) }, why: "the proof the library's CI re-checks" },
    { argv: ["git", "-C", clone, "add", "--", join("library", "catalog", o.proof.entry)], why: "stage only the entry" },
    { argv: ["git", "-C", clone, "commit", "-m", `library: publish ${o.proof.name} ${o.proof.entryVersion}`], why: "one commit in your own git identity" },
    { argv: ["git", "-C", clone, "push", "-u", "origin", branch], why: "the branch to your remote" },
    { write: { path: bodyPath, text: o.body, exclusive: true }, why: "the pull request text" },
    {
      argv: ["gh", "pr", "create", "--repo", LIBRARY_REPO, "--base", "main", "--head", o.headOwner ? `${o.headOwner}:${branch}` : branch, "--title", `library: publish ${o.proof.name} ${o.proof.entryVersion}`, "--body-file", bodyPath],
      why: "the pull request",
    },
  ];
}

export function describeStep(s: Step): string {
  if ("argv" in s) return s.argv.map((a) => (/^[A-Za-z0-9_./:@=-]+$/.test(a) ? a : JSON.stringify(a))).join(" ");
  if ("copy" in s) return `copy ${s.copy.from} -> ${s.copy.to}`;
  return `write ${s.write.path}`;
}

export type Exec = (argv: string[]) => Promise<void>;

export async function runSteps(steps: Step[], exec: Exec): Promise<void> {
  for (const s of steps) {
    if ("argv" in s) await exec(s.argv);
    else if ("copy" in s) {
      // Publishing from the catalog folder itself: nothing to copy, and the
      // folder must not be removed first.
      if (resolve(s.copy.from) === resolve(s.copy.to)) continue;
      if (resolve(s.copy.to).startsWith(resolve(s.copy.from) + "/") || resolve(s.copy.from).startsWith(resolve(s.copy.to) + "/")) throw new Error("the entry folder and the catalog folder contain one another");
      // The old version's folder is replaced; git holds it.
      if (existsSync(s.copy.to)) rmSync(s.copy.to, { recursive: true });
      mkdirSync(s.copy.to, { recursive: true });
      // verbatimSymlinks with errorOnExist: digest.ts already refused links.
      cpSync(s.copy.from, s.copy.to, { recursive: true, errorOnExist: true, force: false, verbatimSymlinks: true });
    } else writeFileSync(s.write.path, s.write.text, s.write.exclusive ? { flag: "wx", mode: 0o600 } : {});
  }
}

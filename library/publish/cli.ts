// tk publish command line (lane G, T3.3).
//   node library/publish/cli.ts publish <entry-dir> --library <clone> --proof github|dns|http
//        [--key-file <path> | --key-stdin] [--upstream <file>] [--open-pr] [--head-owner <login>]
//   node library/publish/cli.ts record (--key-file <path> | --key-stdin)
//   node library/publish/cli.ts verify --pr <clone> --base <clone> --author <login> <id>...
// `publish` prints what it found and the steps of the pull request; with
// `--open-pr` it runs them. A private key is read from a file or stdin, never
// from argv, which other processes can read. Exit 1 on an error finding, 2 on usage.
import { execFile } from "node:child_process";
import { lstatSync, readFileSync, writeFileSync } from "node:fs";
import { describeStep, prBody, prSteps, runSteps } from "./pr.ts";
import { recordFor } from "./namespace.ts";
import { publish } from "./publish.ts";
import { stableText } from "../mirror/store.ts";
import { verifyEntry } from "./verify.ts";

function usage(): never {
  console.error(
    [
      "usage: node library/publish/cli.ts publish <entry-dir> --library <clone> --proof github|dns|http [--key-file <path> | --key-stdin] [--upstream <file>] [--open-pr] [--head-owner <login>]",
      "       node library/publish/cli.ts record (--key-file <path> | --key-stdin)",
      "       node library/publish/cli.ts verify --pr <clone> --base <clone> --author <login> <id>...",
    ].join("\n"),
  );
  process.exit(2);
}

function run(cmd: string, args: string[], opts: { inherit?: boolean } = {}): Promise<string> {
  return new Promise((resolve, reject) => {
    const p = execFile(cmd, args, { maxBuffer: 16 * 1024 * 1024, encoding: "utf8" }, (err, stdout) => (err ? reject(err) : resolve(stdout)));
    if (opts.inherit) p.stderr?.pipe(process.stderr);
  });
}

function readKey(args: Map<string, string | true>): string | undefined {
  const file = args.get("--key-file");
  if (typeof file === "string") {
    const st = lstatSync(file);
    if (!st.isFile()) throw new Error(`${file} is not a regular file`);
    if ((st.mode & 0o077) !== 0) console.error(`warning: ${file} can be read by other users (mode ${(st.mode & 0o777).toString(8)}); chmod 600 it`);
    return readFileSync(file, "utf8");
  }
  if (args.get("--key-stdin") === true) return readFileSync(0, "utf8");
  return undefined;
}

function parse(argv: string[], flags: string[], values: string[]): { args: Map<string, string | true>; rest: string[] } {
  const args = new Map<string, string | true>();
  const rest: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (flags.includes(a)) args.set(a, true);
    else if (values.includes(a)) {
      const v = argv[++i];
      if (v === undefined) usage();
      args.set(a, v);
    } else if (a.startsWith("-")) usage();
    else rest.push(a);
  }
  return { args, rest };
}

const [cmd, ...argv] = process.argv.slice(2);
if (cmd === "record") {
  const { args } = parse(argv, ["--key-stdin"], ["--key-file"]);
  const pem = readKey(args);
  if (!pem) usage();
  // The public line only; nothing of the private key is printed.
  console.log(recordFor(pem));
} else if (cmd === "publish") {
  const { args, rest } = parse(argv, ["--key-stdin", "--open-pr"], ["--library", "--proof", "--key-file", "--upstream", "--head-owner"]);
  const dir = rest[0];
  const library = args.get("--library");
  const proof = args.get("--proof");
  if (!dir || rest.length !== 1 || typeof library !== "string" || (proof !== "github" && proof !== "dns" && proof !== "http")) usage();
  const pem = readKey(args);
  const result = await publish({ dir, library, proof, ...(pem ? { privateKeyPem: pem } : {}) }, { gh: (a) => run("gh", a) });
  for (const x of result.findings) console.log(`${x.severity === "error" ? "error  " : "warning"}${x.path ? ` ${x.path}` : ""}: [${x.rule}] ${x.message}`);
  if (!result.ok || !result.proof || !result.manifest || !result.id) {
    console.error("not published: fix the errors above");
    process.exit(1);
  }
  console.log(`content sha256 ${result.proof.contentSha256}; namespace ${result.proof.namespace.method}${result.proof.namespace.subject ? ` (${result.proof.namespace.subject})` : ""}`);
  const upstream = args.get("--upstream");
  if (typeof upstream === "string" && result.upstream) {
    writeFileSync(upstream, stableText(result.upstream.server), { flag: "wx" });
    console.log(`wrote ${upstream} for the official registry${result.upstream.dropped.length ? `, without ${result.upstream.dropped.join(", ")} (its 4,096-byte cap)` : ""}; publish it with your own login:`);
    console.log(`  mcp-publisher login <github|dns|http> ...`);
    console.log(`  mcp-publisher publish ${upstream}`);
  }
  const headOwner = args.get("--head-owner");
  const steps = prSteps({ clone: library, entryDir: dir, manifestFile: result.proof.kind === "bundle" ? ".claude-plugin/plugin.json" : result.proof.kind === "skill" ? "skill.json" : "server.json", manifest: result.manifest, proof: result.proof, body: prBody(result.proof, result.findings), ...(typeof headOwner === "string" ? { headOwner } : {}) });
  if (args.get("--open-pr") !== true) {
    console.log("the pull request, not opened (add --open-pr to run these):");
    for (const s of steps) console.log(`  ${describeStep(s)}`);
  } else {
    await runSteps(steps, async (a) => void (await run(a[0]!, a.slice(1), { inherit: true })));
    console.log("pull request opened");
  }
} else if (cmd === "verify") {
  const { args, rest } = parse(argv, [], ["--pr", "--base", "--author"]);
  const pr = args.get("--pr");
  const base = args.get("--base");
  const author = args.get("--author");
  if (typeof pr !== "string" || typeof base !== "string" || typeof author !== "string" || !/^[A-Za-z0-9-]{1,39}(\[bot\])?$/.test(author) || rest.length === 0) usage();
  let errors = 0;
  for (const id of rest) {
    const findings = await verifyEntry({ prLibrary: pr, baseLibrary: base, id, prAuthor: author });
    for (const x of findings) {
      if (x.severity === "error") errors++;
      console.log(`${x.severity === "error" ? "error  " : "warning"} ${id}: [${x.rule}] ${x.message}`);
    }
    if (!findings.length) console.log(`ok      ${id}`);
  }
  process.exit(errors ? 1 : 0);
} else {
  usage();
}

// tk library mirror command line (lane G, T3.2).
//   node library/mirror/cli.ts pull --out <dir> [--full] [--no-probe]
//        [--probe-budget N] [--cap N] [--base-url <https url>] [--max-pages N]
//        [--attempts N]
//   node library/mirror/cli.ts probe <url>
// Runs in CI on a schedule (library/mirror/ci/mirror.yml), never from the
// daemon (plans/parallel_build_tk.md, lane G's card). It is the one part of
// the library that calls out by default: the registry's list API, and with
// probes on, each mirrored remote's own host. `--no-probe` turns the second off.
import { canResolve, probe } from "./liveness.ts";
import { pull } from "./pull.ts";

function usage(): never {
  console.error(
    "usage: node library/mirror/cli.ts pull --out <dir> [--full] [--no-probe] [--probe-budget N] [--cap N] [--base-url <url>] [--max-pages N] [--attempts N]\n       node library/mirror/cli.ts probe <url>",
  );
  process.exit(2);
}

function intArg(v: string | undefined, name: string): number {
  if (v === undefined || !/^\d{1,7}$/.test(v)) {
    console.error(`${name} takes a whole number`);
    process.exit(2);
  }
  return Number(v);
}

const [cmd, ...args] = process.argv.slice(2);
if (cmd === "pull") {
  let out: string | undefined;
  let full = false;
  let doProbe = true;
  let budget: number | undefined;
  let cap: number | undefined;
  let baseUrl: string | undefined;
  let maxPages: number | undefined;
  let attempts: number | undefined;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--out") out = args[++i];
    else if (a === "--full") full = true;
    else if (a === "--no-probe") doProbe = false;
    else if (a === "--probe-budget") budget = intArg(args[++i], a);
    else if (a === "--cap") cap = intArg(args[++i], a);
    else if (a === "--max-pages") maxPages = intArg(args[++i], a);
    else if (a === "--attempts") attempts = intArg(args[++i], a);
    else if (a === "--base-url") baseUrl = args[++i];
    else usage();
  }
  if (!out) usage();
  const report = await pull({
    out,
    full,
    probe: doProbe,
    ...(budget !== undefined ? { probeBudget: budget } : {}),
    ...(cap !== undefined ? { cap } : {}),
    ...(maxPages !== undefined ? { maxPages } : {}),
    client: { ...(baseUrl ? { baseUrl } : {}), ...(attempts !== undefined ? { attempts } : {}) },
    log: (l) => console.error(l),
  }).catch((e: unknown) => {
    console.error(`mirror pull failed: ${(e as Error).message}`);
    console.error("the watermark did not move; files written so far are kept and the next run re-reads them");
    process.exit(1);
  });
  console.log(JSON.stringify(report, null, 2));
} else if (cmd === "probe") {
  const url = args[0];
  if (!url || args.length !== 1) usage();
  const cannot = await canResolve();
  if (cannot) {
    console.error(`not probed: ${cannot}; a result from here would call every remote dead`);
    process.exit(3);
  }
  console.log(JSON.stringify(await probe(url), null, 2));
} else {
  usage();
}

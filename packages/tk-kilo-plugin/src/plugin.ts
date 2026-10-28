// Kilo plugin entry (T2.4). Kilo 7.8.18 reads a default export {id, server}
// as a v1 plugin (plugin/shared.ts readV1Plugin); this file exports nothing
// else. The plugin reads nothing when it loads: the policy file is read on the
// first tool call. No import of @kilocode/plugin: the types are not needed at
// run time, and Kilo would have to install the package.
import { after, before, defaultDeps, type AfterInput, type BeforeInput, type BeforeOutput } from "./kilo.ts";

async function server(): Promise<Record<string, (input: never, output: never) => Promise<void>>> {
  const deps = defaultDeps(process.env);
  return {
    "tool.execute.before": (input: BeforeInput, output: BeforeOutput) => before(input, output, deps),
    "tool.execute.after": (input: AfterInput, output: unknown) => after(input, output, deps),
  } as Record<string, (input: never, output: never) => Promise<void>>;
}

export default { id: "tk", server };

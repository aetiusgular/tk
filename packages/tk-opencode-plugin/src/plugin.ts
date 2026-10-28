// OpenCode plugin entry (T2.3). OpenCode 1.19.4 reads a default export
// {id, server} as a v1 plugin (plugin/shared.ts:271-295 at 148842ef); in the
// legacy form every export must be a plugin function, so this file exports
// nothing else. The plugin reads nothing when it loads: the policy file is read
// on the first tool call.
import { after, before, defaultDeps, type AfterInput, type BeforeInput, type BeforeOutput } from "./opencode.ts";

async function server(): Promise<Record<string, (input: never, output: never) => Promise<void>>> {
  const deps = defaultDeps(process.env);
  return {
    "tool.execute.before": (input: BeforeInput, output: BeforeOutput) => before(input, output, deps),
    "tool.execute.after": (input: AfterInput, output: unknown) => after(input, output, deps),
  } as Record<string, (input: never, output: never) => Promise<void>>;
}

export default { id: "tk", server };

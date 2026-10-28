// The input the gate allowed is the input that runs (integration w1 finding
// BX9, rules case 88).
//
// Pi 1.4.0 runs `tool_call` handlers in extension load order, one after the
// other, on one event whose `input` is the very object the tool executes with
// (core/agent-session.js:326-338 passes `input: args`; agent-loop.js:487-545
// returns those `args` for execution), and a later handler may change it in
// place (docs/extensions.md:197, 208). No handler can run after all others:
// `pi.on()` appends to the extension's own list (extensions/loader.js:216-231)
// and nothing fires between the last handler and the tool (tool_execution_start
// fires before tool_call, agent-loop.js:384-390). So once the gate allows a
// call it freezes the input, deeply. A later handler's assignment then throws
// (extension modules are strict mode), Pi turns the throw into an error result
// (agent-loop.js:539-545), and the call does not run with input the gate
// never saw; a handler that only reads (Orca's, which copies the input,
// stablyai/orca 92a754a src/main/pi/agent-status-input-redaction-source.ts) is
// unaffected. Pi's own argument preparation (edit's prepareArguments) runs
// before the handlers (agent-loop.js:497-498).
//
// omp 18.13.3: a later handler that returns `{ input }` replaces the
// arguments without touching the frozen object (extensibility/shared-events.ts:
// 331-379), so this does not cover that route there; recorded in the README.
//
// `TK_PI_FREEZE_INPUT=0`, read from the harness's own environment, turns it
// off for a tool that changes its own arguments while it runs.

const MAX_DEPTH = 64;

/** Freezes plain objects and arrays under `value`, in place. Returns false when the tree is too deep to finish. */
export function deepFreeze(value: unknown, depth = 0, seen: Set<object> = new Set()): boolean {
  if (typeof value !== "object" || value === null) return true;
  if (seen.has(value)) return true;
  if (depth > MAX_DEPTH) return false;
  seen.add(value);
  let ok = true;
  for (const key of Reflect.ownKeys(value)) {
    const d = Object.getOwnPropertyDescriptor(value, key);
    if (d && "value" in d) ok = deepFreeze(d.value, depth + 1, seen) && ok;
  }
  Object.freeze(value);
  return ok;
}

export function freezeEnabled(env: Record<string, string | undefined>): boolean {
  const v = env.TK_PI_FREEZE_INPUT;
  return !(v === "0" || v === "off" || v === "false");
}

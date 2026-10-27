// Replaces session 0's stubs.test.ts. Its rule, "each extension loads and
// touches nothing it is given", was the stub's; the real rule is corner case
// 46: at load an extension only registers (pi.on, a pi.events listener), and
// reads no file, no environment, and starts no timer, socket or process.
// gate.test.ts and emitter.test.ts check the same with their fakes; this file
// runs the modules' default exports as Pi would import them.

import { describe, expect, it, vi } from "vitest";
import gate from "../extensions/gate.ts";
import emitter from "../extensions/emitter.ts";
import memory from "../extensions/memory.ts";
import type { PiApi } from "../src/pi-api.ts";

function recordingApi() {
  const used: string[] = [];
  const api = new Proxy(
    {},
    {
      get(_t, prop) {
        const name = String(prop);
        used.push(name);
        if (name === "on") return () => () => undefined;
        if (name === "events") return { on: () => () => undefined, emit: () => undefined };
        throw new Error(`touched pi.${name} at load`);
      },
    },
  ) as PiApi;
  return { api, used };
}

describe("default exports at load (corner case 46)", () => {
  it("gate registers through pi.on only and reads no environment", () => {
    const env = vi.spyOn(process, "env", "get");
    const { api, used } = recordingApi();
    gate(api);
    expect(new Set(used)).toEqual(new Set(["on"]));
    expect(env).not.toHaveBeenCalled();
    env.mockRestore();
  });

  it("emitter registers through pi.on and pi.events only, starts no timer and reads no environment", () => {
    vi.useFakeTimers();
    const env = vi.spyOn(process, "env", "get");
    const { api, used } = recordingApi();
    emitter(api);
    expect(new Set(used)).toEqual(new Set(["on", "events"]));
    expect(vi.getTimerCount()).toBe(0);
    expect(env).not.toHaveBeenCalled();
    env.mockRestore();
    vi.useRealTimers();
  });

  it("the memory stub (lane K's file, listed in package.json since integration w1) touches nothing", () => {
    const { api, used } = recordingApi();
    memory(api);
    expect(used).toEqual([]);
  });
});

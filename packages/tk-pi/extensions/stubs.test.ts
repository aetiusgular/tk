import { describe, expect, it } from "vitest";
import gate from "./gate.ts";
import emitter from "./emitter.ts";
import memory from "./memory.ts";

describe("tk-pi stubs (session 0)", () => {
  it("each extension loads and touches nothing it is given", () => {
    const pi = new Proxy({}, { get: () => { throw new Error("stub touched the API"); } });
    expect(() => gate(pi)).not.toThrow();
    expect(() => emitter(pi)).not.toThrow();
    expect(() => memory(pi)).not.toThrow();
  });
});

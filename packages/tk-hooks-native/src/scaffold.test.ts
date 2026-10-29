// Placeholder from integration w2, so the package is type-checked and tested
// from its first commit. Lane N replaces it with the package's own tests.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

describe("tk-hooks-native scaffold", () => {
  it("has its manifest", () => {
    const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8")) as { name: string; private: boolean };
    expect(pkg.name).toBe("tk-hooks-native");
    expect(pkg.private).toBe(true);
  });
});

import { expect, it } from "vitest";
import { validateEntry } from "./validate.ts";

it("the session 0 stub never passes an entry", () => {
  expect(validateEntry({})).toHaveLength(1);
});

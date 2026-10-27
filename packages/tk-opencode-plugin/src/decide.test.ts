import { expect, it } from "vitest";
import { decide } from "./decide.ts";

it("the session 0 stub fails closed", () => {
  expect(decide({}).action).toBe("deny");
});

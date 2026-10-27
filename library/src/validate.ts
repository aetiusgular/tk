// The library validator (lane E, T3.1). STUB. Contract:
// contracts/entry.schema.json and contracts/shapes/. Lane E adds the
// library's rules (unique names, annotations, outputSchema, the reject list
// of contracts/policy.md section 3, lookalikes).
export type Finding = { rule: string; path: string; message: string };

export function validateEntry(_entry: unknown): Finding[] {
  return [{ rule: "not-implemented", path: "", message: "validator not implemented (session 0 stub; lane E)" }];
}

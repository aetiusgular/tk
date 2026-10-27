// Claude Code plugin: PreToolUse and PostToolUse hooks (lane F, T2.1). STUB. Contract: contracts/policy.md. Until lane F replaces it, the
// stub answers "deny" so that nothing installed by mistake allows a call.
export type Decision = { action: "allow" | "ask" | "deny"; reason: string };

export function decide(_input: unknown): Decision {
  return { action: "deny", reason: "tk-claude-plugin not implemented (session 0 stub; lane F)" };
}

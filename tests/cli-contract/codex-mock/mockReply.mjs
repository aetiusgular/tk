// The reply logic of codex-mock, shared by serve.mjs and the contract check.
import { text, call } from "../lib/fakeResponses.mjs";

function lastUserPrompt(body) {
  const users = (body?.input ?? []).filter((i) => i.role === "user");
  const last = users.at(-1);
  const parts = Array.isArray(last?.content) ? last.content : [];
  return parts.map((c) => c?.text ?? "").join("\n").trim();
}

function delimiterFor(content) {
  let d = "MOCK_PATCH_END";
  while (content.includes(d)) d += "_X";
  return d;
}

export function mockReply(body) {
  const lastItem = (body?.input ?? []).at(-1);
  if (lastItem?.type === "function_call_output" || lastItem?.type === "custom_tool_call_output") {
    const out = typeof lastItem.output === "string" ? lastItem.output : JSON.stringify(lastItem.output);
    const lines = out.split("\n").filter((l) => l.trim() && !/^(Chunk ID|Wall time|Original token count|Output:)/.test(l));
    return [text(`Done. ${lines.slice(0, 2).join(" ").slice(0, 300) || "no output"}`)];
  }
  const prompt = lastUserPrompt(body);
  let m = /^edit\s+(.+?):\s*(.*?)\s*=>\s*(.*)$/s.exec(prompt);
  if (m) {
    const [, file, from, to] = m;
    const patch = `*** Begin Patch\n*** Update File: ${file}\n@@\n-${from}\n+${to}\n*** End Patch\n`;
    const d = delimiterFor(patch);
    return [call("exec_command", { cmd: `apply_patch <<'${d}'\n${patch}${d}\n` })];
  }
  m = /^write\s+(.+?):\s*(.*)$/s.exec(prompt);
  if (m) {
    const [, file, content] = m;
    const lines = content.split("\n").map((l) => `+${l}`).join("\n");
    const patch = `*** Begin Patch\n*** Add File: ${file}\n${lines}\n*** End Patch\n`;
    const d = delimiterFor(patch);
    return [call("exec_command", { cmd: `apply_patch <<'${d}'\n${patch}${d}\n` })];
  }
  m = /^run\s+(.+)$/s.exec(prompt);
  if (m) return [call("exec_command", { cmd: m[1].trim(), yield_time_ms: 5000 })];
  return [text(`codex-mock received: ${prompt.slice(0, 300)}`)];
}

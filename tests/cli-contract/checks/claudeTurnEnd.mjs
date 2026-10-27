// Mac test 2.8: how a Claude Code turn ends, as MonoCode reads it.
//
// With CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1 (MonoCode's session env), each
// turn should end `result` then `session_state_changed` `idle`, and a stopped
// turn's late result should name the stopped prompt's uuid, not the next
// one's. The check holds an approval open, sends `interrupt`, sends the next
// prompt at once, and reads what comes back.
//
// Pass: state events seen; the stopped turn's result names the first uuid;
// the next turn's result names the second; the last line is `idle`.
// Info: no state events (an older CLI): MonoCode ends turns on the result,
// and a result naming no uuid is taken as the current turn's.

import { randomUUID } from "node:crypto";
import { startFakeMessages, textBlock, toolUse, isSideRequest } from "../lib/fakeMessages.mjs";
import { ClaudeSession } from "../lib/claudeDriver.mjs";
import { makeTempDirs } from "../lib/codexDriver.mjs";

async function stoppedTurnEnd({ claudePath }) {
  const fake = await startFakeMessages({
    script: (body) => {
      if (isSideRequest(body)) return [textBlock("title")];
      const all = JSON.stringify(body.messages ?? []);
      if (all.includes("second prompt")) return [textBlock("second ok")];
      return [toolUse("Bash", { command: "touch held-file.txt", description: "Make a file" })];
    },
  });
  let asked = null;
  const session = new ClaudeSession({
    claudePath,
    dirs: makeTempDirs("turn-end-"),
    baseUrl: fake.url ?? fake.baseUrl,
    env: { CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS: "1" },
    // Never answer the approval: the turn waits on the user when stopped.
    onControl: (req) => {
      asked = req;
      return null;
    },
  });
  const write = session.write.bind(session);
  session.write = (obj) => {
    if (obj.type === "control_response" && obj.response?.response === null) return;
    write(obj);
  };
  const first = randomUUID();
  const second = randomUUID();
  const user = (uuid, text) => ({ type: "user", uuid, session_id: "", parent_tool_use_id: null, message: { role: "user", content: [{ type: "text", text }] } });
  try {
    session.write(user(first, "first prompt"));
    await session.waitFor(() => asked, 60000, "the approval request");
    session.write({ type: "control_request", request_id: "stop-1", request: { subtype: "interrupt" } });
    session.write(user(second, "second prompt"));
    await session.waitFor(() => session.events.filter((e) => e.type === "result").length >= 2, 60000, "two results");
    await new Promise((r) => setTimeout(r, 1500));
  } finally {
    await session.close();
    await fake.close?.();
  }
  const name = (u) => (u === first ? "first" : u === second ? "second" : u ? "other" : "none");
  const trail = session.events
    .map((e) =>
      e.type === "system" && e.subtype === "session_state_changed"
        ? `state:${e.state}`
        : e.type === "result"
          ? `result:${e.subtype}:${name(e.user_message_uuid)}`
          : e.type === "control_request"
            ? `ask:${e.request?.subtype}`
            : null,
    )
    .filter(Boolean);
  const results = session.events.filter((e) => e.type === "result");
  const states = trail.some((t) => t.startsWith("state:"));
  const seen = { trail };
  if (!states) {
    return { status: "info", finding: `No session_state_changed lines: MonoCode ends turns on the result. Trail: ${trail.join(" ")}`, seen };
  }
  const ok = results[0]?.user_message_uuid === first && results[1]?.user_message_uuid === second && trail.at(-1) === "state:idle";
  return {
    status: ok ? "pass" : "fail",
    finding: `${ok ? "State events seen; the stopped turn's late result names its own prompt, the next turn's names the next" : "Unexpected order or uuids"}. Trail: ${trail.join(" ")}`,
    seen,
  };
}

export const claudeTurnEndChecks = [
  { id: "turn-end", title: "A stopped turn's late result names its own prompt, and idle ends each turn", flag: "turn end (Mac 2.8)", run: stoppedTurnEnd },
];

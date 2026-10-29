"""tk's gate for Hermes Agent (T2.4, wave 2 lane I).

Hermes calls ``pre_tool_call`` before every tool call and ``transform_tool_result``
after it. Both hand the call to ``bin/tk-decide.mjs``, which decides with tk's one
gate core, so the rules here are the same as in every other harness.

Hermes facts (NousResearch/hermes-agent v0.22.5, bf0203a7; plans/parallel/tk_lane_I.md):

- A pre_tool_call return of ``{"action": "block", "message"}`` vetoes the call and the
  message becomes the tool result; ``{"action": "approve", "message"}`` sends it to
  Hermes's human gate; ``{"action": "modify", "args"}`` is merged over the args
  (hermes_cli/plugins.py:1941-2059).
- A callback that raises, or runs past ``plugins.hook_callback_timeout`` (30 s by
  default), blocks the call (plugins_dispatch.py:279-294).
- Under ``--yolo``, ``/yolo``, an unattended turn, or ``approvals.mode: off``, Hermes's
  human gate approves everything at once (tools/approval.py:327-332, 990-991). An
  ``approve`` there would let a tk ask through, so tk blocks instead (rules case 83).

Everything that goes wrong here blocks: no Node, a timeout, output that is not the
answer this module expects.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any, Callable, Optional

HERE = Path(__file__).resolve().parent
DECIDE = HERE / "bin" / "tk-decide.mjs"
#: Below Hermes's default 30 s callback timeout; the Node side caps an ask at 20 s.
DECIDE_TIMEOUT_SECONDS = 25.0

Runner = Callable[[dict[str, Any]], dict[str, Any]]


def _block(reason: str) -> dict[str, str]:
    text = " ".join(str(reason).split()) or "blocked"
    return {"action": "block", "message": f"tk: {text}"}


def node_runner(payload: dict[str, Any], *, node: Optional[str] = None, timeout: float = DECIDE_TIMEOUT_SECONDS) -> dict[str, Any]:
    """Runs bin/tk-decide.mjs once. Raises on anything but one JSON object."""
    exe = node or shutil.which("node")
    if not exe:
        raise RuntimeError("node is not on PATH; the tk gate needs Node 22.18 or later")
    proc = subprocess.run(
        [exe, "--no-warnings", str(DECIDE)],
        input=json.dumps(payload, default=str),
        capture_output=True,
        text=True,
        timeout=timeout,
        env=os.environ.copy(),
        cwd=os.getcwd(),
        check=False,
    )
    out = json.loads(proc.stdout.strip().splitlines()[-1]) if proc.stdout.strip() else None
    if not isinstance(out, dict):
        raise RuntimeError(f"tk-decide gave no answer (exit {proc.returncode})")
    return out


def hermes_auto_approves() -> bool:
    """True when Hermes's own human gate would approve at once, or when that cannot be told."""
    try:
        from tools.approval import _yolo_active  # type: ignore[import-not-found]
        from tools.approval_context import _get_approval_mode  # type: ignore[import-not-found]
    except Exception:
        return True
    try:
        return bool(_yolo_active()) or _get_approval_mode() == "off"
    except Exception:
        return True


class Gate:
    def __init__(self, runner: Runner = node_runner, auto_approves: Callable[[], bool] = hermes_auto_approves) -> None:
        self.runner = runner
        self.auto_approves = auto_approves

    def pre_tool_call(self, tool_name: str = "", args: Optional[dict[str, Any]] = None, session_id: str = "", tool_call_id: str = "", **_: Any) -> Optional[dict[str, Any]]:
        try:
            answer = self.runner({"event": "pre", "tool_name": tool_name, "args": args if isinstance(args, dict) else {}, "session_id": session_id, "tool_call_id": tool_call_id})
        except subprocess.TimeoutExpired:
            return _block("the gate timed out, so the call is refused")
        except Exception as exc:  # the gate could not decide: refuse
            return _block(f"the gate failed, so the call is refused ({exc})")
        decision = answer.get("decision")
        reason = answer.get("reason") if isinstance(answer.get("reason"), str) else ""
        if decision == "deny":
            return _block(reason)
        if decision == "ask":
            if self.auto_approves():
                return _block(f"{reason}; Hermes approves everything in this mode (yolo, unattended or approvals off), so tk refuses its ask")
            return {"action": "approve", "message": f"tk asks: {reason}", "rule_key": f"tk:{tool_name}"}
        if decision == "allow":
            new_args = answer.get("args")
            if isinstance(new_args, dict):
                return {"action": "modify", "args": new_args}
            return None
        return _block(f"the gate gave no decision ({decision!r})")

    def transform_tool_result(self, tool_name: str = "", args: Optional[dict[str, Any]] = None, result: Any = None, session_id: str = "", tool_call_id: str = "", status: str = "", **_: Any) -> Optional[str]:
        try:
            answer = self.runner({"event": "post", "tool_name": tool_name, "args": args if isinstance(args, dict) else {}, "result": result, "session_id": session_id, "tool_call_id": tool_call_id, "status": status})
        except Exception:
            return None  # after the call nothing is gained by failing
        redacted = answer.get("result")
        return redacted if isinstance(redacted, str) and answer.get("redacted") else None

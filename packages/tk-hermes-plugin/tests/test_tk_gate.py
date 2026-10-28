"""Unit tests for tk_gate.py. Run: python3 -m unittest discover -s packages/tk-hermes-plugin/tests

The runner is faked except in the tests that start the real bin/tk-decide.mjs.
"""

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("tk_gate", HERE.parent / "tk_gate.py")
tk_gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tk_gate)


def fixed(answer):
    return lambda payload: answer


class GateMapping(unittest.TestCase):
    def test_allow_is_none(self):
        self.assertIsNone(tk_gate.Gate(fixed({"decision": "allow", "reason": "rule terminal"}), lambda: False).pre_tool_call("terminal", {"command": "ls"}))

    def test_allow_with_rewritten_args_is_modify(self):
        out = tk_gate.Gate(fixed({"decision": "allow", "reason": "r", "args": {"q": "x", "api_key": "[redacted by tk]"}}), lambda: False).pre_tool_call("mcp__exa__search", {"q": "x", "api_key": "k"})
        self.assertEqual(out, {"action": "modify", "args": {"q": "x", "api_key": "[redacted by tk]"}})

    def test_deny_is_block_with_message(self):
        self.assertEqual(tk_gate.Gate(fixed({"decision": "deny", "reason": "rule terminal"}), lambda: False).pre_tool_call("terminal", {}), {"action": "block", "message": "tk: rule terminal"})

    def test_ask_escalates_to_hermes_gate(self):
        out = tk_gate.Gate(fixed({"decision": "ask", "reason": "no rule"}), lambda: False).pre_tool_call("write_file", {})
        self.assertEqual(out["action"], "approve")
        self.assertIn("no rule", out["message"])

    def test_ask_under_yolo_blocks(self):
        out = tk_gate.Gate(fixed({"decision": "ask", "reason": "no rule"}), lambda: True).pre_tool_call("write_file", {})
        self.assertEqual(out["action"], "block")
        self.assertIn("approves everything", out["message"])

    def test_auto_approve_check_outside_hermes_says_true(self):
        # No `tools` package here: the check cannot be made, so it says true and an ask blocks.
        self.assertTrue(tk_gate.hermes_auto_approves())

    def test_every_failure_blocks(self):
        def boom(payload):
            raise RuntimeError("x")

        def slow(payload):
            raise subprocess.TimeoutExpired("node", 25)

        for runner in (boom, slow, fixed({}), fixed({"decision": "maybe"})):
            self.assertEqual(tk_gate.Gate(runner, lambda: False).pre_tool_call("terminal", {"command": "ls"})["action"], "block")

    def test_result_is_replaced_only_when_redacted(self):
        g = tk_gate.Gate(fixed({"result": "TOKEN=[redacted by tk]", "redacted": 1}), lambda: False)
        self.assertEqual(g.transform_tool_result("terminal", {}, "TOKEN=ghp_x"), "TOKEN=[redacted by tk]")
        self.assertIsNone(tk_gate.Gate(fixed({"redacted": 0}), lambda: False).transform_tool_result("terminal", {}, "ok"))

        def boom(payload):
            raise RuntimeError("x")

        self.assertIsNone(tk_gate.Gate(boom, lambda: False).transform_tool_result("terminal", {}, "ok"))

    def test_no_node_blocks(self):
        old = os.environ.get("PATH", "")
        os.environ["PATH"] = "/nonexistent"
        try:
            out = tk_gate.Gate(tk_gate.node_runner, lambda: False).pre_tool_call("terminal", {"command": "ls"})
        finally:
            os.environ["PATH"] = old
        self.assertEqual(out["action"], "block")
        self.assertIn("node is not on PATH", out["message"])


@unittest.skipIf(shutil.which("node") is None, "node not on PATH")
class RealDecide(unittest.TestCase):
    """bin/tk-decide.mjs with a real policy file, no daemon."""

    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.policy = os.path.join(self.dir, "policy.json")
        with open(self.policy, "w") as f:
            json.dump({"version": 1, "askTimeoutSeconds": 1, "servers": ["exa"], "rules": [
                {"tool": "terminal", "action": "allow"},
                {"tool": "exa/*", "action": "deny"},
                {"tool": "write_file", "action": "ask"},
            ]}, f)
        os.chmod(self.policy, 0o600)
        self.env = {"TK_POLICY": self.policy, "TK_DAEMON_SOCKET": os.path.join(self.dir, "absent.sock")}
        self.saved = {k: os.environ.get(k) for k in self.env}
        os.environ.update(self.env)
        self.gate = tk_gate.Gate(tk_gate.node_runner, lambda: False)

    def tearDown(self):
        for k, v in self.saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        shutil.rmtree(self.dir, ignore_errors=True)

    def test_allowed_terminal_command(self):
        self.assertIsNone(self.gate.pre_tool_call("terminal", {"command": "ls -la"}, session_id="s", tool_call_id="c1"))

    def test_mcp_tool_denied_by_server_rule(self):
        out = self.gate.pre_tool_call("mcp__exa__web_search", {"query": "x"})
        self.assertEqual(out, {"action": "block", "message": "tk: rule exa/*"})

    def test_reject_list(self):
        out = self.gate.pre_tool_call("terminal", {"command": "git -c core.sshCommand=x fetch"})
        self.assertEqual(out["action"], "block")
        self.assertIn("reject list: core.sshcommand", out["message"])

    def test_ask_with_no_approver_escalates(self):
        out = self.gate.pre_tool_call("write_file", {"path": "a", "content": "b"})
        self.assertEqual(out["action"], "approve")

    def test_hermes_control_file_asks(self):
        out = self.gate.pre_tool_call("terminal", {"command": "echo plugins.disabled tk >> /home/u/.hermes/config.yaml"})
        self.assertEqual(out["action"], "approve")
        self.assertIn("control file (.hermes/)", out["message"])

    def test_result_redaction(self):
        out = self.gate.transform_tool_result("terminal", {"command": "env"}, "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789AB\nok")
        self.assertIsNotNone(out)
        self.assertNotIn("ghp_abcdefghijklmnopqrstuvwxyz0123456789AB", out)


class Register(unittest.TestCase):
    def test_register_wires_the_two_hooks(self):
        pkg_spec = importlib.util.spec_from_file_location("hermes_plugins.tk", HERE.parent / "__init__.py", submodule_search_locations=[str(HERE.parent)])
        mod = importlib.util.module_from_spec(pkg_spec)
        sys.modules["hermes_plugins.tk"] = mod
        pkg_spec.loader.exec_module(mod)
        seen = []

        class Ctx:
            def register_hook(self, name, cb):
                seen.append((name, cb))

        mod.register(Ctx())
        self.assertEqual([n for n, _ in seen], ["pre_tool_call", "transform_tool_result"])


if __name__ == "__main__":
    unittest.main()

"""tk for Hermes Agent: register(ctx) wires tk's gate (tk_gate.py) into two hooks.

Nothing is read or started at load: the policy file is read by the first call.
"""

from .tk_gate import Gate


def register(ctx):
    gate = Gate()
    ctx.register_hook("pre_tool_call", gate.pre_tool_call)
    ctx.register_hook("transform_tool_result", gate.transform_tool_result)

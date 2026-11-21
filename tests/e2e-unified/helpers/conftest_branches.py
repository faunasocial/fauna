"""Which app branches of a conftest function call a given helper — read off the
source, never run.

`_build_app_config` wires each app's launch environment by hand, one
`app_name == "<app>"` branch at a time, so an env helper every app needs is
called once per branch, and a branch that forgets it fails silently, with
nothing at launch to notice. A tier_1 test pins the wiring structurally with
:func:`branch_calls`: it parses the function's source (calling it would build
real apps and resolve real fixtures, which is a build, not a unit test) and
answers which app names' branches contain the call.

Lifted 2026-11-16 from `test_r14_trust_seed_default.py`, where it pinned the
escrow-trust seed. Its second user is `test_place_accepts_log_env.py`, which
pins the engine's held-pass log target.
"""

import ast
import inspect


def branch_calls(func, callee: str) -> set[str]:
    """App names whose `app_name ==` branch in `func` contains a `callee` call.

    Reads the source rather than running it: `_build_app_config` builds real
    apps and resolves real fixtures, so calling it would be a build, not a
    unit test. The branch structure IS what's being pinned.
    """
    tree = ast.parse(inspect.getsource(func))
    calls: set[str] = set()

    def is_app_name(node) -> bool:
        """`app_name` itself, or `KIDS_PARENT.get(app_name, …)` — a kids column's
        parent, which takes its parent's branch (the ios branch reads it so)."""
        if isinstance(node, ast.Name):
            return node.id == "app_name"
        return (
            isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "get"
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "KIDS_PARENT"
            and bool(node.args)
            and isinstance(node.args[0], ast.Name)
            and node.args[0].id == "app_name"
        )

    def app_names(test) -> set[str]:
        """The string literals an `app_name == "..."` test in `test` names."""
        out = set()
        for node in ast.walk(test):
            if isinstance(node, ast.Compare) and is_app_name(node.left):
                for c in node.comparators:
                    if isinstance(c, ast.Constant) and isinstance(c.value, str):
                        out.add(c.value)
        return out

    def calls_callee(node) -> bool:
        return any(
            isinstance(n, ast.Call)
            and isinstance(n.func, ast.Name)
            and n.func.id == callee
            for n in ast.walk(node)
        )

    def walk(stmts, names: set[str]) -> None:
        # `elif` is an `If` inside `orelse`, so branches must be dispatched on
        # the STATEMENT, never on a container's children — walking children
        # loses each elif's own `app_name ==` test (and with it every branch
        # past the first, which is how this walker's first draft reported
        # linux/macos/tui as unseeded when they were wired all along).
        for stmt in stmts:
            if isinstance(stmt, ast.If):
                walk(stmt.body, app_names(stmt.test) or names)
                walk(stmt.orelse, names)
                continue
            if names and calls_callee(stmt):
                calls.update(names)
            walk(
                [c for c in ast.iter_child_nodes(stmt) if isinstance(c, ast.stmt)],
                names,
            )

    walk(tree.body, set())
    return calls

#!/usr/bin/env python3
"""check_deadline_loop_ratchet.py — the cheap-merge-tier enforcement half of
`docs/goal/architecture/e2e-conventions.md` convention 6's deadline-loop
rider:

  No deadline loop in tests/e2e-unified/actions/ may exit without signalling:
  either it raises (self-diagnosing, e2e rule 6), or it returns a value the
  caller must consume. A loop that falls out of `while <deadline>` and lets
  the function return None silently is the defect — the caller proceeds as if
  the awaited state had arrived, and the real failure surfaces far from its
  cause, misdiagnosed as an unrelated bug. Case studies:
  `ConversationsActions._wait_thread_open` (a chrome-only timeout read as a
  broken chunk-rejoin for two days),
  `_wait_add_participant_dialog_closed`, and
  `BridgesActions._wait_for_linux_action_label` — all fixed here.

AST-based, not regex: the loop and its exit statement are frequently several
lines apart and wrapped in try/except, so a line scan misses the shape (same
reasoning as `check_app_gate_ratchet.py`). Detects a `while` loop whose test
compares a monotonic-clock call (`time.time()`/`time.monotonic()`, bare or
inside a `bool` combination) against a deadline expression, then checks
whether the function raises, or returns a non-None value, anywhere at or
after the loop's last line. A same-line `# deadline-ok: <reason>` comment on
the `while` line exempts a documented-legal silent settle —
`_ensure_on_conversations_page` (an empty conversation list is a real,
caller-independent state) is the precedent, same annotation shape as
`check_sleep_ratchet.py`'s `# sleep-ok:`.

This is a heuristic line/AST-adjacency scan, not a full control-flow
analysis: a `raise` anywhere after the loop in the same function counts as
"signals", even if it sits inside a conditional the interpreter might not
reach on every path. That asymmetry only ever hides a REMAINING silent exit
behind a wrongly-satisfied check — it never manufactures a false positive on
a genuinely-fixed helper — so it stays inside the same down-only-ratchet
safety margin `check_sleep_ratchet.py` already accepts.

Enforcement is a DOWN-ONLY RATCHET against a committed per-file baseline,
exactly like `check_sleep_ratchet.py`: this targets a large existing
population (187 deadline loops / 26 files measured 2026-09-11; most of them
are not `_wait`-named), not a hand-maintained list — convention 7's own
precedent for this shape of debt. New files start at baseline 0; existing
violations are grandfathered and shrink one session at a time.

Usage:
  check_deadline_loop_ratchet.py                    # run the gate
  check_deadline_loop_ratchet.py --update-baseline   # regenerate the baseline
                                                      # (refuses to write a rise)
"""

from __future__ import annotations

import ast
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
E2E_ROOT = REPO / "tests" / "e2e-unified"
ACTIONS_DIR = E2E_ROOT / "actions"
BASELINE_PATH = E2E_ROOT / "baselines" / "deadline_loop_ratchet_baseline.json"

DEADLINE_OK = re.compile(r"#\s*deadline-ok:\s*\S")

_SCOPE_BOUNDARY = (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda, ast.ClassDef)


def _is_clock_call(node: ast.AST) -> bool:
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr in ("time", "monotonic")
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "time"
    )


def _is_deadline_compare(test: ast.AST) -> bool:
    if not isinstance(test, ast.Compare) or len(test.ops) != 1:
        return False
    if not isinstance(test.ops[0], (ast.Lt, ast.LtE, ast.Gt, ast.GtE)):
        return False
    return _is_clock_call(test.left) or _is_clock_call(test.comparators[0])


def _is_deadline_while(node: ast.While) -> bool:
    """True if `node.test` is (or contains, via `and`/`or`) a comparison
    between a time.time()/time.monotonic() call and a deadline expression."""
    test = node.test
    if _is_deadline_compare(test):
        return True
    if isinstance(test, ast.BoolOp):
        return any(_is_deadline_compare(v) for v in test.values)
    return False


def _walk_own_scope(node: ast.AST):
    """Yield every descendant of `node` EXCEPT those inside a nested
    function/lambda/class body — keeps each scope's control flow separate so
    a nested closure's loop is never misattributed to its enclosing function."""
    for child in ast.iter_child_nodes(node):
        yield child
        if isinstance(child, _SCOPE_BOUNDARY):
            continue
        yield from _walk_own_scope(child)


def _returns_value(node: ast.Return) -> bool:
    """A `return <expr>` where <expr> isn't a literal None — a value the
    caller must consume, not a bare success signal."""
    if node.value is None:
        return False
    return not (isinstance(node.value, ast.Constant) and node.value.value is None)


# Driver primitives that are THEMSELVES documented to raise on failure
# (`drivers/base.py::wait_for` raises TimeoutError; `::assert_on_screen` raises
# AssertionError) — a bare call to one of these after a loop is exactly as
# much a signal as a literal `raise`/`assert` written inline.
_RAISING_CALL_NAMES = {"wait_for", "assert_on_screen"}


def _is_raising_call(node: ast.AST) -> bool:
    return (
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr in _RAISING_CALL_NAMES
    )


def _signals_after(func: ast.AST, loop: ast.While) -> bool:
    """True if a Raise, an Assert, a Return carrying a non-None value, or a
    call to a known-raising driver primitive (`wait_for`/`assert_on_screen`)
    appears at or after `loop`'s last line within `func`'s own scope."""
    for n in _walk_own_scope(func):
        if getattr(n, "lineno", 0) <= loop.end_lineno:
            continue
        if isinstance(n, (ast.Raise, ast.Assert)):
            return True
        if isinstance(n, ast.Return) and _returns_value(n):
            return True
        if _is_raising_call(n):
            return True
    return False


def _loop_yields_value_on_success(loop: ast.While) -> bool:
    """True if the loop's OWN body already returns a non-None value on its
    success path — the function is an Optional-shaped helper (like
    `wait_for_gated_badge_text`) where an implicit-None timeout fallthrough is
    a legitimate, distinguishable part of the contract, not silence."""
    for n in _walk_own_scope(loop):
        if isinstance(n, ast.Return) and _returns_value(n):
            return True
    return False


def _iter_functions(tree: ast.AST):
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield node


def violations_in_file(path: Path) -> list[tuple[int, str]]:
    """(line, function-name) for every deadline loop in `path` that neither
    raises nor returns a value after it, and isn't `# deadline-ok:`-annotated."""
    try:
        source = path.read_text()
        tree = ast.parse(source, filename=str(path))
    except (OSError, UnicodeDecodeError, SyntaxError):
        return []
    lines = source.splitlines()
    out: list[tuple[int, str]] = []
    for func in _iter_functions(tree):
        for loop in _walk_own_scope(func):
            if not (isinstance(loop, ast.While) and _is_deadline_while(loop)):
                continue
            line = lines[loop.lineno - 1] if 0 < loop.lineno <= len(lines) else ""
            if DEADLINE_OK.search(line):
                continue
            if _signals_after(func, loop) or _loop_yields_value_on_success(loop):
                continue
            out.append((loop.lineno, func.name))
    return sorted(out)


def _scan_files() -> list[Path]:
    if not ACTIONS_DIR.is_dir():
        return []
    return sorted(ACTIONS_DIR.rglob("*.py"))


def current_counts() -> dict[str, list[tuple[int, str]]]:
    counts = {}
    for path in _scan_files():
        v = violations_in_file(path)
        if v:
            counts[path.relative_to(E2E_ROOT).as_posix()] = v
    return counts


def load_baseline() -> dict[str, int]:
    if not BASELINE_PATH.exists():
        return {}
    return json.loads(BASELINE_PATH.read_text())


def run_gate() -> int:
    baseline = load_baseline()
    counts = current_counts()

    risen = []
    improved = []
    for file, violations in counts.items():
        n = len(violations)
        base_n = baseline.get(file, 0)
        if n > base_n:
            risen.append((file, base_n, n, violations))
        elif n < base_n:
            improved.append((file, base_n, n))
    for file, base_n in baseline.items():
        if file not in counts and base_n > 0:
            improved.append((file, base_n, 0))

    if risen:
        print(
            "check-deadline-loop-ratchet: FAIL — a silent deadline-loop exit "
            "rose past the committed baseline (e2e-conventions.md convention 6 "
            "rider):",
            file=sys.stderr,
        )
        for file, base_n, n, violations in sorted(risen):
            print(f"\n  {file}: {base_n} -> {n}", file=sys.stderr)
            for line_no, funcname in violations:
                print(f"    {file}:{line_no}: {funcname}", file=sys.stderr)
        print(
            "\nFix: make the loop raise on timeout (self-diagnosing — carry "
            "`self.driver.diagnose(...)`) or return a value the caller must "
            "consume — see `helpers/waiting.py::wait_until` and "
            "`ConversationsActions._wait_resolve`. A documented-legal silent "
            "settle (like `_ensure_on_conversations_page`) gets a same-line "
            "`# deadline-ok: <reason>` comment on the `while` line instead of "
            "a baseline bump. Never widen the baseline to launder a new "
            "silent exit in.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-deadline-loop-ratchet: OK — no file exceeds its baseline "
        f"({BASELINE_PATH})."
    )
    if improved:
        print(
            f"  {len(improved)} file(s) improved (fewer silent deadline-loop "
            "exits than baseline) — run `just deadline-loop-ratchet-update` to "
            "shrink the ratchet."
        )
    return 0


def update_baseline() -> int:
    # Bootstrapping a brand-new baseline always "improves" on the implicit
    # empty one, so there is nothing to refuse — write current counts as the
    # starting floor. Only an EXISTING baseline can be laundered by a rise.
    if BASELINE_PATH.exists() and run_gate() != 0:
        print(
            "check-deadline-loop-ratchet: refusing --update-baseline — the "
            "gate is currently failing against the existing baseline. A "
            "down-only ratchet never writes a rise; fix the violations first.",
            file=sys.stderr,
        )
        return 1
    counts = current_counts()
    new_baseline = {file: len(v) for file, v in counts.items()}
    BASELINE_PATH.parent.mkdir(parents=True, exist_ok=True)
    BASELINE_PATH.write_text(json.dumps(new_baseline, indent=2, sort_keys=True) + "\n")
    print(
        f"check-deadline-loop-ratchet: wrote {len(new_baseline)} file(s) to "
        f"{BASELINE_PATH} — commit it alongside your fix."
    )
    return 0


def main(argv: list[str]) -> int:
    if "--update-baseline" in argv[1:]:
        return update_baseline()
    return run_gate()


if __name__ == "__main__":
    sys.exit(main(sys.argv))

#!/usr/bin/env python3
"""check_negative_visibility_ratchet.py — the cheap-merge-tier enforcement half of
`docs/goal/architecture/e2e-conventions.md` convention 6's rider "a NEGATIVE
visibility read is vacuous on a scrollable surface":

  `driver.is_visible()` forwards to each app's own `/element/visible`, and
  windows' bridge answers `!IsOffscreen` — a VIEWPORT predicate over the same
  tree lookup `count()` does. So `assert not is_visible(x)` conflates *"x is
  absent"* with *"x is one scroll away"*, and a defect that really did paint x
  below the fold passes the assertion it was written to fail. Measured
  2026-09-21: the forged-c2pa negative read went green against a mutant that
  painted the badge, while `count` read 2 the whole time.

The gated class is EVERY `assert not <recv>.is_visible(...)`. It started as the
loud subset — an index >= 1 in the element id or scope (`post-card[2]`,
`device-card[{n}]`), which can sit past the fold by construction — and was
widened once the windows audit showed the un-indexed reads are the same trap on
a long scrolling surface (a provisioning page's Cancel button, a settings page's
warning line), honest only by an unpinned assumption about how much fits the
window. The honest spelling is one driver primitive:

    assert driver.is_absent("id", scope="…")

`PlatformDriver.is_absent` is `not is_visible` by default (unchanged on every app
whose bridge has no viewport predicate) and `count == 0` on windows — so the
per-app difference lives in the driver shell and a test states only the intent.
Where the element stays in the tree when logically absent, `not
is_visible_scrolled(...)` is the windows-side alternative (it is NOT cross-app:
web's scroll waits out its bridge timeout on an absent element).

Enforcement is a DOWN-ONLY RATCHET, the same shape as `check_sleep_ratchet.py`
and `check_app_gate_ratchet.py`: a per-file count of offending reads must never
exceed the committed baseline. A file missing from the baseline has a baseline
of 0, so a brand-new file starts clean and only pre-existing debt is
grandfathered. A file whose count DROPPED is reported but never blocks; shrink
the baseline explicitly with --update-baseline.

Two exemptions, and no others:

  * `# negative-visibility-ok: <reason>` on the `assert` line or the line above
    it — an element that genuinely cannot be below a fold (a fixed page with no
    scroll container). The reason is required: an unexplained exemption is the
    debt this ratchet exists to stop, wearing a better name.
  * the exact id `error-message` — convention 2's own rider governs that
    element's absence-when-silent (windows collapses it via
    `MessageShim.ShouldShow`), so a different rule owns it.

The scan is over the AST, not the text: a docstring or comment that QUOTES the
trap is documentation, and a gate that counted it would punish the explanations
that keep the trap from coming back.

WIDENED: `assert not app.settings.identity_qr_shown()`
is invisible to the literal-`is_visible` scan above — the trap moves into the
action-helper layer, not away from it. So the scan also indexes every
`tests/e2e-unified/actions/**.py` function whose body (past its docstring) is
a single `return <recv>.is_visible(...)` — a HELPER-MEDIATED bare visibility
read — and gates every `assert not <call to one of those names>(...)` the same
way. The fix for a caught helper is the one used throughout this pass: rewrite
the HELPER's own body to `return not <recv>.is_absent(...)` (zero call-site
changes — `assert not helper()` keeps reading correctly, since the boolean
`helper()` returns is unchanged everywhere except windows). A helper used only
for POSITIVE reads (`assert helper()`) is not gated — only the negative use is
the vacuous one.

Usage:
  check_negative_visibility_ratchet.py                   # run the gate
  check_negative_visibility_ratchet.py --update-baseline # regenerate (refuses
                                                         # while the gate fails)
"""

from __future__ import annotations

import ast
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
E2E_ROOT = REPO / "tests" / "e2e-unified"
SCAN_DIRS = ["tests", "actions"]
BASELINE_PATH = E2E_ROOT / "baselines" / "negative_visibility_ratchet_baseline.json"

OK_COMMENT = re.compile(r"#\s*negative-visibility-ok:\s*\S")

# Ids another rule owns. Matched on the FIRST string literal — the id argument —
# and on the exact value, never a prefix.
EXEMPT_IDS = frozenset({"error-message"})


def _is_visible_call(node: ast.AST) -> bool:
    """`<anything>.is_visible(...)` or a bare `is_visible(...)`. The receiver is
    free-form because seats are spelled every way (`driver`, `app.driver`, `d`,
    `owner.app.driver`)."""
    if not isinstance(node, ast.Call):
        return False
    fn = node.func
    return (isinstance(fn, ast.Attribute) and fn.attr == "is_visible") or (
        isinstance(fn, ast.Name) and fn.id == "is_visible"
    )


def _first_literal(call: ast.Call) -> str | None:
    """The id argument when it is a string literal (`is_visible("x")`)."""
    if call.args and isinstance(call.args[0], ast.Constant) and isinstance(call.args[0].value, str):
        return call.args[0].value
    return None


def _bare_visible_helper_names(actions_root: Path) -> frozenset[str]:
    """Names of action-layer functions whose body (past an optional docstring)
    is a single `return <recv>.is_visible(...)` — the shape
    `identity_qr_shown` had before it was fixed. Used to widen the gate onto
    `assert not <name>(...)` call sites the literal scan can't see."""
    names: set[str] = set()
    if not actions_root.is_dir():
        return frozenset(names)
    for p in sorted(actions_root.rglob("*.py")):
        src = p.read_text(encoding="utf-8", errors="replace")
        for node in ast.walk(ast.parse(src, filename=str(p))):
            if not isinstance(node, ast.FunctionDef):
                continue
            body = node.body
            if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
                rest = body[1:]
            else:
                rest = body
            if len(rest) == 1 and isinstance(rest[0], ast.Return) and rest[0].value is not None and _is_visible_call(rest[0].value):
                names.add(node.name)
    return frozenset(names)


def _is_call_to_name(node: ast.AST, names: frozenset[str]) -> bool:
    """`<anything>.<name>(...)` or a bare `<name>(...)` for `name` in `names`."""
    if not isinstance(node, ast.Call):
        return False
    fn = node.func
    if isinstance(fn, ast.Attribute):
        return fn.attr in names
    if isinstance(fn, ast.Name):
        return fn.id in names
    return False


def offenders(path: Path, helper_names: frozenset[str] = frozenset()) -> list[tuple[int, str]]:
    src = path.read_text(encoding="utf-8", errors="replace")
    lines = src.split("\n")
    out: list[tuple[int, str]] = []
    for node in ast.walk(ast.parse(src, filename=str(path))):
        if not isinstance(node, ast.Assert):
            continue
        test = node.test
        if not (isinstance(test, ast.UnaryOp) and isinstance(test.op, ast.Not)):
            continue
        operand = test.operand
        if _is_visible_call(operand):
            if _first_literal(operand) in EXEMPT_IDS:
                continue
        elif not _is_call_to_name(operand, helper_names):
            continue
        # `# negative-visibility-ok:` on the assert line or the one above it.
        here = lines[node.lineno - 1]
        above = lines[node.lineno - 2] if node.lineno >= 2 else ""
        if OK_COMMENT.search(here) or OK_COMMENT.search(above):
            continue
        out.append((node.lineno, here.strip()[:120]))
    return sorted(out)


def scan(repo: Path = REPO) -> dict[str, list[tuple[int, str]]]:
    e2e = repo / "tests" / "e2e-unified"
    helper_names = _bare_visible_helper_names(e2e / "actions")
    found: dict[str, list[tuple[int, str]]] = {}
    for d in SCAN_DIRS:
        root = e2e / d
        if not root.is_dir():
            continue
        for p in sorted(root.rglob("*.py")):
            hits = offenders(p, helper_names)
            if hits:
                found[p.relative_to(repo).as_posix()] = hits
    return found


def load_baseline(baseline_path: Path = BASELINE_PATH) -> dict[str, int]:
    if not baseline_path.exists():
        return {}
    return json.loads(baseline_path.read_text(encoding="utf-8")).get("files", {})


def _risen(counts: dict[str, int], baseline: dict[str, int]) -> dict[str, tuple[int, int]]:
    return {f: (n, baseline.get(f, 0)) for f, n in counts.items() if n > baseline.get(f, 0)}


def run_gate(repo: Path = REPO, baseline_path: Path = BASELINE_PATH) -> int:
    found = scan(repo)
    counts = {f: len(h) for f, h in found.items()}
    baseline = load_baseline(baseline_path)

    risen = _risen(counts, baseline)
    dropped = {f: (counts.get(f, 0), b) for f, b in baseline.items() if counts.get(f, 0) < b}

    for f, (now, was) in sorted(dropped.items()):
        print(f"[negative-visibility-ratchet] {f}: {was} -> {now} (shrink the "
              f"baseline with --update-baseline)")

    if risen:
        print("\nFAIL: negative visibility reads rose above the baseline.\n"
              "  A bare `assert not is_visible(x)` is vacuous wherever x can sit below\n"
              "  the fold: windows' bridge answers `!IsOffscreen`, so an element the\n"
              "  defect really did paint reads False and the assertion passes for the\n"
              "  boring reason. Use `driver.is_absent(id, scope=...)` (exact on every\n"
              "  app; on windows it is `count == 0`) — or, in a windows-only test whose\n"
              "  element stays in the tree when logically absent, `not is_visible_scrolled(...)`\n"
              "  (never cross-app: web's scroll waits out its timeout on an absent element)\n"
              "  — or annotate the line with\n"
              "  `# negative-visibility-ok: <why it cannot be below a fold>`.\n"
              "  The same trap applies through an actions/ helper whose body is a bare\n"
              "  `return X.is_visible(...)`: fix the HELPER (`return not X.is_absent(...)`),\n"
              "  never the call site — `assert not helper()` keeps reading correctly.\n"
              "  Rule: docs/goal/architecture/e2e-conventions.md convention 6's rider.\n",
              file=sys.stderr)
        for f, (now, was) in sorted(risen.items()):
            print(f"  {f}: {now} > baseline {was}", file=sys.stderr)
            for lineno, text in found[f]:
                print(f"      {f}:{lineno}  {text}", file=sys.stderr)
        return 1

    total = sum(counts.values())
    print(f"[negative-visibility-ratchet] OK — {total} grandfathered negative "
          f"read(s) across {len(counts)} file(s), none above baseline")
    return 0


def update_baseline(repo: Path = REPO, baseline_path: Path = BASELINE_PATH) -> int:
    found = scan(repo)
    counts = {f: len(h) for f, h in found.items()}
    # Bootstrapping a brand-new baseline always "improves" on the implicit empty
    # one, so there is nothing to refuse — same rule as check_sleep_ratchet.py:
    # only an EXISTING baseline can be laundered by a rise. (Widening the gated
    # class is exactly such a one-time re-bootstrap: delete the file, regenerate,
    # and the commit shows what was grandfathered.)
    risen = _risen(counts, load_baseline(baseline_path)) if baseline_path.exists() else {}
    if risen:
        print("REFUSING --update-baseline: the gate currently FAILS; fix the "
              "reads below (or annotate them) rather than laundering them in.",
              file=sys.stderr)
        for f, (now, was) in sorted(risen.items()):
            print(f"  {f}: {now} > {was}", file=sys.stderr)
        return 1
    baseline_path.parent.mkdir(parents=True, exist_ok=True)
    baseline_path.write_text(
        json.dumps({
            "_comment": "Down-only ratchet for e2e-conventions.md convention 6's "
                        "negative-visibility rider. Regenerate with "
                        "scripts/check_negative_visibility_ratchet.py --update-baseline.",
            "files": dict(sorted(counts.items())),
        }, indent=2) + "\n",
        encoding="utf-8",
    )
    try:
        shown = baseline_path.relative_to(repo).as_posix()
    except ValueError:
        shown = str(baseline_path)
    print(f"baseline written: {shown} "
          f"({sum(counts.values())} reads across {len(counts)} files)")
    return 0


def main() -> int:
    if "--update-baseline" in sys.argv[1:]:
        return update_baseline()
    return run_gate()


if __name__ == "__main__":
    sys.exit(main())

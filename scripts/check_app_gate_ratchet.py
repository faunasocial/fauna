#!/usr/bin/env python3
"""check_app_gate_ratchet.py — the cheap-merge-tier enforcement half of
testing.md § Cross-app e2e conventions, convention 7's *app-gate* bullet:

  An e2e test may not quietly not-run. Convention 7's only sanctioned skip is
  **structural impossibility**; a skip meaning *"this app hasn't built the
  surface yet"* is temporary debt, and it must be VISIBLE — declared through
  `helpers/app_surface.py` so `--strict-app` can turn it into a failure and a
  run can print how many it hit.

This script is the static half: it counts *undeclared* app-gated skips — a bare
`pytest.skip(...)` reached under an app-identity guard (`driver.is_tui()` and
friends) rather than through `skip_unbuilt()` / `declared_absence()`. Enforcement
is a DOWN-ONLY RATCHET against a committed per-file baseline, exactly like
`check_sleep_ratchet.py`: the debt the convention was ratified against cannot
grow, and shrinks as sessions migrate files one at a time.

WHY A SCAN RATHER THAN A LIST. The 2026-07-29 measuring pass
recorded "there are exactly three of these gates today, all in actions/admin.py"
— and was wrong at the moment it was written: `actions/backups.py` and three
sites in `actions/onboarding.py` were already there. The enumeration was keyed
on the three gate *names* someone already knew, so it could only rediscover
them. A parse-only scan keyed on the *class* is the only form of this
enumeration that stays true, which is why the count lives here and not in a
markdown table.

The scan is AST-based, not regex-based, because the guard and the skip are
routinely several lines apart, the guard is often negated over a disjunction
(`if not (driver.is_web() or driver.is_linux())`), and the skip is frequently a
multi-line call. A line-oriented scan misses all three shapes.

MODE 5 — A FIFTH HIDING SHAPE, and the reason the taxonomy is not four modes:
a fixture that skips on a **driver-CAPABILITY predicate**
(`driver.supports_unclean_kill()`) rather than an app-IDENTITY predicate
(`driver.is_tui()`). The guard names no app and no `is_X`
call, so the scan below could not see it by construction — it hid all 11
`test_crash_recovery_journeys.py` journeys on tui for as long as the driver
existed, reading as a *structural* impossibility ("driver must own its app child
as a Popen") when it was ordinary unbuilt debt (fixed). Two
capability-predicate-aware pieces close this mode, both driven off the SAME
source of truth — `capability_predicate_names()`, which finds every
`PlatformDriver` method whose body is a trivial `return <constant>` (a
capability a driver silently answers False/None for until it overrides it),
excluding the `is_<app>` identity family (which is correct-by-design, not debt):

  1. `undeclared_app_gates()` now also treats a bare `pytest.skip()` reached
     under a guard on one of those capability predicates as an app gate — the
     exact shape that hid the crash-recovery journeys.
  2. `run_capability_debt()` (`--capability-debt`) is a separate, INFORMATIONAL
     report (not a gate): for each capability predicate, which class in each of
     the 7 leaf app drivers' MRO actually answers it. A resolver other than the
     leaf's own class means that driver inherits the answer from somewhere else
     in its chain — not necessarily a bug (linux correctly inherits
     `supports_unclean_kill` from `HttpBridgeDriver`), but exactly the kind of
     silent inheritance that hid the tui bug, so it is surfaced as a coverage-
     debt inventory for a human to glance over rather than left invisible.
     Full context: `docs/goal/architecture/testing.md` § convention 7, mode 5.

Usage:
  check_app_gate_ratchet.py                    # run the gate
  check_app_gate_ratchet.py --list             # print every site found, grouped
  check_app_gate_ratchet.py --update-baseline  # regenerate from the current
                                               # tree; refuses to write a rise
  check_app_gate_ratchet.py --capability-debt  # print the mode-5 driver-
                                               # capability override inventory
                                               # (informational, not a gate)
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
E2E_ROOT = REPO / "tests" / "e2e-unified"
SCAN_DIRS = ["tests", "actions", "drivers", "helpers"]
BASELINE_PATH = E2E_ROOT / "baselines" / "app_gate_ratchet_baseline.json"
DRIVERS_ROOT = E2E_ROOT / "drivers"
BASE_DRIVER_FILE = DRIVERS_ROOT / "base.py"

# The gate's OWN inputs, DERIVED from the constants above rather than restated.
# A change to any of them can move the verdict on a file the merge never
# touched, so the changed-files mode (`--paths-from`, the merge path) falls back
# to the full walk when the diff reaches one. `drivers/base.py` is the one that
# is not simply "the script and its baseline", and it is the whole reason this
# gate needed the question asked rather than the pattern copied: the gate
# predicate set is DERIVED from that file (`capability_predicate_names`), so a
# new trivial-constant-return predicate there turns previously-invisible skips
# in files nobody touched into counted app gates. Same guard, same reason as the
# publication tooling's own identifier ratchet, which established the pattern
# (merge-gates.md § Local-merge gates -> The 10-second budget, rule 2's diff
# hand-off).
FULL_SCAN_TRIGGERS = (
    Path(__file__).resolve().relative_to(REPO).as_posix(),
    BASELINE_PATH.relative_to(REPO).as_posix(),
    BASE_DRIVER_FILE.relative_to(REPO).as_posix(),
)

# The driver predicates that identify WHICH app is under test. A skip reached
# under one of these is an app gate. `is_mobile()` is deliberately absent: it
# names a form factor, which is a legitimate structural branch (convention 3),
# not an app identity.
APP_PREDICATES = frozenset(
    {
        "is_tui",
        "is_web",
        "is_linux",
        "is_windows",
        "is_macos",
        "is_ios",
        "is_android",
    }
)

# Identity predicates never count as capability debt (mode 5) — each is
# correct-by-design False for every app but its own, not an unbuilt override.
IDENTITY_PREDICATES = APP_PREDICATES | {"is_mobile"}

# Declaring helpers from helpers/app_surface.py. A skip routed through one of
# these is already visible to --strict-app, so it is NOT undeclared debt.
DECLARING_HELPERS = frozenset({"skip_unbuilt", "declared_absence", "skip_environment"})


def _call_name(node: ast.AST) -> str | None:
    """`pytest.skip` -> "pytest.skip"; `skip_unbuilt` -> "skip_unbuilt"."""
    if not isinstance(node, ast.Call):
        return None
    func = node.func
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def _mentions_app_predicate(node: ast.AST, gate_predicates: frozenset[str] = APP_PREDICATES) -> bool:
    """True if `node` calls any gate predicate anywhere inside it.

    `gate_predicates` defaults to the app-identity set but also accepts the
    mode-5 capability-predicate set unioned in — the two shapes (`is_tui()`
    and `supports_unclean_kill()`) are structurally identical, just naming a
    different kind of gate. Walks the whole subtree so a negated disjunction
    (`not (d.is_web() or d.is_linux())`) counts, as does a guard reached
    through a local alias (`drv = self.driver; if drv.is_tui()`).
    """
    for child in ast.walk(node):
        if isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute):
            if child.func.attr in gate_predicates:
                return True
        # A bare reference also counts: `if driver.is_tui:` (a truthy bound
        # method — a real bug, but still an app gate we want to see).
        if isinstance(child, ast.Attribute) and child.attr in gate_predicates:
            return True
    return False


class _AppGateVisitor(ast.NodeVisitor):
    """Collect bare `pytest.skip` calls that sit under an app-gating guard.

    "Under" means lexically inside the body (or orelse) of an `if` whose test
    mentions a gate predicate — an app-identity predicate (`is_tui()`) or a
    driver-capability predicate (`supports_unclean_kill()`, mode 5) — which is
    how every real site in this tree is written. A skip guarded only by an
    environment condition (network, DNS, a missing fixture) is correctly
    ignored: it is not an app gate, and --strict-app must not touch it.
    """

    def __init__(self, gate_predicates: frozenset[str] = APP_PREDICATES) -> None:
        self.gate_predicates = gate_predicates
        self.sites: list[tuple[int, str]] = []
        self._app_guard_depth = 0

    def visit_If(self, node: ast.If) -> None:
        gated = _mentions_app_predicate(node.test, self.gate_predicates)
        if gated:
            self._app_guard_depth += 1
        for stmt in node.body:
            self.visit(stmt)
        for stmt in node.orelse:
            self.visit(stmt)
        if gated:
            self._app_guard_depth -= 1

    def visit_Call(self, node: ast.Call) -> None:
        name = _call_name(node)
        if name == "skip" and self._app_guard_depth > 0:
            # `pytest.skip` / `skip` reached under an app guard, and not via a
            # declaring helper (those are Name calls with different names).
            self.sites.append((node.lineno, "pytest.skip under an app-gating guard"))
        elif name in DECLARING_HELPERS:
            # Declared — visible to --strict-app. Not debt.
            pass
        self.generic_visit(node)


def undeclared_app_gates(
    path: Path, gate_predicates: frozenset[str] = APP_PREDICATES
) -> list[tuple[int, str]]:
    """(line, why) for every undeclared app-gated skip in `path`."""
    try:
        tree = ast.parse(path.read_text())
    except (OSError, UnicodeDecodeError, SyntaxError):
        return []
    visitor = _AppGateVisitor(gate_predicates)
    visitor.visit(tree)
    return sorted(visitor.sites)


def _scan_files(
    scan_root: Path = E2E_ROOT, only: set[str] | None = None
) -> list[Path]:
    """Every scanned `*.py`, or just `only` (scan-root-relative posix paths).

    `only` still passes through SCAN_DIRS: a changed file outside those four
    subtrees is not this gate's subject and must not become one by being named
    in a diff. A named path that no longer exists is a deletion, which drops out
    of the counts — exactly what the ratchet needs to read as zero.
    """
    files: list[Path] = []
    for sub in SCAN_DIRS:
        base = scan_root / sub
        if not base.is_dir():
            continue
        if only is None:
            files.extend(sorted(base.rglob("*.py")))
        else:
            prefix = sub + "/"
            files.extend(
                sorted(
                    scan_root / rel
                    for rel in only
                    if rel.startswith(prefix) and (scan_root / rel).is_file()
                )
            )
    return files


# ─── Mode 5: driver-capability predicates ──────────────────────────────────
# A capability predicate is a `PlatformDriver` method whose base body is just
# `return <constant>` (docstring optional) — a driver silently gets that
# constant answer until it overrides the method. `is_<app>()` predicates have
# the same *shape* but are excluded (IDENTITY_PREDICATES): a False default
# there is correct-by-design for every app but one, not debt.


def _is_trivial_constant_return(func: ast.FunctionDef) -> bool:
    """True if `func`'s body is (an optional docstring, then) exactly one
    `return <constant-or-nothing>` — the "silent default" shape."""
    body = func.body
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        body = body[1:]  # skip the docstring
    if len(body) != 1 or not isinstance(body[0], ast.Return):
        return False
    value = body[0].value
    return value is None or isinstance(value, ast.Constant)


def _is_abstract(func: ast.FunctionDef) -> bool:
    return any(
        (isinstance(d, ast.Name) and d.id == "abstractmethod")
        or (isinstance(d, ast.Attribute) and d.attr == "abstractmethod")
        for d in func.decorator_list
    )


def _is_bare_none_annotation(func: ast.FunctionDef) -> bool:
    """True for a literal `-> None` return annotation — a side-effecting
    ACTION hook (`enable_dns_fake_provider`), not a predicate/data getter
    callers branch on. Excluded even when its body trivially `return`s: a
    documented intentional no-op default (every native client enables the
    behavior another way; only web overrides) is not capability debt, and
    including it was this scan's own first false positive."""
    ann = func.returns
    return isinstance(ann, ast.Constant) and ann.value is None


def capability_predicate_names(base_path: Path = BASE_DRIVER_FILE) -> frozenset[str]:
    """`PlatformDriver` methods that are trivial constant-return capability
    predicates — override candidates a driver forgets at its own risk
    (testing.md convention 7, mode 5). Excludes the `is_<app>` identity
    family, abstract methods (fail loudly, not silently, if unimplemented),
    and bare `-> None` action hooks (side effects, not predicates)."""
    try:
        tree = ast.parse(base_path.read_text())
    except (OSError, UnicodeDecodeError, SyntaxError):
        return frozenset()
    names = []
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef) and node.name == "PlatformDriver":
            for item in node.body:
                if (
                    isinstance(item, ast.FunctionDef)
                    and not item.name.startswith("_")
                    and item.name not in IDENTITY_PREDICATES
                    and not _is_abstract(item)
                    and not _is_bare_none_annotation(item)
                    and _is_trivial_constant_return(item)
                ):
                    names.append(item.name)
    return frozenset(names)


def _is_trivial_true_return(func: ast.FunctionDef) -> bool:
    """True if `func`'s body is (an optional docstring, then) exactly one
    `return True` — how a leaf driver claims its own identity predicate.
    `PlatformDriver`'s own `is_<app>()` bodies are `return False` and must
    NOT match this (else the base class itself looks like a leaf for every
    app at once — this scan's own second false positive, caught by the
    `test_leaf_app_drivers_finds_the_seven_by_their_own_is_x_override` proof
    before it ever reached the real tree)."""
    body = func.body
    if (
        body
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    ):
        body = body[1:]
    return (
        len(body) == 1
        and isinstance(body[0], ast.Return)
        and isinstance(body[0].value, ast.Constant)
        and body[0].value.value is True
    )


def _parse_driver_classes(drivers_root: Path = DRIVERS_ROOT) -> dict[str, dict]:
    """{class_name: {"base": str|None, "methods": set[str], "true_returning":
    set[str], "file": str}} for every class directly defined under
    `drivers/*.py` (single-inheritance only — every driver class in this tree
    has exactly one base). `true_returning` is the subset of `methods` whose
    trivial body is `return True` — how `leaf_app_drivers` tells a real
    identity override from the base class's own `return False` default."""
    classes: dict[str, dict] = {}
    for path in sorted(drivers_root.glob("*.py")):
        try:
            tree = ast.parse(path.read_text())
        except (OSError, UnicodeDecodeError, SyntaxError):
            continue
        for node in tree.body:
            if not isinstance(node, ast.ClassDef):
                continue
            base = None
            if node.bases:
                b = node.bases[0]
                if isinstance(b, ast.Name):
                    base = b.id
                elif isinstance(b, ast.Attribute):
                    base = b.attr
            funcs = [n for n in node.body if isinstance(n, ast.FunctionDef)]
            methods = {n.name for n in funcs}
            true_returning = {n.name for n in funcs if _is_trivial_true_return(n)}
            classes[node.name] = {
                "base": base,
                "methods": methods,
                "true_returning": true_returning,
                "file": path.name,
            }
    return classes


def leaf_app_drivers(classes: dict[str, dict]) -> dict[str, str]:
    """{class_name: app_name} for the 7 concrete per-app driver classes, found
    STRUCTURALLY — a class whose own `is_<app>()` override `return`s `True` —
    rather than a hand-maintained name list (the exact "never hand-maintain a
    count of a code class" lesson: a name-keyed enumeration can only rediscover what someone
    already found). Checking the return VALUE, not just the method's
    presence, is what keeps `PlatformDriver` itself — which defines every
    `is_<app>()` too, all returning `False` — from looking like a leaf for
    all seven apps at once."""
    apps = {p[len("is_") :] for p in APP_PREDICATES}
    leaves = {}
    for name, info in classes.items():
        for app in apps:
            if f"is_{app}" in info["true_returning"]:
                leaves[name] = app
    return leaves


def capability_debt(
    drivers_root: Path = DRIVERS_ROOT, base_path: Path = BASE_DRIVER_FILE
) -> dict[str, dict[str, str]]:
    """{method_name: {app_name: resolving_class_name}} — for every capability
    predicate, which class in each leaf app driver's MRO chain (leaf up to,
    but excluding, `PlatformDriver`) actually defines it. `"PlatformDriver"`
    means nobody along the chain overrode it — the driver gets the raw base
    default; any other resolver still worth a glance if it isn't the leaf's
    own class — that's silent inheritance from a possibly-different driver's
    assumptions, exactly the shape of the pre-fix tui
    `supports_unclean_kill` bug (`HttpBridgeDriver`'s override tested a Popen
    tui never had)."""
    methods = sorted(capability_predicate_names(base_path))
    classes = _parse_driver_classes(drivers_root)
    leaves = leaf_app_drivers(classes)

    report: dict[str, dict[str, str]] = {m: {} for m in methods}
    for cls_name, app in leaves.items():
        chain = [cls_name]
        seen = {cls_name}
        cur = cls_name
        while True:
            base = classes.get(cur, {}).get("base")
            if not base or base == "PlatformDriver" or base in seen:
                break
            chain.append(base)
            seen.add(base)
            cur = base
        for m in methods:
            resolver = "PlatformDriver"
            for c in chain:
                if m in classes.get(c, {}).get("methods", set()):
                    resolver = c
                    break
            report[m][app] = resolver
    return report


def run_capability_debt(
    drivers_root: Path = DRIVERS_ROOT, base_path: Path = BASE_DRIVER_FILE
) -> int:
    """Print the mode-5 inventory. INFORMATIONAL — not a gate, not baselined:
    "a leaf inherits this capability predicate rather than defining its own"
    is not automatically a bug (linux correctly inherits
    `supports_unclean_kill` from `HttpBridgeDriver`), so this is a list for a
    human to glance over, the same way `--list` surfaces mode 2/3 sites for
    triage rather than failing on them outright."""
    report = capability_debt(drivers_root, base_path)
    classes = _parse_driver_classes(drivers_root)
    leaves = leaf_app_drivers(classes)
    leaf_by_app = {app: cls for cls, app in leaves.items()}

    print(
        "Driver-capability coverage-debt inventory (testing.md convention 7, "
        "mode 5) — informational, not a gate.\nA resolver other than the "
        "app's own leaf class means that app answers this capability "
        "predicate via an\ninherited default; worth a human glance to "
        "confirm the inherited answer is actually correct for that app.\n"
    )
    any_debt = False
    for method in sorted(report):
        rows = report[method]
        inherited = {
            app: resolver for app, resolver in rows.items() if resolver != leaf_by_app.get(app)
        }
        if not inherited:
            continue
        any_debt = True
        print(f"  {method}:")
        for app in sorted(inherited):
            print(f"    {app} ({leaf_by_app[app]}) -> {inherited[app]}")
    if not any_debt:
        print("  (every leaf driver overrides every capability predicate directly)")
    return 0


def _gate_predicates(base_path: Path = BASE_DRIVER_FILE) -> frozenset[str]:
    """The full gate-predicate set: app identity (`is_tui` and friends) union
    mode-5 driver-capability predicates (`supports_unclean_kill` and
    friends) — everything a bare `pytest.skip()` under an `if` on it must
    instead declare through `helpers/app_surface.py`."""
    return APP_PREDICATES | capability_predicate_names(base_path)


def current_counts(
    scan_root: Path = E2E_ROOT,
    base_path: Path = BASE_DRIVER_FILE,
    only: set[str] | None = None,
) -> dict[str, list[tuple[int, str]]]:
    """{relative-to-scan_root path: sites}, POSIX-separated keys."""
    gate_predicates = _gate_predicates(base_path)
    counts = {}
    for path in _scan_files(scan_root, only=only):
        sites = undeclared_app_gates(path, gate_predicates)
        if sites:
            counts[path.relative_to(scan_root).as_posix()] = sites
    return counts


def load_baseline(baseline_path: Path = BASELINE_PATH) -> dict[str, int]:
    if not baseline_path.exists():
        return {}
    return json.loads(baseline_path.read_text())


def scoped_paths(
    changed: list[str],
    scan_root: Path = E2E_ROOT,
    repo_root: Path = REPO,
) -> set[str] | None:
    """The scan-root-relative `.py` paths this diff reaches, or None for "walk
    it all" — the diff touched one of the gate's own inputs, so the predicate
    set or the baseline census the comparison rests on may have moved."""
    norm = [c.replace("\\", "/").strip().strip("/") for c in changed]
    norm = [c for c in norm if c]
    triggers = sorted(set(norm) & set(FULL_SCAN_TRIGGERS))
    if triggers:
        print(
            "check-app-gate-ratchet: the diff reaches the gate's own inputs "
            f"({', '.join(triggers)}) — running the full walk, not the "
            "changed-files scan."
        )
        return None
    prefix = scan_root.resolve().relative_to(repo_root.resolve()).as_posix() + "/"
    return {
        c[len(prefix):] for c in norm if c.startswith(prefix) and c.endswith(".py")
    }


def run_gate(
    scan_root: Path = E2E_ROOT,
    baseline_path: Path = BASELINE_PATH,
    changed: list[str] | None = None,
    repo_root: Path = REPO,
) -> int:
    """The gate. `changed` — repo-relative paths, the merge path's own
    `git diff --name-only` — makes it the CHANGED-FILES mode (2026-09-05,
    merge-gates.md § Local-merge gates → The 10-second budget, rule 2's diff
    hand-off). Exact once the predicate set is fixed: `undeclared_app_gates` is
    a pure function of one file's AST, and each count is compared with that
    file's own baseline entry. What fixes the predicate set is
    `drivers/base.py`, which is a FULL_SCAN_TRIGGER for exactly that reason;
    `app-gate-ratchet-full` in the asynchronous check keeps the whole-tree
    witness.
    """
    only: set[str] | None = None
    if changed is not None:
        only = scoped_paths(changed, scan_root, repo_root)
    baseline = load_baseline(baseline_path)
    # The predicate source is DERIVED from `scan_root`, not taken from the
    # module constant: `BASE_DRIVER_FILE` is `E2E_ROOT/drivers/base.py`, so for
    # the real tree this is the same file — but a caller that passes a scan_root
    # would otherwise get that tree's files judged against the REAL repo's
    # predicate set, which is how a scratch fixture silently stops resembling
    # its subject.
    counts = current_counts(
        scan_root, base_path=scan_root / "drivers" / "base.py", only=only
    )

    risen, improved = [], []
    for file, sites in counts.items():
        n, base_n = len(sites), baseline.get(file, 0)
        if n > base_n:
            risen.append((file, base_n, n, sites))
        elif n < base_n:
            improved.append((file, base_n, n))
    for file, base_n in baseline.items():
        if only is not None and file not in only:
            continue  # not read this run; "absent from counts" says nothing
        if file not in counts and base_n > 0:
            improved.append((file, base_n, 0))

    if risen:
        print(
            "check-app-gate-ratchet: FAIL — undeclared app-gated skips rose past "
            "the committed baseline (testing.md convention 7):",
            file=sys.stderr,
        )
        for file, base_n, n, sites in sorted(risen):
            print(f"\n  {file}: {base_n} -> {n}", file=sys.stderr)
            for line_no, why in sites:
                print(f"    {file}:{line_no}: {why}", file=sys.stderr)
        print(
            "\nFix: route the skip through helpers/app_surface.py — "
            "`skip_unbuilt(driver, surface=...)` for a surface this app has not "
            "built yet (a --strict-app failure and a counted ratchet entry), or "
            "`declared_absence(driver, capability=..., doc=...)` for a permanent "
            "platform absence declared in a goal doc. Never widen the baseline to "
            "launder a new undeclared gate in.",
            file=sys.stderr,
        )
        return 1

    total = sum(len(v) for v in counts.values())
    where = (
        f"in the {len(only)} changed path(s) this merge touched"
        if only is not None
        else (
            f"({total} undeclared app-gated skip(s) remaining, "
            f"{baseline_path.name})"
        )
    )
    print(
        f"check-app-gate-ratchet: OK — no file exceeds its baseline {where}."
    )
    if improved:
        print(
            f"  {len(improved)} file(s) improved — run "
            "`just app-gate-ratchet-update` to shrink the ratchet."
        )
    return 0


def run_list(scan_root: Path = E2E_ROOT) -> int:
    counts = current_counts(scan_root)
    total = sum(len(v) for v in counts.values())
    print(f"{total} undeclared app-gated skip(s) in {len(counts)} file(s):\n")
    for file in sorted(counts):
        print(f"  {file}  ({len(counts[file])})")
        for line_no, _why in counts[file]:
            print(f"    :{line_no}")
    return 0


def update_baseline(
    scan_root: Path = E2E_ROOT, baseline_path: Path = BASELINE_PATH
) -> int:
    if baseline_path.exists() and run_gate(scan_root, baseline_path) != 0:
        print(
            "check-app-gate-ratchet: refusing --update-baseline — the gate is "
            "currently failing against the existing baseline. A down-only ratchet "
            "never writes a rise; fix the violations first.",
            file=sys.stderr,
        )
        return 1
    counts = current_counts(scan_root)
    new_baseline = {file: len(v) for file, v in counts.items()}
    baseline_path.parent.mkdir(parents=True, exist_ok=True)
    baseline_path.write_text(json.dumps(new_baseline, indent=2, sort_keys=True) + "\n")
    print(
        f"check-app-gate-ratchet: wrote {len(new_baseline)} file(s) to "
        f"{baseline_path} — commit it alongside your fix."
    )
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--list", action="store_true",
                    help="print every site found, grouped")
    ap.add_argument("--update-baseline", action="store_true",
                    help="regenerate from the current tree; refuses a rise")
    ap.add_argument("--capability-debt", action="store_true",
                    help="the mode-5 inventory (informational, not a gate)")
    ap.add_argument(
        "--paths-from",
        metavar="FILE",
        help="run the gate over only these repo-relative changed paths, one per "
        "line (`-` = stdin) — the merge path's own `git diff --name-only`. A path "
        "among the gate's own inputs (FULL_SCAN_TRIGGERS, which includes "
        "drivers/base.py: it defines the gate predicates) forces the full walk; "
        "the asynchronous merge-gate check always runs the full walk.",
    )
    args = ap.parse_args(argv[1:])
    if args.paths_from and (
        args.list or args.update_baseline or args.capability_debt
    ):
        ap.error("--paths-from only applies to the gate itself")
    if args.capability_debt:
        return run_capability_debt()
    if args.list:
        return run_list()
    if args.update_baseline:
        return update_baseline()
    changed: list[str] | None = None
    if args.paths_from:
        if args.paths_from == "-":
            changed = [ln.strip() for ln in sys.stdin if ln.strip()]
        else:
            with open(args.paths_from, encoding="utf-8") as fh:
                changed = [ln.strip() for ln in fh if ln.strip()]
    return run_gate(changed=changed)


if __name__ == "__main__":
    sys.exit(main(sys.argv))

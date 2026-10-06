#!/usr/bin/env -S uv run --quiet
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyyaml"]
# ///
"""ui.yaml registry lint — the *reverse* and *internal* directions of UI-id conformance.

``scripts/lint-ui-elements.py`` answers one question: **is every id ui.yaml declares
implemented by every app?** (spec → app). That leaves two directions unguarded, and
both carry live drift today:

1. **internal** — ``pages``/``components``/``onboarding``/``global`` lists reference ids
   that have no entry in the bottom ``elements:`` registry, even though ui.yaml's own
   header says the registry "defines types and descriptions for all IDs (including
   optional/extra ones)". Nothing checks it.
2. **reverse** — an app renders an id that appears **nowhere** in ui.yaml (app → spec).
   ``ui.yaml`` rule A calls a new/extra id a deviation needing explicit user approval,
   but no lint has ever looked in this direction, so an invented id ships silently.

Both matter beyond bookkeeping: they are the input-set question for a generated
id-constant module. A generator emitting from the registry cannot emit a constant for a
page-list id the registry never got, so every one of those ids would keep its
hand-written literal — defeating the point at exactly the sites that most need it.

Three reports
-------------
``referenced-not-registered``  page/component lists → missing registry row  (direction 1)
``registered-not-referenced``  registry row no page list uses               (direction 1, orphan side)
``rendered-not-declared``      app render site → id absent from ui.yaml     (direction 2)

Advisory by default (like ``just i18n-lint`` / ``i18n-coverage``): it prints
candidates to triage and exits 0. ``--strict`` makes any finding exit 1 — for a future
merge gate, once the current backlog is triaged. ``--json`` emits machine-readable
output. ``--report <name>`` restricts to one report.

Caveats (why ``rendered-not-declared`` is a candidate list, not a verdict)
-------------------------------------------------------------------------
* It anchors on each app's **own id-setting syntax** (``data-testid="…"``,
  ``testTag("…")``, ``accessibilityIdentifier("…")``, ``AutomationId="…"``,
  ``set_test_id(…, "…")``), so an ordinary kebab-case string elsewhere in the source
  is not mistaken for an id. tui has no distinguishing syntax — its ids are the first
  argument of ``Element::`` constructors — so tui is anchored on those constructors.
* Test-only sources still count: a test that renders an undeclared id is asserting on
  an id the spec does not have, which is the same drift one step removed.
* Dynamically composed ids (``format!("{}-tab", …)``) are invisible here by design —
  ``ui.yaml``'s ``id_generation_patterns`` section owns that class.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent


def ui_yaml_path(root: Path) -> Path:
    return root / "tests" / "e2e-unified" / "ui.yaml"

# A ui.yaml element id: lowercase alnum segments joined by '-', at least two segments.
# (Three registry ids carry an underscore inside a segment — `inbox-mode-allow_knock`
# and friends — so the segment class allows `_` after the first character.)
KEBAB = r"[a-z][a-z0-9_]*(?:-[a-z0-9_]+)+"

# Per app: source dir, file extensions, and the regexes that mark "this string is an
# element id". Group 1 of each pattern is the id.
APPS: dict[str, dict] = {
    "web": {
        "dir": "apps/fauna-web/src",
        "exts": {".svelte", ".ts", ".js"},
        "patterns": [rf'data-testid="({KEBAB})"'],
    },
    "windows": {
        "dir": "apps/fauna-windows",
        "exts": {".xaml", ".cs"},
        "patterns": [rf'AutomationId="({KEBAB})"'],
    },
    "android": {
        "dir": "apps/fauna-android",
        "exts": {".kt"},
        "patterns": [rf'testTag\("({KEBAB})"\)'],
    },
    "apple": {
        "dir": "apps/fauna-apple",
        "exts": {".swift"},
        "patterns": [rf'accessibilityIdentifier\("({KEBAB})"\)'],
    },
    "linux": {
        "dir": "apps/fauna-linux/src",
        "exts": {".rs"},
        "patterns": [rf'set_test_id\([^,()]+,\s*"({KEBAB})"\)'],
    },
    "tui": {
        "dir": "apps/fauna-tui/src",
        "exts": {".rs"},
        # tui ids are the first argument of an Element constructor.
        "patterns": [rf'Element::[a-z_]+\(\s*"({KEBAB})"'],
    },
}

# Sections whose `elements` / `optional_elements` / `platform_elements` lists name ids.
LIST_SECTIONS = ("pages", "onboarding", "global", "components", "navigation")

# Keys whose list value names element ids. Kept as data, not scattered through `walk`,
# because "one more key nobody walked" is this checker's recurring defect: `components:`
# and `navigation.tabs` were both found unwalked on 2026-08-10 (64 hidden ids), and
# `children:` on 2026-08-11 (5 more). `unknown_id_list_keys` below turns the next
# occurrence into a test failure instead of a silent blind spot.
ID_LIST_KEYS = ("elements", "optional_elements", "children", "errors")

# Keys whose list value is deliberately NOT element ids, and why. A key must be in
# exactly one of these two sets, or `unknown_id_list_keys` reports it.
NON_ID_LIST_KEYS = {
    "used_in": "page names a component appears on, not ids",
    "state_fields": "snapshot field names the page reads, not ids",
    "platforms": "platform names",
    "pages": "navigation.layouts.*.pages — page names",
    "admin_pages": "navigation.layouts.*.admin_pages — page names",
    "more": "navigation.layouts.*.more — page names",
    "sidebar": "navigation.layouts.*.sidebar — page names",
    "navbar": "navigation.layouts.*.navbar — page names",
}

REPORTS = ("referenced-not-registered", "registered-not-referenced", "rendered-not-declared")


def load_ui(root: Path) -> tuple[dict, str]:
    raw = ui_yaml_path(root).read_text(encoding="utf-8")
    return yaml.safe_load(raw) or {}, raw


def collect_referenced(doc: dict) -> dict[str, list[str]]:
    """Every id named by a page/component/onboarding/global element list → where."""
    refs: dict[str, list[str]] = {}

    def note(element_id: str, source: str) -> None:
        refs.setdefault(element_id, []).append(source)

    def walk(node, path: str) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                child = f"{path}.{key}" if path else key
                if key in ID_LIST_KEYS and isinstance(value, list):
                    # `children:` names the CHILD COMPONENT ids a container composes
                    # (post-card → interaction-bar, post-tip-list, feed-post-actions-menu,
                    # …). Measured 2026-08-11: unwalked, so 5 ids whose only reference is
                    # a `children:` list were invisible to both reports at once, exactly
                    # like the `components:` gap before it. `errors:` names the error
                    # element a page surfaces (`errors: [error-message]`).
                    for item in value:
                        if isinstance(item, str):
                            note(item, child)
                elif key == "platform_elements" and isinstance(value, dict):
                    for platform, ids in value.items():
                        if isinstance(ids, list):
                            for item in ids:
                                if isinstance(item, str):
                                    note(item, f"{child}.{platform}")
                elif key == "components" and isinstance(value, list):
                    # A page/component's OWN `components:` list names container ids
                    # it uses (ui.yaml header: "List of component IDs used by this
                    # page"). Distinct from the top-level `components:` SECTION
                    # (a dict of name -> definition), which `walk` recurses into
                    # via the `else` branch below — the list/dict split is what
                    # tells the two apart.
                    for item in value:
                        if isinstance(item, str):
                            note(item, child)
                elif child == "navigation.tabs" and isinstance(value, list):
                    # navigation.tabs: a bare list of "{page}-tab" ids. Path-exact —
                    # navigation.layouts.{mobile,desktop}.tabs/sidebar share the key
                    # name "tabs" but list PAGE names, not element ids; must not match.
                    for item in value:
                        if isinstance(item, str):
                            note(item, child)
                elif child == "navigation.gated_tabs" and isinstance(value, list):
                    # navigation.gated_tabs: [{id: ..., gate: ..., ...}, ...] —
                    # the id is a nav entry name exactly like a tabs list member.
                    for item in value:
                        if isinstance(item, dict) and isinstance(item.get("id"), str):
                            note(item["id"], child)
                elif child == "navigation.sub_page_nav_rows" and isinstance(value, dict):
                    # navigation.sub_page_nav_rows: {<shell>: {id: ..., key: ..., …}} —
                    # each shell's `id` is the indexed rail-row element
                    # (`admin-nav-row[<page key>]`), a nav entry like a gated tab's.
                    for shell, entry in value.items():
                        if isinstance(entry, dict) and isinstance(entry.get("id"), str):
                            note(entry["id"], f"{child}.{shell}")
                else:
                    walk(value, child)
        elif isinstance(node, list):
            for index, item in enumerate(node):
                walk(item, f"{path}[{index}]")

    for section in LIST_SECTIONS:
        if section in doc:
            walk(doc[section], section)
    return refs


def collect_declared(doc: dict, registry: dict, referenced: dict) -> set[str]:
    """Every id ui.yaml *declares*, by any of its structural means.

    "Declared" is wider than the registry — an id a page list names is declared even
    while its registry row is still missing (that gap is `referenced-not-registered`'s
    business) — but it is strictly **structural**. It deliberately does NOT include ids
    that merely appear somewhere in ui.yaml's raw text: this check used to fall back to
    a whole-file regex, which let a bare *prose mention inside a comment* count as a
    declaration and hid three live rule-A findings (`nostr-settings-link`,
    `atproto-settings-link`, and `role-badge` — the last recorded in a comment as
    REMOVED while apple still renders it, `ui.yaml:8386`). A comment describing an id is
    not a declaration of it.

    The structural means, all of which name a thing ui.yaml defines:
      * a row in the bottom `elements:` registry;
      * a reference from any id list (`collect_referenced`);
      * a top-level `pages:` / `components:` / `onboarding:` key — the container's own id,
        which apps legitimately render on the block itself (`web-settings`,
        `admin-bridges-rotate-confirm`).
    """
    declared = set(registry) | set(referenced)
    for section in ("pages", "components", "onboarding"):
        node = doc.get(section)
        if isinstance(node, dict):
            declared |= {key for key in node if isinstance(key, str)}
    # `harness_probe_ids:` — ids the e2e harness renders to test ITSELF (a dispatch
    # round-trip, a window-up sentinel). Declared in ui.yaml but deliberately kept OUT of
    # the `elements:` registry, so the reverse gate can go strict without a probe being
    # read as product surface owed by all 7 apps. Ratified 2026-08-11; ui.yaml's own
    # section comment carries the rationale and the qualifying rule.
    probes = doc.get("harness_probe_ids")
    if isinstance(probes, list):
        declared |= {item for item in probes if isinstance(item, str)}
    return declared


def unknown_id_list_keys(doc: dict) -> dict[str, list[str]]:
    """Keys holding a list of kebab-ish strings that are in neither allowlist → paths.

    The guard against this checker's recurring defect. Three separate id-list key shapes
    (`components:`, `navigation.tabs`, `children:`) were each found unwalked *after* the
    lint shipped, and each one hid real findings from both reports simultaneously. Rather
    than wait for a fourth, every list-of-strings key in the walked sections must be
    classified: walked (`ID_LIST_KEYS`, `platform_elements`, path-exact
    `navigation.tabs`/`gated_tabs`) or explicitly not-ids (`NON_ID_LIST_KEYS`). A new
    ui.yaml key lands here and fails a test instead of going quietly unchecked.
    """
    unknown: dict[str, list[str]] = {}
    walked = set(ID_LIST_KEYS) | {"components", "tabs", "gated_tabs"}

    def walk(node, path: str, parent_key: str | None) -> None:
        if isinstance(node, dict):
            for key, value in node.items():
                child = f"{path}.{key}" if path else key
                # platform_elements' sub-keys are platform names; its lists are walked.
                if parent_key != "platform_elements" and isinstance(value, list):
                    if value and all(isinstance(item, str) for item in value):
                        if key not in walked and key not in NON_ID_LIST_KEYS:
                            unknown.setdefault(key, []).append(child)
                walk(value, child, key)
        elif isinstance(node, list):
            for index, item in enumerate(node):
                walk(item, f"{path}[{index}]", parent_key)

    for section in LIST_SECTIONS:
        if section in doc:
            walk(doc[section], section, None)
    return unknown


# Per-app TEST source sets. An id set inside one is a test FIXTURE's tag (a probe
# widget a unit test mounts to drive a helper), not an app render site — it can
# never reach a user, so it cannot be a rule-A deviation, and counting it would
# let a unit test's scaffolding move the `rendered-not-declared` ratchet (it did:
# android's `OfflineGateTest.kt` mounted a `testTag("in-dialog")` button on
# 2026-08-19 and the tier_1 ceiling went 1 → 2 on `origin/main` unnoticed until
# 2026-08-21). Gradle source sets for android; the `*Tests` project/target
# directories for windows (`FaunaApp.Tests`) and apple (`Fauna-macOSTests`,
# `FaunaKitTests`). Web/linux/tui keep their tests beside the code with no
# id-setting syntax of their own, so they need no entry.
_TEST_SOURCE_PARTS = frozenset({"test", "androidTest"})


def _is_test_source(rel: Path) -> bool:
    parts = rel.parts
    for i, part in enumerate(parts[:-1]):
        if part in _TEST_SOURCE_PARTS and i > 0 and parts[i - 1] == "src":
            return True
        if part.endswith("Tests"):
            return True
    return False


def _source_files(base: Path) -> list[Path]:
    """The files under ``base`` git counts as source: tracked, plus untracked
    ones not ignored — never ignored build output. MSBuild's ``obj/`` keeps a
    copy of every page's XAML, and a stale one outlives its source: a deleted
    page's ids then read as rendered-not-declared only in the one checkout
    still holding that build output (2026-09-30,
    `obj/Release/.../BlueskyPage.xaml`). Outside a git checkout
    (a copied tree in a unit test) every file counts, as before."""
    try:
        listed = subprocess.run(
            ["git", "-C", str(base), "ls-files", "-z", "--cached", "--others",
             "--exclude-standard", "--", "."],
            capture_output=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return sorted(p for p in base.rglob("*") if p.is_file())
    return sorted(base / name for name in listed.decode("utf-8").split("\0") if name)


def collect_rendered(app: str, root: Path) -> dict[str, str]:
    """Every id an app sets through its own id-setting syntax → first file:line.

    Deliberately **literal-only**, and it stays that way after an app adopts the
    generated constants (`scripts/ui-ids-generate.py`). The sibling lint
    `lint-ui-elements.py` had to learn the constant form because it asks "is this
    element implemented?" and an adopted app has no literal left to find. This one
    asks the opposite question — "which rendered ids is ui.yaml missing?" — and a
    reference to a generated constant *cannot* name an undeclared id, because the
    constant only exists if ui.yaml declared it. So constant references would
    contribute exactly zero findings, while the literals that remain are precisely
    the ones that could have been invented. Adoption therefore shrinks this report to
    the sites still able to carry a rule-A deviation, which is the point; a session
    finding tui's count at 0 after adoption is looking at success, not a blind spot.
    """
    spec = APPS[app]
    base = root / spec["dir"]
    compiled = [re.compile(p) for p in spec["patterns"]]
    found: dict[str, str] = {}
    if not base.exists():
        return found
    for path in _source_files(base):
        if not path.is_file() or path.suffix not in spec["exts"]:
            continue
        if _is_test_source(path.relative_to(base)):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for pattern in compiled:
            for match in pattern.finditer(text):
                element_id = match.group(1)
                if element_id in found:
                    continue
                line = text[: match.start()].count("\n") + 1
                found[element_id] = f"{path.relative_to(root)}:{line}"
    return found


def build_findings(reports: tuple[str, ...], root: Path = REPO_ROOT) -> dict:
    doc, _raw = load_ui(root)
    registry = doc.get("elements") or {}
    referenced = collect_referenced(doc)
    result: dict[str, object] = {}

    if "referenced-not-registered" in reports:
        result["referenced-not-registered"] = {
            element_id: sorted(set(sources))
            for element_id, sources in sorted(referenced.items())
            if element_id not in registry
        }

    if "registered-not-referenced" in reports:
        result["registered-not-referenced"] = sorted(set(registry) - set(referenced))

    if "rendered-not-declared" in reports:
        declared = collect_declared(doc, registry, referenced)
        per_app: dict[str, dict[str, str]] = {}
        for app in APPS:
            rendered = collect_rendered(app, root)
            per_app[app] = {
                element_id: where
                for element_id, where in sorted(rendered.items())
                if element_id not in declared
            }
        result["rendered-not-declared"] = per_app

    return result


def print_human(findings: dict) -> None:
    if "referenced-not-registered" in findings:
        items = findings["referenced-not-registered"]
        print(f"\n=== referenced-not-registered: {len(items)} ===")
        print("ids a page/component list names that have NO entry in the `elements:` registry.")
        for element_id, sources in items.items():
            print(f"  {element_id:52s} <- {sources[0]}")

    if "registered-not-referenced" in findings:
        items = findings["registered-not-referenced"]
        print(f"\n=== registered-not-referenced: {len(items)} ===")
        print("registry rows no page/component list uses (retired elements, or a missing list entry).")
        for element_id in items:
            print(f"  {element_id}")

    if "rendered-not-declared" in findings:
        per_app = findings["rendered-not-declared"]
        total = sum(len(v) for v in per_app.values())
        print(f"\n=== rendered-not-declared: {total} ===")
        print("ids an app renders that appear NOWHERE in ui.yaml (rule A: a new id needs approval).")
        for app, items in per_app.items():
            print(f"  {app} ({len(items)}):")
            for element_id, where in items.items():
                print(f"    {element_id:48s} {where}")


def count(findings: dict) -> int:
    total = 0
    for key, value in findings.items():
        if key == "rendered-not-declared":
            total += sum(len(v) for v in value.values())
        else:
            total += len(value)
    return total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--report",
        action="append",
        choices=REPORTS,
        help="restrict to one report (repeatable); default is all three",
    )
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument(
        "--strict", action="store_true", help="exit 1 when any finding is present"
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=REPO_ROOT,
        help="repo root to analyse (tests point this at a synthetic tree)",
    )
    args = parser.parse_args(argv)

    reports = tuple(args.report) if args.report else REPORTS
    findings = build_findings(reports, args.root)

    if args.json:
        print(json.dumps(findings, indent=2, sort_keys=True))
    else:
        print_human(findings)
        total = count(findings)
        print(f"\n{total} finding(s).", end=" ")
        print(
            "ADVISORY — exits 0. Pass --strict to gate."
            if not args.strict
            else "STRICT — a finding fails this run."
        )

    return 1 if (args.strict and count(findings)) else 0


if __name__ == "__main__":
    sys.exit(main())

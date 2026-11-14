#!/usr/bin/env python3
"""The catalog lint — the seven rules of `docs/goal/architecture/feature-catalog.md`
§ The catalog lint, cheap merge tier, parse-only, stdlib-only.

The rules, in the doc's own numbering:

1. every page's front-matter is well-formed (`slug` equals the stem; `goal` and every
   contract citation resolve to a goal doc **and a heading in it**; every `absences`
   value is a citation);
2. every contract-mapped test id exists in the tree **and carries
   `@pytest.mark.feature("<that slug>")`**;
3. every marker-tagged test appears in some page's contract;
4. no `[nest]` outcome maps to a test that launches an app driver, and no `[app]`
   outcome to one that launches none;
5. the committed `MATRIX.md`, every `## Status` block and `matrix.json` equal a fresh
   render;
6. the ledger holds no record under a feature that no longer maps its test (re-marked,
   renamed, deleted — `just features-prune` removes them), nor a `skipped: unbuilt`
   record whose `surface` the registry now lists as built on its column; no declared-absent app has
   a *passing* record for the feature — nor, for a
   per-outcome absence, for a witness of that outcome (the declaration is
   stale), and a page whose outcomes are all `[nest]` says so in its
   `What a user gets`;
7. the surface registry (`tests/e2e-unified/surfaces.toml`, e2e-conventions.md
   convention 7) reads, and every `@pytest.mark.requires_surface("<key>")` names a
   key it declares; no surface is declared unbuilt on an app whose ledger holds a
   passing record of a test that requires it, nor — keyed by a ui.yaml id — on an app
   whose `ui-actual-<app>.yaml` records that id; and no `product` entry lists all
   seven apps (finished: the entry and its gates are deleted together).

Rules 2 and 3 are the two directions of the same promise, which is why they are
separate rules rather than one: a contract line naming a test that does not carry the
marker breaks feature -> test, and a marker with no contract line breaks test ->
feature. Either alone leaves the catalog half-discoverable.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import features_catalog as fc
import features_scan as fs

REPO = fc.REPO

#: The one sentence a nest-only page uses to say its feature carries no user-facing
#: choice (rule 6). A single recognised phrase rather than a prose sniff: the rule is
#: a real product invariant — a `[nest]` choice with no `[app]` outcome is
#: configuration-file theatre (`docs/goal/principles.md` § One configuration surface)
#: — so the opt-out has to be a deliberate, greppable act, not an accident of wording.
NO_CHOICE_PHRASE = "nothing to choose"

_CITATION_RE = re.compile(r"^(?P<doc>[\w./-]+\.md)\s+§\s+(?P<section>.+?)\s*$")
_HEADING_RE = re.compile(r"^#{1,6}\s+(.*?)\s*$")


def _headings(path: Path) -> list:
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        match = _HEADING_RE.match(line)
        if match:
            out.append(match.group(1).strip())
    return out


def _resolve_citation(citation: str, repo: Path) -> str | None:
    """`None` when the citation resolves; otherwise why it does not."""
    match = _CITATION_RE.match(citation.strip())
    if not match:
        return f"{citation!r} is not a `<doc>.md § <section>` citation"
    doc, section = match.group("doc"), match.group("section")
    path = repo / doc
    if not path.exists():
        return f"{doc} does not exist"
    wanted = section.casefold()
    for heading in _headings(path):
        folded = heading.casefold()
        if folded == wanted:
            return None
        # `§ 6. Mail` against `## 6. Mail — the manual form`: a citation may name the
        # stable head of a heading whose tail is prose.
        if folded.startswith(wanted) and not folded[len(wanted):len(wanted) + 1].isalnum():
            return None
    return f"{doc} has no heading matching § {section}"


def _rule_1(pages: list, repo: Path) -> list:
    """Front-matter well-formed; `goal`, contract citations and absences resolve.

    The slug/stem equality and the structural shape are enforced by `parse_page`
    itself (a page it cannot read is a hard stop, not a warning), so what is left
    here is the half that needs the rest of the tree: do the citations point at
    something that exists.
    """
    problems = []
    for page in pages:
        for label, citation in [("goal", page.goal)] + \
                [(f"absences[{app}]", cite) for app, cite in sorted(page.absences.items())] + \
                [(f"absences[{app} (outcome {n})]", cite)
                 for app, numbers in sorted(page.outcome_absences.items())
                 for n, cite in sorted(numbers.items())]:
            why = _resolve_citation(citation, repo)
            if why:
                problems.append(f"{page.path.name}: {label}: {why}")
        for outcome in page.outcomes:
            why = _resolve_citation(outcome.citation, repo)
            if why:
                problems.append(
                    f"{page.path.name}:{outcome.line}: outcome {outcome.number}: {why}")
    ui_yaml = repo / "tests" / "e2e-unified" / "ui.yaml"
    if any(page.ui_pages for page in pages):
        known = (fc.ui_page_ranges(ui_yaml.read_text(encoding="utf-8"))
                 if ui_yaml.is_file() else {})
        for page in pages:
            for key in page.ui_pages:
                if key not in known:
                    problems.append(
                        f"{page.path.name}: ui-pages: `{key}` names no page in "
                        "tests/e2e-unified/ui.yaml (`just ui-show pages` lists them)")
    return problems


def _rule_2(pages: list, tests: dict, ships=None) -> list:
    """Every mapped test exists and carries the marker for that slug — and, when
    `ships` (the publish allowlist's "does this source path reach the public tree?"
    predicate) is given, is cited in the spelling its shipping status demands.

    The catalog ships and the ledger does not, so a contract's citations are the one
    place a shipped page can point a public reader at a test they do not have (§ The
    coverage contract, *Maintainer-only witnesses*, 2026-09-09 — measured that day:
    seven citations across three pages, every one to a file the allowlist drops for a
    reason that would still be true next month). The two halves are one rule, not an
    exemption list: an unmarked witness must ship, and a `(maintainer-only)` mark must
    name a file that does NOT ship — a mark that outlives its reason hides real,
    public evidence from the reader, so it is a finding of its own. `ships` is None
    only where there is no allowlist to ask (the fixture-world tests, and a public
    clone — where `lint()` has already refused on the missing ledger before this rule
    runs); the merge gate always passes the real predicate through `main()`.
    """
    problems = []
    for page in pages:
        for outcome in page.outcomes:
            for test in outcome.tests:
                facts = tests.get(test)
                if facts is None:
                    problems.append(
                        f"{page.path.name}:{outcome.line}: outcome {outcome.number} maps to "
                        f"`{test}`, which does not exist in the tree")
                    continue
                if ships is not None:
                    file_ships = ships(facts.rel)
                    marked = test in outcome.maintainer_only
                    if marked and file_ships:
                        problems.append(
                            f"{page.path.name}:{outcome.line}: outcome {outcome.number} cites "
                            f"`{test}` as {fc.MAINTAINER_ONLY}, but {facts.rel} SHIPS — the "
                            "mark has outlived its reason; cite the test plainly")
                    elif not marked and not file_ships:
                        problems.append(
                            f"{page.path.name}:{outcome.line}: outcome {outcome.number} maps to "
                            f"`{test}`, whose file {facts.rel} does not ship (the publish "
                            "allowlist drops it) — a public reader would be sent to a test "
                            f"they do not have; cite it as `{fc.maintainer_only_witness(test)}` "
                            "or ship the test")
                if page.slug not in facts.features:
                    problems.append(
                        f"{page.path.name}:{outcome.line}: outcome {outcome.number} maps to "
                        f"`{test}`, which does not carry "
                        f"@pytest.mark.feature(\"{page.slug}\") "
                        f"({facts.rel}:{facts.line})")
    return problems


def _rule_3(pages: list, tests: dict) -> list:
    """Every marker-tagged test appears in some contract."""
    mapped: dict = {}
    for page in pages:
        for outcome in page.outcomes:
            for test in outcome.tests:
                mapped.setdefault(test, set()).add(page.slug)
    known = {page.slug for page in pages}
    problems = []
    for node_id, facts in sorted(tests.items()):
        for slug in facts.features:
            if slug not in known:
                problems.append(
                    f"{facts.rel}:{facts.line}: "
                    f"@pytest.mark.feature(\"{slug}\") names no page in docs/features/")
            elif slug not in mapped.get(node_id, ()):
                problems.append(
                    f"{facts.rel}:{facts.line}: "
                    f"`{node_id}` is tagged feature \"{slug}\" but no outcome in "
                    f"docs/features/{slug}.md maps to it — a witness nobody asked for")
    return problems


def _rule_4(pages: list, tests: dict) -> list:
    """The surface a contract declares must match what the test actually drives."""
    problems = []
    for page in pages:
        for outcome in page.outcomes:
            for test in outcome.tests:
                facts = tests.get(test)
                if facts is None:
                    continue  # rule 2 already reported it
                if outcome.surface == fc.NEST and facts.drives_app:
                    problems.append(
                        f"{page.path.name}:{outcome.line}: outcome {outcome.number} is "
                        f"[nest] but `{test}` launches an app driver — a nest outcome is "
                        "proven app-independently, observed from outside")
                if outcome.surface == "app" and not facts.drives_app:
                    problems.append(
                        f"{page.path.name}:{outcome.line}: outcome {outcome.number} is "
                        f"[app] but `{test}` launches no app driver — an app outcome is "
                        "proven through that app's own UI")
    return problems


def _rule_5(features: Path, repo: Path = REPO) -> list:
    """The committed render equals a fresh one — `MATRIX.md`, every page's `## Status`
    block, `matrix.json`, and the derived version-skew pin.

    The pin joins this rule the moment the release table carries a promoted row
    (§ Tag gate and release table → *The pin is derived from it*); until then
    `check_pin` returns `None` and the hand-written pin stands, unchecked, exactly as
    it did before step 4.
    """
    stale = [f"docs/features/{name} is stale — run `just features-render` and commit it"
             for name in fc.check_all(features)]
    pin = fc.check_pin(features, repo)
    if pin:
        stale.append(
            f"{pin} is stale — it is DERIVED from the catalog's release table now "
            "(its newest promoted row); run `just features-render` and commit it. "
            "Advancing it by hand is what step 4 retired.")
    return stale


def _marks_of(tests: dict) -> tuple:
    """`(app marks, host-platform marks)` from a `scan_tree` answer — the first and
    third halves of `features_catalog.scan_marks`, without a second scan."""
    return ({n: f.apps for n, f in tests.items() if f.apps},
            {n: f.platforms for n, f in tests.items() if f.platforms is not None})


def _rule_6_host_platforms(tests: dict) -> list:
    """Every `@pytest.mark.host_platform(...)` declaration reads (e2e-conventions.md
    convention 7, the fourth skip kind; feature-catalog.md § The ledger, *Stale
    records*, ruling (i)): its words are the ledger's platform vocabulary; it names no
    platform outside its `tests/platform/<os>/` venue; it names none outside every
    column the test can speak for (`COLUMN_PLATFORMS` over its apps — the
    `contradictory` reading); and its folded marks leave at least one platform."""
    problems = []
    for node_id, facts in sorted(tests.items()):
        if facts.platforms is None:
            continue
        where = f"{facts.rel}:{facts.line}: `{node_id.split('::', 1)[1]}`"
        if not facts.platforms:
            problems.append(f"{where}: its host_platform marks share no platform — no "
                            "host can ever run it")
            continue
        unknown = sorted(set(facts.platforms) - fc.PLATFORMS)
        if unknown:
            problems.append(f"{where}: host_platform names {', '.join(map(repr, unknown))}, "
                            f"outside the ledger's platform vocabulary {sorted(fc.PLATFORMS)}")
            continue
        venue = fc.venue_platforms(node_id)
        outside_venue = sorted(set(facts.platforms) - venue)
        if outside_venue:
            problems.append(f"{where}: host_platform names {', '.join(outside_venue)}, outside "
                            f"its venue's platforms {sorted(venue)} — a `tests/platform/<os>/` "
                            "module is declared by its venue")
        # A nest witness (no app driver) speaks for the nest file, which every
        # platform can stamp; an app witness for the columns its marks allow.
        columns = facts.apps or fc.APPS
        spoken = (frozenset().union(*(fc.COLUMN_PLATFORMS.get(a, frozenset()) for a in columns))
                  if facts.drives_app else fc.PLATFORMS)
        outside_columns = sorted(set(facts.platforms) - spoken)
        if outside_columns:
            problems.append(f"{where}: host_platform names {', '.join(outside_columns)}, on "
                            f"which none of its columns ({', '.join(columns)}) runs")
    return problems


def _rule_6(pages: list, ledger: dict, registry: dict | None = None,
            app_marks: dict | None = None, platform_marks: dict | None = None) -> list:
    problems = []
    # A `skipped: unbuilt` record whose surface the registry now lists as built on its
    # column (§ Cell semantics, *Staleness is decided by the registry*) — named one by
    # one, because the commit that widened the entry is the one that owes the prune.
    unbuilt = fc.stale_unbuilt_records(ledger, registry or {})
    for app, slug, node_id, platform in unbuilt:
        record = ledger[app][slug]["tests"][node_id][platform]
        problems.append(
            f"ledger/{app}.json: `{node_id}` under `{slug}` ({platform}) is a stale "
            f"`skipped: unbuilt` record — tests/e2e-unified/surfaces.toml lists "
            f"`{record.get('surface')}` as built on {app} — run `just features-prune` "
            "and commit the ledger")
    # The ledger holds no record under a feature that no longer maps its test, nor one
    # on a platform its column can no longer run on (§ The ledger, *Stale records*).
    # Reported per app file and feature, with the one fix.
    stale: dict = {}
    unrunnable: dict = {}
    for app, slug, node_id, platform in fc.stale_records(pages, ledger, None,
                                                          app_marks, platform_marks):
        if platform is None:
            stale.setdefault((app, slug), []).append(node_id)
        else:
            unrunnable.setdefault((app, slug), set()).add(platform)
    for (app, slug), node_ids in sorted(stale.items()):
        problems.append(
            f"ledger/{app}.json: {len(node_ids)} record(s) under `{slug}` for test(s) its "
            f"contract no longer maps (e.g. `{node_ids[0]}`) — run `just features-prune` "
            "and commit the ledger")
    for (app, slug), platforms in sorted(unrunnable.items()):
        named = ", ".join(f"`{p}`" for p in sorted(platforms))
        why = ((f"a platform the {app} column cannot run it on, or on no leg of the "
                "column's set" if app != fc.NEST else
                "a platform that cannot complete the feature's nest set")
               + " (a `tests/platform/docker/` witness runs only where a docker daemon is, "
               "a `tests/platform/<os>/` or `host_platform`-marked one on those OSes "
               "alone, and a platform whose set a sibling's contains outright is on no "
               "leg — § The ledger, *Stale records*, rulings (e), (g), (h) and (i))")
        problems.append(
            f"ledger/{app}.json: records under `{slug}` on {named}, {why} — run "
            "`just features-prune` and commit the ledger")
    # Every file's top level is the fold of its platforms over the surface's legs
    # (§ The ledger, *Stale records*, rulings (h) and (i)) — complete only once every
    # leg holds a stamp; a top level that disagrees was written before the contract
    # or a marker gained or lost a leg, re-folded without the legs, or merged by hand.
    for app, slug in fc.misfolded_cells(pages, ledger, app_marks, platform_marks):
        problems.append(
            f"ledger/{app}.json: the cell under `{slug}` is not the fold of its platforms "
            "over that surface's legs (§ The ledger, *Stale records*, rulings (h) and "
            "(i)) — run `just features-prune` and commit the ledger")
    for page in pages:
        mapped = fc.mapped_tests(page)
        for app, citation in sorted(page.absences.items()):
            records = ((ledger.get(app) or {}).get(page.slug) or {}).get("tests") or {}
            # A test now carries one record per platform (§ The ledger, *Platform
            # keying*); the declaration is stale if it passed on ANY of them. Only a
            # MAPPED test speaks for the feature (a stale record is reported above),
            # and only one that can witness this column: a test marked for other apps
            # records here as a helper seat (§ Cell semantics, the marked-witness
            # rule — the cell render reads it the same way, `applicable`).
            passing = sorted(t for t, by_platform in records.items()
                              if t in mapped and fc.applicable(t, app, app_marks)
                              and any(r.get("outcome") in fc.PASSING for r in by_platform.values()))
            if passing:
                problems.append(
                    f"{page.path.name}: `{app}` is declared absent ({citation}) but "
                    f"{len(passing)} mapped test(s) PASSED there "
                    f"(e.g. `{passing[0]}`) — the declaration is stale")
        for app, numbers in sorted(page.outcome_absences.items()):
            records = ((ledger.get(app) or {}).get(page.slug) or {}).get("tests") or {}
            for outcome in page.outcomes:
                if outcome.number not in numbers:
                    continue
                # Same evidence, one outcome wide: a witness OF THAT OUTCOME passed on
                # the column excused from it.
                passing = sorted(t for t in outcome.tests
                                 if fc.applicable(t, app, app_marks) and any(r.get("outcome") in fc.PASSING
                                        for r in (records.get(t) or {}).values()))
                if passing:
                    problems.append(
                        f"{page.path.name}: `{app}` is declared absent from outcome "
                        f"{outcome.number} ({numbers[outcome.number]}) but its witness "
                        f"`{passing[0]}` PASSED there — the declaration is stale")
        if page.nest_only and NO_CHOICE_PHRASE not in page.what.casefold():
            problems.append(
                f"{page.path.name}: every outcome is [nest] and no [app] outcome "
                "reaches it, so the page must say in `## What a user gets` that there "
                f"is {NO_CHOICE_PHRASE} — otherwise a knob nobody can reach from the "
                "only configuration surface there is "
                "(docs/goal/principles.md § One configuration surface)")
    return problems


def lint(features: Path = fc.FEATURES, e2e: Path = fs.E2E, repo: Path = REPO, *,
         ships=None) -> list:
    """Every rule's problems, in rule order. An empty list is a green gate.

    `ships(rel) -> bool` is the publish allowlist's shipping predicate for rule 2's
    citation-spelling halves (see `_rule_2`); `main()` wires the real one, a fixture
    world passes its own, and None leaves those two halves unevaluated.
    """
    try:
        pages = fc.load_pages(features)
        ledger = fc.load_ledger(features)
    except fc.CatalogError as exc:
        return [str(exc)]
    tests = fs.scan_tree(e2e, repo=repo)

    slugs = [page.slug for page in pages]
    duplicates = sorted({s for s in slugs if slugs.count(s) > 1})
    problems = [f"duplicate slug: {s}" for s in duplicates]

    problems += _rule_1(pages, repo)
    problems += _rule_2(pages, tests, ships)
    problems += _rule_3(pages, tests)
    problems += _rule_4(pages, tests)
    try:
        problems += _rule_5(features, repo)
    except fc.CatalogError as exc:
        problems.append(str(exc))
    try:
        registry = fc.load_surface_registry(e2e / "surfaces.toml")
    except fc.CatalogError:
        registry = None  # rule 7 reports it; the rules reading it skip their halves
    # Rule 6 judges the files as written — only a file can be pruned — never the
    # two-source reading every cell is computed on (§ Cell semantics, *Two sources*).
    try:
        written = fc.load_ledger(features, last_green=False)
    except fc.CatalogError as exc:
        return problems + [str(exc)]
    app_marks, platform_marks = _marks_of(tests)
    problems += _rule_6(pages, written, registry, app_marks, platform_marks)
    problems += _rule_6_host_platforms(tests)
    problems += _rule_7(tests, e2e / "surfaces.toml")
    if registry is not None:
        problems += _rule_7_declarations(pages, ledger, tests, registry, e2e)
    return problems


def _rule_7(tests: dict, registry_path: Path) -> list:
    """The surface registry reads, and every `requires_surface` key is declared in it
    (e2e-conventions.md convention 7 — the marker's unknown-key rule, without a run)."""
    try:
        registry = fc.load_surface_registry(registry_path)
    except fc.CatalogError as exc:
        return [str(exc)]
    problems = []
    for _node_id, facts in sorted(tests.items()):
        for key in facts.surfaces:
            if key not in registry:
                problems.append(
                    f"{facts.rel}:{facts.line}: "
                    f"@pytest.mark.requires_surface(\"{key}\") names no table in "
                    f"tests/e2e-unified/surfaces.toml")
    return problems


#: A list item in the e2e YAML files that names an element id: `- <kebab-id>`, with an
#: optional trailing comment. `state_fields` paths (dotted) and quoted strings never match.
_YAML_ITEM = re.compile(r"^(\s*)-\s+([a-z0-9][a-z0-9-]*)\s*(?:#.*)?$")
_YAML_KEY = re.compile(r"^(\s*)([A-Za-z0-9_.-]+):(?:\s|$)")


def yaml_list_ids(text: str, under: frozenset | None = None) -> frozenset:
    """Every element id a YAML list item names — with `under`, only items whose nearest
    enclosing key is one of those names (`elements`, `optional_elements`).

    A line reader, not a YAML parse, ON PURPOSE: this lint is the cheap merge tier,
    stdlib-only (the module docstring), and the ui lints' loader is PyYAML. Every list
    the rule 7 fence reads — ui.yaml's and each `ui-actual-<app>.yaml`'s `elements:` —
    is the one-id-per-line block form both files are written in."""
    stack: list = []          # (indent, key)
    out = set()
    for line in text.splitlines():
        item = _YAML_ITEM.match(line)
        if item:
            indent = len(item.group(1))
            while stack and stack[-1][0] > indent:
                stack.pop()
            if under is None or (stack and stack[-1][1] in under):
                out.add(item.group(2))
            continue
        key = _YAML_KEY.match(line)
        if key:
            indent = len(key.group(1))
            while stack and stack[-1][0] >= indent:
                stack.pop()
            stack.append((indent, key.group(2)))
    return frozenset(out)


#: The `ui-actual-<app>.yaml` keys whose lists record an id as PRESENT on the app;
#: `missing`, `missing_pages`, `missing_from_client` and the rest record its absence.
UI_ACTUAL_PRESENT = frozenset({"elements", "optional_elements"})


def _rule_7_declarations(pages: list, ledger: dict, tests: dict, registry: dict,
                         e2e: Path) -> list:
    """Convention 7's two declaration lints and its finished entry (§ Cell semantics,
    *Not built is declared* — the registry the run refutes in both directions):

    (a) a surface declared unbuilt on an app whose ledger holds a PASSING record of a
        mapped test whose map names the surface, read for that record — a key only
        another record taught counts only if this run reached it too (`Declared.requires`)
        — a stale declaration, the twin of a passing test on a declared-absent app;
    (b) a `product` surface keyed by a ui.yaml id, declared unbuilt on an app whose
        `ui-actual-<app>.yaml` records that id present — the fence against declaring
        unbuilt what exists, naming both files;
    (c) a `product` entry whose `built` holds all seven apps — finished (a kids column
        it leaves out is a plane the kids build excises, kept off the column by the
        page's absence)."""
    problems = []
    surface_marks = {node_id: facts.surfaces for node_id, facts in tests.items()
                     if facts.surfaces}
    declared = fc.build_declared(registry, surface_marks, ledger)
    by_slug = {page.slug: page for page in pages}
    for app in fc.APPS:
        passed: dict = {}
        for slug, entry in sorted((ledger.get(app) or {}).items()):
            mapped = fc.mapped_tests(by_slug.get(slug))
            for node_id, by_platform in sorted((entry.get("tests") or {}).items()):
                if node_id not in mapped:
                    continue
                for record in by_platform.values():
                    if record.get("outcome") in fc.PASSING:
                        for key in declared.requires(node_id, record):
                            passed.setdefault(key, node_id)
        for key, node_id in sorted(passed.items()):
            entry = registry.get(key)
            if entry is not None and app not in entry.built:
                problems.append(
                    f"tests/e2e-unified/surfaces.toml [{key}]: declared not built on {app}, "
                    f"but `{node_id}`, which requires it, PASSED there "
                    f"(ledger/{app}.json) — the declaration is stale; add {app} to `built` "
                    "and run `just features-prune`")
    ui_yaml = e2e / "ui.yaml"
    ui_ids = (yaml_list_ids(ui_yaml.read_text(encoding="utf-8"))
              if ui_yaml.is_file() else frozenset())
    present: dict = {}
    for key, entry in sorted(registry.items()):
        if entry.kind != "product":
            continue
        if entry.built >= frozenset(fc.APP_COLUMNS):
            problems.append(
                f"tests/e2e-unified/surfaces.toml [{key}]: built on all seven apps — "
                "finished: delete the entry and its gates (e2e-conventions.md convention 7)")
            continue
        if key not in ui_ids:
            continue
        for app in fc.APPS:
            if app in entry.built:
                continue
            if app not in present:
                actual = e2e / f"ui-actual-{app}.yaml"
                present[app] = (yaml_list_ids(actual.read_text(encoding="utf-8"),
                                              UI_ACTUAL_PRESENT)
                                if actual.is_file() else frozenset())
            if key in present[app]:
                problems.append(
                    f"tests/e2e-unified/surfaces.toml [{key}]: declared not built on {app}, "
                    f"but tests/e2e-unified/ui-actual-{app}.yaml records the ui.yaml id "
                    f"`{key}` present there — declaring unbuilt what exists is a never-move "
                    "(feature-catalog.md § Closing a gap); widen `built` or correct the "
                    "snapshot")
    return problems


def _publish_ships(repo: Path):
    """The publish allowlist's shipping predicate, or None where there is none.

    The allowlist and the transform that reads it live in `scripts/publish/`, which
    never ships — so this resolves to a real predicate in exactly the tree where rule
    2's shipping halves are load-bearing (the maintainers' tree, on the merge gate)
    and to None in a public clone, where `lint()` refuses on the missing ledger before
    rule 2 runs. Loaded by file path, never `import`ed: `features_lint` itself ships,
    and the publish gate's import check rightly refuses a shipped module that names
    an unshipped one in an import statement — the same by-path loading every
    `scripts/publish/test_*.py` uses for the same reason.
    """
    allowlist = repo / "scripts" / "publish" / "allowlist"
    if not allowlist.exists():
        return None  # the guard orphan_test_check honours: absent means "run without it"
    import importlib.util
    spec = importlib.util.spec_from_file_location(
        "_publish_transform_for_features_lint", allowlist.parent / "transform.py")
    transform = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(transform)
    rules = transform.parse_allowlist(allowlist)
    return lambda rel: transform.ships_source_path(rel, *rules)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--features", type=Path, default=fc.FEATURES,
                        help="the catalog directory (default docs/features/)")
    parser.add_argument("--e2e", type=Path, default=fs.E2E,
                        help="the e2e test tree to scan (default tests/e2e-unified/)")
    args = parser.parse_args(argv)

    problems = lint(args.features, args.e2e, ships=_publish_ships(REPO))
    if not problems:
        print("features-lint: OK")
        return 0
    print(f"features-lint: {len(problems)} problem(s)", file=sys.stderr)
    for problem in problems:
        print(f"  {problem}", file=sys.stderr)
    print("\nContract: docs/goal/architecture/feature-catalog.md § The catalog lint",
          file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

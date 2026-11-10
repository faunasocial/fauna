#!/usr/bin/env python3
"""The feature catalog's parse / compute / render core.

Owner of the *format*: `docs/goal/architecture/feature-catalog.md` — this module is
that document made executable, and every rule below cites the section it implements.
Two thin CLIs sit on top: `scripts/features-render.py` (writes) and
`scripts/features_lint.py` (the seven rules of § The catalog lint).

**Pure stdlib, sub-second, idempotent** (§ Render). No PyYAML: the front-matter shape
is a fixed, tiny grammar — scalars plus the one nested `absences` map — and a
hand-rolled reader keeps the lint parse-only on every machine, which is the same
reason `features_scan.py` reads the test tree with `ast` rather than by import.

The inputs are a **features directory** and the ledger inside it, never "the repo":
the catalog is empty in the tree until the pages land, and a library that can only
run against its own repo cannot be tested against one that is not.
"""

from __future__ import annotations

import json
import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FEATURES = REPO / "docs" / "features"

#: The seven app columns — the apps, each its own column and its own ledger file, in
#: the order the seed matrix used. Also the only app MARKERS a test may carry: a kids
#: column is never a mark (`applicable`).
APP_COLUMNS = ("web", "linux", "windows", "macos", "ios", "android", "tui")

#: The kids channels and the app each is a store identity of (§ Cell semantics, *A
#: second store identity of an app … is a COLUMN*): the android and ios apps built
#: under the `kids` flavor, each a column of its own with its own ledger file and
#: app-axis value (`--app android-kids`). A kids column's applicable set is its
#: parent's marked witnesses plus the unmarked ones, and it runs where its parent runs.
COLUMN_PARENT = {"android-kids": "android", "ios-kids": "ios"}

#: Every column, fixed here so the matrix, the JSON and every page's status block
#: agree: the seven, then the kids channels (`matrix.json` names each one's parent,
#: so a reader can group them).
APPS = APP_COLUMNS + tuple(COLUMN_PARENT)


def parent_app(column: str) -> str:
    """The app a column is a build of — itself for the seven, the parent for a kids
    channel. What a column's app marks, platforms and source tree are read as."""
    return COLUMN_PARENT.get(column, column)

#: `nest.json` is a ledger file like an app's, but never a column (§ The two surfaces).
NEST = "nest"


def applicable(node_id: str, app: str, app_marks: dict | None = None) -> bool:
    """Can `node_id` ever witness `app`'s column? (§ Cell semantics, *for that app*)

    `app_marks` is `{node id: apps it is marked for}` — `features_scan.scan_tree`'s
    `apps` field. A test marked for some apps runs only on those, which is the very
    rule the run's app axis deselects by; an UNMARKED test is parametrized over
    whichever apps the run selected, so it can speak for any column. An ABSENT entry
    is the unmarked case, not a special one: a caller with no marker knowledge asks
    the same question the tree answers for an unmarked test, and gets the same yes.

    A kids column is read through its parent (`parent_app`): a test marked for the
    parent can drive the kids build, which is that app's UI with planes removed, and a
    witness of an excised plane is kept off the kids column by the page's absence,
    never by a mark.
    """
    marks = (app_marks or {}).get(node_id)
    return not marks or parent_app(app) in marks


def scan_app_marks(repo: Path = REPO) -> dict:
    """`{node id: (apps)}` for the whole e2e tree — the input `applicable` wants.

    Kept here rather than in the render so the ledger writer and the audit reach it
    the same way. Parse-only and ~2 s over the real tree, the same `ast` scan the
    lint already pays (§ Retiring the hand matrix: a merge-gated artifact is derived
    by a parse, never by a pytest collection, which differs per machine).
    """
    return scan_marks(repo)[0]


def scan_marks(repo: Path = REPO) -> tuple:
    """`({node id: (apps)}, {node id: (surface keys)}, {node id: (host platforms)})`
    from ONE scan of the e2e tree — the app markers `applicable` wants, the
    `requires_surface` markers the test → surface map's first reader wants (§ Cell
    semantics, *Not built is declared*), and the `host_platform` markers
    `witness_platforms` wants (§ The ledger, *Stale records*, ruling (i)). One `ast`
    pass for all three, so the render pays the ~2 s once."""
    import sys
    scripts = str(Path(__file__).resolve().parent)
    if scripts not in sys.path:
        sys.path.insert(0, scripts)
    import features_scan
    scanned = features_scan.scan_tree(root=repo / "tests" / "e2e-unified", repo=repo)
    return ({node_id: facts.apps for node_id, facts in scanned.items() if facts.apps},
            {node_id: facts.surfaces for node_id, facts in scanned.items() if facts.surfaces},
            {node_id: facts.platforms for node_id, facts in scanned.items()
             if facts.platforms is not None})

#: The version-skew pin the release table derives (§ Tag gate and release table → *The
#: pin is derived from it*). Repo-relative because it lives OUTSIDE the catalog: it is
#: test-infrastructure data whose semantics are owned by version-compatibility.md
#: § Dimension 6, and all this module does is stop it being a second hand-kept copy of
#: a fact the table already holds.
PIN_REL = "tests/e2e-unified/pinned-previous-build.toml"

#: The immutable per-commit tag the release pipeline pushes (image-pipeline.md § Image
#: tags & channels). Only a fallback: a release row records the exact image string the
#: candidate run drove, and that is preferred over re-deriving one.
NEST_IMAGE_REPO = "ghcr.io/faunasocial/nest"

#: Section order in the rendered matrix. A page may mint a new section — a section is
#: a heading, not a concept (§ The page) — and an unknown one sorts after these, by
#: name, so minting one is never a lint error. The first eight are the seed matrix's;
#: "your nest" was minted by the page-writing pass for the capabilities a nest
#: delivers with nothing to choose.
SECTION_ORDER = (
    "getting in",
    "everyday",
    "your data and devices",
    "mail, calendar and contacts",
    "bridges and other networks",
    "money",
    "family and personalization",
    "admin area",
    "your nest",
)

#: § Cell semantics. The glyphs are ASCII-ish marks on purpose: coloured-circle emoji
#: are the internal review log's severity vocabulary and the publish gate counts them
#: in every shipped tree, so a public matrix using them would be indistinguishable
#: from that vocabulary. The *state* is the word — that is what `matrix.json` carries
#: and what the site colours from.
MARKS = {
    "full": "✅",      # white heavy check mark
    "partial": "⚠",   # warning sign
    "failing": "❌",   # cross mark
    "unbuilt": "🚧",  # construction sign — held short by build work, not by a failure
    "none": "",
    "absent": "—",    # em dash
}

#: Outcomes that count as a witness having succeeded (§ Cell semantics).
PASSING = frozenset({"passed", "xpassed"})


def is_unbuilt(record: dict, app: str) -> bool:
    """Is this record *unbuilt* for `app`'s column (§ Cell semantics, the *unbuilt*
    row)? The recorded route: its outcome is a skip of class `unbuilt`. A record the
    registry reads otherwise — stale (its `surface` is built on the column) or held by
    a `harness` surface — never reaches here: `live_records` drops it first, so it
    reads as unrun (`Declared.record_reading`)."""
    return record.get("outcome") == "skipped" and record.get("skip_class") == "unbuilt"


def is_real_red(record: dict) -> bool:
    """A non-passing record that is a failure, not a declared skip: `failed`, `error`,
    `xfailed`, or a skip of no class (§ Cell semantics). It is what turns an
    *unbuilt* column ⚠ — the failure is the more urgent signal."""
    outcome = record.get("outcome")
    if outcome == "skipped":
        return not record.get("skip_class")
    return outcome not in PASSING

BEGIN = "<!-- features-render:begin -->"
END = "<!-- features-render:end -->"

_STAMP_RE = re.compile(r"^Stamped\s+(\d{4}-\d{2}-\d{2})\s+at\s+([0-9a-f]{7,40})\.?\s*$")
_OUTCOME_RE = re.compile(r"^(\d+)\.\s+\[(app|nest)\]\s+(.*)$")

#: A witness whose test file does not ship (§ The coverage contract, *Maintainer-only
#: witnesses*, 2026-09-09). The node id sits inside the provenance span the publish
#: transform excises (the U+27E6 / U+27E7 bracket pair, docs/goal/README.md § Writing
#: protocol), so the internal tree keeps a citation the lint, the ledger and the render
#: all read, and the public page carries only the mark — never a pointer to a test the
#: reader does not have. The bare mark (no span) is what the SHIPPED page reads as, and
#: it parses too: a public reader's tooling must never choke on the public form.
#:
#: Built from code points, never spelled: this module SHIPS, and the transform's
#: excision pass rewrites any literal pair it finds in a shipped file — measured
#: 2026-09-09, a literal `"<begin>", "<end>"` here shipped as an empty string and
#: broke the public tree's catalog tooling at import.
MAINTAINER_ONLY = "(maintainer-only)"
SPAN_BEGIN, SPAN_END = chr(0x27E6), chr(0x27E7)
_TEST_RE = re.compile(
    r"^\s+-\s+(?:`(?P<id>[^`]+)`|\((?P<none>none)\)"
    r"|\(maintainer-only\)(?:" + SPAN_BEGIN + r"\s*`(?P<mo_id>[^`]+)`\s*" + SPAN_END + r")?)\s*$")
_CITATION_RE = re.compile(r"\s—\s`([^`]+)`\s+§\s+(.+?)\s*$")


def maintainer_only_witness(node_id: str) -> str:
    """The contract/status spelling of a maintainer-only witness — the one place the
    shape is spelled, so the render and a hand-written contract line cannot drift."""
    return f"{MAINTAINER_ONLY}{SPAN_BEGIN} `{node_id}`{SPAN_END}"


class CatalogError(Exception):
    """A page the catalog cannot read — a hard stop, never a warning.

    The render's whole value is that nobody types a cell, so a page it cannot parse
    must stop it rather than quietly contribute an empty row that reads like "not
    witnessed yet".
    """


# ── the model ───────────────────────────────────────────────────────────────
@dataclass(frozen=True)
class Outcome:
    """One numbered user-observable outcome of a coverage contract."""

    number: int
    surface: str          # "app" | "nest"
    text: str             # the outcome sentence, user voice
    citation: str         # "docs/goal/ui/feed.md § Reading"
    tests: tuple          # node id prefixes; empty is the honest `(none)` mapping
    line: int
    #: The subset of `tests` cited as maintainer-only — their files do not ship, so
    #: the public page carries the mark and not the id (§ The coverage contract,
    #: *Maintainer-only witnesses*). Always a subset of `tests`: such a witness still
    #: counts for fullness, the ledger and the render exactly like any other — what
    #: differs is only how the citation is SPELLED where it ships.
    maintainer_only: frozenset = frozenset()


@dataclass
class Page:
    path: Path
    slug: str
    title: str
    section: str
    goal: str
    guide: str
    absences: dict
    what: str
    stamp_date: str
    stamp_commit: str
    outcomes: list = field(default_factory=list)
    #: `{app: {outcome number: citation}}` — the `absences` entries keyed
    #: `<app> (outcome N)`: the column is excused from THOSE outcomes by design and
    #: owes the rest (§ The page, `absences`). `absences` above stays the page-level
    #: map, whose column renders `—`; a column here renders a real cell.
    outcome_absences: dict = field(default_factory=dict)
    #: The ui.yaml `pages:` keys the feature's contract tests drive (front-matter
    #: `ui-pages`, optional): what lets a ui.yaml edit name the pages it touches
    #: (the touched-set selector, delayed-testing.md § What a session runs).
    ui_pages: tuple = ()
    #: The kids-column absences carried from the parent (`_inherit_kids_absences`) —
    #: `android-kids` for a page-level one, `android-kids (outcome N)` per outcome.
    #: Present in `absences` / `outcome_absences` like any other; kept apart so the
    #: contract gate asks no second record for what the parent's move already carries.
    inherited_absences: frozenset = frozenset()

    def absent_from(self, app: str, outcome: Outcome) -> bool:
        """Is `app` excused from this one outcome by a per-outcome absence?"""
        return outcome.number in self.outcome_absences.get(app, {})

    @property
    def full(self) -> bool:
        """§ The coverage contract: fullness is derived, never a mark someone types."""
        return bool(self.outcomes) and all(o.tests for o in self.outcomes)

    def full_for(self, app: str, app_marks: dict | None = None) -> bool:
        """§ The coverage contract's fullness, read PER COLUMN (§ Cell semantics).

        An `[app]` outcome whose only witnesses are other apps' tests maps to nothing
        *here*, which is precisely what `(none)` means — so the column is partial,
        exactly as a literal `(none)` would make it. Reading fullness per page instead
        would let a column show ✅ on the strength of a sibling platform's witness.

        `[nest]` outcomes are NOT narrowed: a nest record counts for every column
        (§ The two surfaces), so a nest witness that happens to run on one machine
        only still witnesses this column — it either ran or it did not, and that is
        the ledger's question, not this one's.
        """
        return bool(self.outcomes) and not self.blocking(app, app_marks)

    def blocking(self, app: str, app_marks: dict | None = None) -> list:
        """The outcomes that keep this column short of full — `full_for`'s *why*.

        Same rule, one definition: `blocking` is empty exactly when `full_for` is
        true (for a page with any outcomes at all). Yes/no is what the render needs;
        every reader's next question is *which outcome*, and deriving that by hand
        from a page and a marker table is what several triage passes did one page at
        a time before this existed.

        A blocked `[app]` outcome is one no test that can speak for this column
        witnesses — a literal `(none)`, or citations every one of which is marked for
        other apps. A blocked `[nest]` outcome is `(none)`, and blocks every column
        alike, because a nest record is never narrowed. An outcome this
        column is declared absent from never blocks it: it is not the column's to owe.
        """
        return [o for o in self.outcomes
                if not self.absent_from(app, o)
                and not (bool(o.tests) if o.surface == NEST
                         else any(applicable(t, app, app_marks) for t in o.tests))]

    def completable(self, app_marks: dict | None = None) -> list:
        """The columns some run could ever complete — `full_for` over every app.

        The parse-only half of the cell question. `compute_cell` asks what a run has
        RECORDED; this asks what a run could ever record, which is a property of the
        contract alone. Empty means the page can be stamped on no column, on no
        machine, in no nest mode — the blank cell is then a contract fact and running
        more tests will not move it.

        A declared-absent column is never completable and never short: it renders `—`
        by design (§ Cell semantics), so it is not a column any run completes, and
        counting it would read a designed absence as coverage backlog.
        """
        return [a for a in APPS if a not in self.absences and self.full_for(a, app_marks)]

    @property
    def nest_only(self) -> bool:
        """Every outcome on the nest surface — renders as one cell spanning the row."""
        return bool(self.outcomes) and all(o.surface == NEST for o in self.outcomes)

    def surface_tests(self, surface: str, app: str | None = None,
                      app_marks: dict | None = None) -> list:
        """The witnesses on one surface; with `app`, only those that column can have.

        The unfiltered form is still the right question for the `nest` surface and
        for the lint, which asks about the contract itself rather than about a column.
        With `app`, an outcome that column is declared absent from contributes nothing:
        its witnesses are not part of the column's set, so a record the column can
        never have does not hold its cell blank.
        """
        seen, out = set(), []
        for outcome in self.outcomes:
            if outcome.surface != surface:
                continue
            if app is not None and self.absent_from(app, outcome):
                continue
            for test in outcome.tests:
                if test in seen:
                    continue
                if app is not None and not applicable(test, app, app_marks):
                    continue
                seen.add(test)
                out.append(test)
        return out


@dataclass(frozen=True)
class Blocker:
    """One outcome keeping one column short of full, and who already witnesses it.

    `Page.blocking` read from the column's side. The extra field is `witnesses` — the
    columns whose applicable citations DO witness the outcome — and it is what makes
    the backlog routable rather than merely countable: a gap some sibling column has
    already closed is a lift with a named reference implementation, while a gap no
    column witnesses is an unwritten test nobody can lift.
    """

    page: "Page"
    outcome: Outcome
    witnesses: tuple      # columns that can witness it; `()` is the honest `(none)`

    @property
    def owed_here(self) -> bool:
        """Is closing this THIS column's debt? True exactly when a sibling column
        already has the witness (§ Cell semantics: an `[app]` outcome with no
        applicable witness is `(none)` *for that column*, so a column short of an
        outcome its siblings have is short of a lift).

        A `[nest]` outcome is never `owed_here`: a nest record counts for every column
        (§ The two surfaces), so it is never narrowed and never lifted from one column
        to another — an unwitnessed one blocks every column alike and belongs to whoever
        owns the feature, exactly as a literal `(none)` does.
        """
        return bool(self.witnesses)


def witnessing_columns(outcome: Outcome, app_marks: dict | None = None) -> tuple:
    """The columns an outcome's citations can ever speak for — `Page.blocking` seen
    from the other side, and the same `applicable` rule in both directions.

    A nest outcome speaks for every column or for none, so narrowing it per column
    would misreport it; it is reported as witnessing nothing, which is what
    `Blocker.owed_here` needs and what the parity render already prints as `[nest]`.
    """
    if not outcome.tests or outcome.surface == NEST:
        return ()
    return tuple(a for a in APPS
                 if any(applicable(t, a, app_marks) for t in outcome.tests))


def column_backlog(pages: list, app: str, app_marks: dict | None = None) -> list:
    """Every outcome keeping `app`'s column short, across the catalog — the parity
    view transposed from page-major to column-major.

    § Cell semantics answers "which columns is THIS PAGE short on", which is what a
    reader of one page wants. Routing that backlog asks the transpose, because a
    trickle-down PASS and the queue that receives its capture are both **per app**
    (`testing.md` § Default app and nest mode: batched per-app trickle-down passes,
    never per-feature × 6).
    Transposing 105 pages by hand is exactly the derivation this module exists to stop.

    A declared-absent column has no backlog: `—` is neither completable nor short, so
    listing its outcomes as work reads a designed absence as coverage debt — the same
    rule `completable` follows. Empty is therefore exactly `full_for`, page by page.
    """
    out = []
    for page in pages:
        if app in page.absences or not page.outcomes:
            continue
        for outcome in page.blocking(app, app_marks):
            out.append(Blocker(page=page, outcome=outcome,
                               witnesses=witnessing_columns(outcome, app_marks)))
    return out


@dataclass(frozen=True)
class Gap:
    """One missing witness, and every column short of it — `Blocker` deduplicated.

    A column's backlog is the right unit for *doing* the work and the wrong one for
    *routing* it: an outcome witnessed on a single app appears in six columns'
    backlogs, and sending each to its app queue would mint the same piece of work six
    times — the per-feature × 6 shape `testing.md` § Default app and nest mode refuses,
    arriving through a count instead of through a plan. `reach` is what tells the two apart.
    """

    page: "Page"
    outcome: Outcome
    witnesses: tuple      # columns that can witness it (never empty — see `gap_reach`)
    short: tuple          # columns owed it; declared absences are in neither tuple

    @property
    def reach(self) -> int:
        """How many columns one missing witness blocks. A high reach is a **cross-app**
        gap belonging to one row in the cross-app queue; a reach of 1 is a genuine
        per-app straggler and is what a batched trickle-down pass should receive."""
        return len(self.short)


def gap_reach(pages: list, app_marks: dict | None = None) -> dict:
    """`{(slug, outcome number): Gap}` for every LIFTABLE gap in the catalog.

    `column_backlog` transposed once more — from column-major to gap-major, which is
    the unit routing actually moves. Liftable is the whole selection rule: an outcome
    no column witnesses (a literal `(none)`, or an unwitnessed `[nest]` outcome) is
    excluded rather than ranked as reaching all seven, because ranking it would put
    the widest unwritten test at the top of a list only app queues read, which is
    precisely where it does not belong — it is the feature area's, not theirs.
    """
    gaps = {}
    for app in APPS:
        for blocker in column_backlog(pages, app, app_marks):
            if not blocker.owed_here:
                continue
            key = (blocker.page.slug, blocker.outcome.number)
            gaps.setdefault(key, {"blocker": blocker, "short": []})["short"].append(app)
    return {key: Gap(page=v["blocker"].page, outcome=v["blocker"].outcome,
                     witnesses=v["blocker"].witnesses, short=tuple(v["short"]))
            for key, v in gaps.items()}


def column_trickle_down(pages: list, app: str, max_reach: int,
                        app_marks: dict | None = None) -> list:
    """The gaps `app` owes whose reach is at most `max_reach` — one app queue's batch.

    `column_backlog` is everything keeping the column short and `gap_reach` is every
    gap in the catalog; a batched per-app trickle-down pass wants their intersection
    under a cap, because a gap above the cap belongs to ONE cross-app row and doing it
    from an app queue mints it once per short column. Catalog order, so a row that
    derives its list from this drains top to bottom as cells move.
    """
    return [gap for gap in gap_reach(pages, app_marks).values()
            if app in gap.short and gap.reach <= max_reach]


@dataclass(frozen=True)
class Cell:
    state: str            # full | partial | failing | unbuilt | none | absent
    stamp: str
    date: str
    #: The stamp's run had uncommitted changes, so its commit is a lower bound on the
    #: code it saw (feature-catalog.md § The ledger, *Dirty trees*). Carried as data
    #: so no reader has to parse `.dirty` out of the stamp string.
    dirty: bool = False

    @property
    def mark(self) -> str:
        return MARKS[self.state]


# ── parsing ─────────────────────────────────────────────────────────────────
def _split_front_matter(text: str, path: Path) -> tuple:
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        raise CatalogError(f"{path.name}: no front-matter fence on line 1")
    try:
        close = lines.index("---", 1)
    except ValueError:
        raise CatalogError(f"{path.name}: front-matter is never closed") from None
    return lines[1:close], lines[close + 1:]


def _parse_front_matter(raw: list, path: Path) -> dict:
    """The fixed grammar: `key: value` scalars plus one nested `key:` / `  sub: value`.

    Deliberately not YAML. The shape is closed (§ The page's front-matter table), and
    a closed grammar read by hand is what keeps the lint stdlib-only.
    """
    data: dict = {}
    current = None
    for offset, line in enumerate(raw, start=2):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if line.startswith(("  ", "\t")):
            if current is None:
                raise CatalogError(f"{path.name}:{offset}: indented line under no key")
            key, sep, value = line.strip().partition(":")
            if not sep:
                raise CatalogError(f"{path.name}:{offset}: expected `key: value`")
            data[current][key.strip()] = value.strip().strip('"')
            continue
        key, sep, value = line.partition(":")
        if not sep:
            raise CatalogError(f"{path.name}:{offset}: expected `key: value`")
        key, value = key.strip(), value.strip().strip('"')
        if value:
            data[key] = value
            current = None
        else:
            data[key] = {}
            current = key
    return data


def _sections(body: list) -> dict:
    """`## Heading` -> its lines, with the line number each heading sat on."""
    out: dict = {}
    name, start, buf = None, 0, []
    for offset, line in enumerate(body, start=1):
        if line.startswith("## "):
            if name is not None:
                out[name] = (start, buf)
            name, start, buf = line[3:].strip(), offset, []
        elif name is not None:
            buf.append(line)
    if name is not None:
        out[name] = (start, buf)
    return out


def _reject_stray_outcome_lines(sections: dict, path: Path) -> None:
    """An outcome or witness line outside `## Coverage contract` parses as ordinary
    prose in whatever section holds it and is silently dropped — never counted,
    never a lint error (feature-catalog.md § The coverage contract)."""
    for name, (start, lines) in sections.items():
        if name == "Coverage contract":
            continue
        for offset, line in enumerate(lines, start=start + 1):
            if _OUTCOME_RE.match(line.strip()) or _TEST_RE.match(line):
                raise CatalogError(
                    f"{path.name}:{offset}: outcome/witness line inside `## {name}`, "
                    "not `## Coverage contract` — it is silently dropped there and "
                    "never counted (feature-catalog.md § The coverage contract)")


def _parse_contract(lines: list, base_line: int, path: Path) -> tuple:
    stamp_date = stamp_commit = ""
    outcomes: list = []
    pending: Outcome | None = None
    tests: list = []
    maintainer_only: set = set()

    def flush():
        nonlocal pending, tests, maintainer_only
        if pending is not None:
            outcomes.append(Outcome(
                number=pending.number, surface=pending.surface, text=pending.text,
                citation=pending.citation, tests=tuple(tests), line=pending.line,
                maintainer_only=frozenset(maintainer_only)))
        pending, tests, maintainer_only = None, [], set()

    for offset, line in enumerate(lines, start=base_line + 1):
        stripped = line.strip()
        if not stripped:
            continue
        stamp = _STAMP_RE.match(stripped)
        if stamp:
            stamp_date, stamp_commit = stamp.group(1), stamp.group(2)
            continue
        head = _OUTCOME_RE.match(stripped)
        if head:
            flush()
            rest = head.group(3)
            citation = _CITATION_RE.search(rest)
            if not citation:
                raise CatalogError(
                    f"{path.name}:{offset}: outcome {head.group(1)} carries no goal-doc "
                    "citation — the grammar is `… — `<doc>` § <section>` "
                    "(feature-catalog.md § The coverage contract)")
            pending = Outcome(
                number=int(head.group(1)), surface=head.group(2),
                text=rest[:citation.start()].strip(),
                citation=f"{citation.group(1)} § {citation.group(2)}",
                tests=(), line=offset)
            continue
        test = _TEST_RE.match(line)
        if test:
            if pending is None:
                raise CatalogError(f"{path.name}:{offset}: a test id under no outcome")
            if test.group("id"):
                tests.append(test.group("id"))
            elif test.group("mo_id"):
                # The internal spelling: the id is inside the excised span, so it is
                # read here and absent from the shipped page. A bare `(maintainer-
                # only)` (the shipped spelling) is a legal line naming no id.
                tests.append(test.group("mo_id"))
                maintainer_only.add(test.group("mo_id"))
            continue
        raise CatalogError(f"{path.name}:{offset}: unreadable contract line: {stripped!r}")
    flush()
    return stamp_date, stamp_commit, outcomes


def parse_page(path: Path) -> Page:
    text = path.read_text(encoding="utf-8")
    raw_fm, body = _split_front_matter(text, path)
    front = _parse_front_matter(raw_fm, path)

    stem = path.stem
    slug = front.get("slug")
    if slug != stem:
        raise CatalogError(
            f"{path.name}: front-matter slug {slug!r} does not equal the filename "
            f"stem {stem!r} — the slug is the identity (feature-catalog.md "
            "§ The page)")
    for required in ("title", "section", "goal", "guide"):
        if not front.get(required):
            raise CatalogError(f"{path.name}: front-matter is missing `{required}`")
    raw_absences = front.get("absences") or {}
    if not isinstance(raw_absences, dict):
        raise CatalogError(f"{path.name}: `absences` must be a map of app -> citation")
    absences, scoped = _split_absences(raw_absences, path)

    sections = _sections(body)
    for required in ("What a user gets", "Coverage contract", "Status"):
        if required not in sections:
            raise CatalogError(f"{path.name}: no `## {required}` section")
    _reject_stray_outcome_lines(sections, path)
    contract_line, contract_lines = sections["Coverage contract"]
    stamp_date, stamp_commit, outcomes = _parse_contract(contract_lines, contract_line, path)
    numbers = [o.number for o in outcomes]
    if numbers != list(range(1, len(numbers) + 1)):
        raise CatalogError(f"{path.name}: contract outcomes are not numbered 1..n: {numbers}")

    outcome_absences = _check_outcome_absences(scoped, absences, outcomes, path)
    inherited = _inherit_kids_absences(
        absences, outcome_absences, path, _parse_kids_owed(front.get("kids-owed"), path))
    return Page(
        path=path, slug=slug, title=front["title"], section=front["section"].lower(),
        goal=front["goal"], guide=front["guide"], absences=absences,
        what="\n".join(sections["What a user gets"][1]).strip(),
        stamp_date=stamp_date, stamp_commit=stamp_commit, outcomes=outcomes,
        outcome_absences=outcome_absences,
        ui_pages=_parse_ui_pages(front.get("ui-pages"), path),
        inherited_absences=inherited)


def _parse_kids_owed(raw, path: Path) -> frozenset:
    """Front-matter `kids-owed: android-kids, ios-kids` -> the kids columns the page is
    OWED on although the parent column is absent (§ Cell semantics: the `kids-app` page,
    whose outcomes are false on every main build by design and true on the kids build)."""
    if raw is None:
        return frozenset()
    if not isinstance(raw, str):
        raise CatalogError(f"{path.name}: `kids-owed` is a comma-separated list of kids "
                           "columns, not a map")
    owed = frozenset(k.strip() for k in raw.split(","))
    bad = sorted(owed - set(COLUMN_PARENT))
    if bad:
        raise CatalogError(f"{path.name}: `kids-owed` names {bad}, which are not kids "
                           f"columns ({', '.join(COLUMN_PARENT)})")
    return owed


def _inherit_kids_absences(absences: dict, outcome_absences: dict, path: Path,
                           owed: frozenset = frozenset()) -> frozenset:
    """Carry each parent's absences onto its kids column, IN PLACE, and return the kids
    keys that came by inheritance (§ Cell semantics, *A second store identity …*: the
    kids build is its parent's UI with planes removed, so what the parent lacks by
    design the kids build lacks too, under the parent's citation and approval). A
    page-level parent absence makes the kids column absent from the page; a parent's
    per-outcome absence excuses the kids column from that outcome. A kids column's own
    entries stand beside them (its own D-K5 absences); a kids per-outcome entry on a
    page its parent is absent from is refused — the column is absent with it."""
    inherited = set()
    for kids, parent in COLUMN_PARENT.items():
        if kids in owed:
            if kids in absences or kids in outcome_absences:
                raise CatalogError(
                    f"{path.name}: `kids-owed` names {kids}, which `absences` also excuses")
            continue
        if parent in absences and kids not in absences:
            if kids in outcome_absences:
                raise CatalogError(
                    f"{path.name}: `absences` excuses {kids} from some outcomes, but its "
                    f"parent {parent} is absent from the whole page, and {kids} with it "
                    "(feature-catalog.md § Cell semantics)")
            absences[kids] = absences[parent]
            inherited.add(kids)
            continue
        if kids in absences:
            continue
        for number, citation in (outcome_absences.get(parent) or {}).items():
            own = outcome_absences.setdefault(kids, {})
            if number not in own:
                own[number] = citation
                inherited.add(f"{kids} (outcome {number})")
        if kids in outcome_absences:
            outcome_absences[kids] = dict(sorted(outcome_absences[kids].items()))
    return frozenset(inherited)


def _parse_ui_pages(raw, path: Path) -> tuple:
    """Front-matter `ui-pages: feed, settings` -> `("feed", "settings")` (§ The page)."""
    if raw is None:
        return ()
    if not isinstance(raw, str):
        raise CatalogError(f"{path.name}: `ui-pages` is a comma-separated list of "
                           "ui.yaml page keys, not a map")
    keys = [k.strip() for k in raw.split(",")]
    if not all(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.-]*", k) for k in keys):
        raise CatalogError(f"{path.name}: `ui-pages` holds a malformed key: {raw!r}")
    return tuple(keys)


def ui_page_ranges(text: str) -> dict:
    """`{page key: (first line, last line)}` of ui.yaml's `pages:` section, 1-based.

    The same indentation scan `scripts/ui-show.py` addresses `pages.<key>` by: a
    two-space key under the top-level `pages:`. A page's last line is the line before
    the next key, trailing blank and comment lines excluded.
    """
    ranges: dict = {}
    in_pages, key, last = False, None, 0
    for number, line in enumerate(text.splitlines(), start=1):
        top = re.match(r"^([A-Za-z_][A-Za-z0-9_]*):", line)
        if top or (key and re.match(r"^  [A-Za-z_][A-Za-z0-9_.-]*:", line)):
            if key:
                ranges[key] = (ranges[key][0], last)
                key = None
            if top:
                in_pages = top.group(1) == "pages"
                continue
        if not in_pages:
            continue
        child = re.match(r"^  ([A-Za-z_][A-Za-z0-9_.-]*):", line)
        if child:
            key, last = child.group(1), number
            ranges[key] = (number, number)
        elif key and line.strip() and not line.lstrip().startswith("#"):
            last = number
    if key:
        ranges[key] = (ranges[key][0], last)
    return ranges


#: A per-outcome `absences` key: `web (outcome 4)`, `ios (outcomes 4, 5)`.
_OUTCOME_ABSENCE_RE = re.compile(
    r"^(?P<app>[a-z]+(?:-[a-z]+)*)\s+\(outcomes?\s+(?P<nums>\d+(?:\s*,\s*\d+)*)\)$")


def _split_absences(raw: dict, path: Path) -> tuple:
    """`absences` -> (page-level `{app: citation}`, `[(app, number, citation)]`).

    One front-matter map carries both forms (§ The page, `absences`), so every
    absence a page declares is still read in one place: a bare app key is the
    feature absent on that column, an `<app> (outcome N)` key is the column excused
    from those outcomes alone. The scoped entries are checked against the contract
    once it is parsed (`_check_outcome_absences`).
    """
    page_level, scoped = {}, []
    for key, citation in raw.items():
        if key in APPS:
            page_level[key] = citation
            continue
        match = _OUTCOME_ABSENCE_RE.match(key)
        if match is None:
            if re.fullmatch(r"[a-z]+(?:-[a-z]+)*", key):
                raise CatalogError(f"{path.name}: `absences` names unknown app(s): {key}")
            raise CatalogError(
                f"{path.name}: unreadable `absences` key {key!r} — an entry is `<app>` "
                "or `<app> (outcome N)` / `<app> (outcomes N, M)` (feature-catalog.md "
                "§ The page)")
        app = match.group("app")
        if app not in APPS:
            raise CatalogError(f"{path.name}: `absences` names unknown app(s): {app}")
        for number in match.group("nums").split(","):
            scoped.append((app, int(number), citation))
    return page_level, scoped


def _check_outcome_absences(scoped: list, absences: dict, outcomes: list, path: Path) -> dict:
    """The per-outcome entries a contract can honour, as `{app: {number: citation}}`.

    Refused: an outcome the contract does not have; a `[nest]` outcome (a nest
    record counts for every column, § The two surfaces, so no column can be excused
    from one); a column already absent from the whole page; one (app, outcome) named
    twice; and a column excused from EVERY outcome, which is the page-level entry
    spelled the long way round — the feature is absent there, and the page says so
    once.
    """
    by_number = {o.number: o for o in outcomes}
    out: dict = {}
    for app, number, citation in scoped:
        outcome = by_number.get(number)
        if outcome is None:
            raise CatalogError(
                f"{path.name}: `absences` excuses {app} from outcome {number}, but the "
                f"contract has no outcome {number}")
        if outcome.surface == NEST:
            raise CatalogError(
                f"{path.name}: `absences` excuses {app} from outcome {number}, a [nest] "
                "outcome — a nest record counts for every column, so no column can be "
                "excused from one (feature-catalog.md § The two surfaces)")
        if app in absences:
            raise CatalogError(
                f"{path.name}: `absences` excuses {app} from outcome {number}, but {app} "
                "is already declared absent from the whole page")
        if number in out.get(app, {}):
            raise CatalogError(
                f"{path.name}: `absences` names {app} (outcome {number}) more than once")
        out.setdefault(app, {})[number] = citation
    for app, numbers in out.items():
        if outcomes and set(numbers) == set(by_number):
            raise CatalogError(
                f"{path.name}: `absences` excuses {app} from every outcome — the feature "
                f"is absent there: declare `{app}` page-level instead (feature-catalog.md "
                "§ The page)")
    return {app: dict(sorted(numbers.items())) for app, numbers in sorted(out.items())}


def load_pages(features: Path = FEATURES) -> list:
    """Every `docs/features/*.md` page, sorted by section order then title."""
    if not features.exists():
        return []
    pages = [parse_page(p) for p in sorted(features.glob("*.md"))
             if p.name not in ("MATRIX.md", "README.md")]
    return sorted(pages, key=lambda p: (_section_rank(p.section), p.title.lower()))


def _section_rank(section: str) -> tuple:
    try:
        return (0, SECTION_ORDER.index(section))
    except ValueError:
        return (1, 0)


def load_ledger(features: Path = FEATURES, *, last_green: bool = True,
                repo: Path = REPO) -> dict:
    """`{app: {slug: {"cell": {...}, "tests": {node id: {platform: record}}}}}`,
    missing files absent (§ The ledger, *Platform keying*: one test can carry more
    than one platform's outcome, so the render folds across them — never reads a
    bare record straight off `tests[node id]`).

    With `last_green` (the default) each record is the NEWER of the ledger's and the
    published last-green snapshot's for its key (§ Cell semantics, *Two sources*;
    `overlay_last_green`), so every reader of a cell — the render, the lint, the gap
    view — reads one observation. `last_green=False` is the files as written: what
    the prune rewrites and what lint rule 6 judges, since only a file can be pruned."""
    out = _load_ledger_files(features)
    if last_green:
        overlay_last_green(out, features, repo=repo)
    return out


def _load_ledger_files(features: Path) -> dict:
    out: dict = {}
    ledger_dir = features / "ledger"
    if not ledger_dir.is_dir():
        # A hard stop, never an empty ledger (2026-09-09). The ledger is the render's
        # INPUT and does not ship (§ The ledger), so the one checkout that lacks it is
        # a public clone — and an empty ledger there is not "nothing recorded", it is
        # a render that would blank every committed `## Status` block and a rule-5
        # lint that would report every page stale (measured: 100 of the 107 findings
        # a public `just features-lint` used to print). One precise refusal instead.
        raise CatalogError(
            f"{ledger_dir} does not exist — the catalog's ledger is the render's input "
            "and does not ship, so the render and the lint run only in the "
            "maintainers' tree; this checkout carries the committed render "
            "(feature-catalog.md § The ledger)")
    for path in sorted(ledger_dir.glob("*.json")):
        if path.stem == "releases":
            continue
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as exc:
            raise CatalogError(f"ledger/{path.name}: {exc}") from exc
        out[path.stem] = data.get("features") or {}
    return out


def load_releases(features: Path = FEATURES) -> list:
    """The release table's rows. Absent until step 4 mints the first candidate run."""
    path = features / "ledger" / "releases.json"
    if not path.exists():
        return []
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise CatalogError(f"ledger/releases.json: {exc}") from exc
    return data.get("releases") or []


# ── the surface registry (e2e-conventions.md convention 7) ──────────────────
#: The registry file. Convention 7 owns it (*The surface registry*); this is its one
#: reader — the run-time gate (`helpers/app_surface.py::require_surface`) and the
#: conftest marker call it too, so the matrix and the run cannot read it apart.
SURFACES = REPO / "tests" / "e2e-unified" / "surfaces.toml"
#: `product` — the APP lacks the surface (the matrix's *not built*); `harness` — the
#: e2e driver lacks it for that app (read as unrun).
SURFACE_KINDS = ("product", "harness")
_SURFACE_KEY = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


@dataclass(frozen=True)
class Surface:
    key: str
    kind: str
    built: frozenset
    detail: str = ""
    tracked: str = ""


def load_surface_registry(path: Path = SURFACES) -> dict:
    """`{key: Surface}` from `surfaces.toml` — stdlib `tomllib`, never an import of
    the test tree. An absent file is an empty registry; a malformed one is a
    `CatalogError`, never a partial read: a gate that silently lost an entry would
    turn an unbuilt skip into an unknown-key error mid-run, or worse, a key into
    *built everywhere*."""
    import tomllib

    if not path.exists():
        return {}
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as exc:
        raise CatalogError(f"{path.name}: {exc}") from exc
    registry = {}
    for key, entry in data.items():
        where = f"{path.name} [{key}]"
        if not _SURFACE_KEY.match(key):
            raise CatalogError(f"{where}: a surface key is kebab-case")
        if not isinstance(entry, dict):
            raise CatalogError(f"{where}: an entry is a table")
        unknown = set(entry) - {"kind", "built", "detail", "tracked"}
        if unknown:
            raise CatalogError(f"{where}: unknown field(s) {', '.join(sorted(unknown))}")
        kind = entry.get("kind")
        if kind not in SURFACE_KINDS:
            raise CatalogError(f"{where}: kind must be one of {', '.join(SURFACE_KINDS)}")
        built = entry.get("built")
        if not isinstance(built, list) or not all(isinstance(a, str) for a in built):
            raise CatalogError(f"{where}: built is a list of app names")
        strays = set(built) - set(APPS)
        if strays:
            raise CatalogError(f"{where}: built names no app {', '.join(sorted(strays))}")
        for text in ("detail", "tracked"):
            if not isinstance(entry.get(text, ""), str):
                raise CatalogError(f"{where}: {text} is a string")
        if kind == "harness":
            # A harness surface is the e2e DRIVER's, and a kids column is driven by its
            # parent's driver under the kids identity (§ Cell semantics), so it has
            # what the parent's driver has. A product surface is the app's, which the
            # kids build may excise: listed for a kids column by hand, or not at all.
            built = list(built) + [kids for kids, parent in COLUMN_PARENT.items()
                                   if parent in built and kids not in built]
        registry[key] = Surface(key=key, kind=kind, built=frozenset(built),
                                detail=entry.get("detail", ""),
                                tracked=entry.get("tracked", ""))
    return registry


def surfaces_path(features: Path) -> Path:
    """The registry beside a catalog — `tests/e2e-unified/surfaces.toml` of the tree
    that holds `docs/features/`, so a fixture world reads its own (or none)."""
    return features.parent.parent / "tests" / "e2e-unified" / "surfaces.toml"


def _record_keys(record: dict) -> list:
    keys = list(record.get("surfaces") or [])
    if record.get("surface"):
        keys.append(record["surface"])
    return keys


def test_surfaces(node_id: str, scan: dict, ledger: dict) -> frozenset:
    """The surface keys `node_id` requires (§ Cell semantics, *Not built is declared*:
    the test → surface map is the union of two readers) — its `requires_surface`
    markers (`scan`, `{node id: keys}`, the second half of `scan_marks`) and the
    `surfaces` every record of it carries, in every ledger file, under every feature,
    on every platform. A run on any app teaches the map what every column meets."""
    keys = set(scan.get(node_id) or ())
    for features in ledger.values():
        for entry in features.values():
            for record in ((entry.get("tests") or {}).get(node_id) or {}).values():
                keys.update(_record_keys(record))
    return frozenset(keys)


def surface_map(scan: dict, ledger: dict) -> dict:
    """`{node id: keys}` — `test_surfaces` for every test either reader names, in one
    pass over the ledger (the render asks it ~900 cells × their witnesses over)."""
    out = {node_id: set(keys) for node_id, keys in scan.items()}
    for features in ledger.values():
        for entry in features.values():
            for node_id, by_platform in (entry.get("tests") or {}).items():
                for record in by_platform.values():
                    keys = _record_keys(record)
                    if keys:
                        out.setdefault(node_id, set()).update(keys)
    return {node_id: frozenset(keys) for node_id, keys in out.items() if keys}


#: The first day every ledger record lists the gates its run reached — the writer
#: (`feature_ledger.record`) grew `surfaces` late on 2026-10-25 and omits it when the
#: run reached none, so from this date a record WITHOUT the field reached no gate
#: (§ Cell semantics, *Not built is declared*, the map's second reading).
SURFACES_RECORDED_SINCE = "2026-10-26"


def record_reach(record: dict) -> frozenset | None:
    """The surface keys `record`'s own run reached, or None where the record cannot
    say — written before the writer listed them, so its silence is no evidence."""
    if "surfaces" in record or str(record.get("date") or "") >= SURFACES_RECORDED_SINCE:
        return frozenset(_record_keys(record))
    return None


def borrowed_map(scan: dict, smap: dict) -> dict:
    """`{node id: keys}` — the keys `surface_map` holds for a test only because some
    record listed them, never its own `requires_surface` markers. A record that
    knows its own reach is judged without these (`Declared.requires`)."""
    out = {}
    for node_id, keys in smap.items():
        borrowed = keys - frozenset(scan.get(node_id) or ())
        if borrowed:
            out[node_id] = borrowed
    return out


@dataclass(frozen=True)
class Declared:
    """The declared route's inputs (§ Cell semantics, *Not built is declared*): the
    registry and the test → surface map. Every reading the cell, the cause view, the
    prune and the lint make of them is a method here, so the five cannot read the
    registry apart. Empty (no registry, no map) reads exactly as the recorded route
    alone — the catalog before the registry."""

    registry: dict = field(default_factory=dict)
    surfaces: dict = field(default_factory=dict)
    #: The part of `surfaces` only records taught (`borrowed_map`) — dropped when the
    #: record being judged knows its own reach, so a gate the test no longer calls
    #: cannot outlive its removal through another platform's older record.
    borrowed: dict = field(default_factory=dict)

    def requires(self, test: str, record: dict | None = None) -> frozenset:
        """The surface keys `test` requires — for a column with no record, the whole
        map (what a run elsewhere taught it to expect); for one record, the map minus
        every borrowed key that record's own run did not reach, when it can say."""
        keys = frozenset(self.surfaces.get(test, ()))
        reach = None if record is None else record_reach(record)
        if reach is None:
            return keys
        return keys - (frozenset(self.borrowed.get(test, ())) - reach)

    def unbuilt(self, test: str, app: str, kind: str = "product",
                record: dict | None = None) -> tuple:
        """The `kind` surfaces `test` requires (`requires`, read for `record` when one
        is given) that `app` has not built, sorted. A key the registry does not
        declare is rule 7's finding, never a declaration."""
        return tuple(sorted(
            k for k in self.requires(test, record)
            if (s := self.registry.get(k)) is not None and s.kind == kind
            and app not in s.built))

    def record_reading(self, record: dict, app: str) -> str | None:
        """How the registry reads a `skipped: unbuilt` record on `app`'s column:
        `"stale"` — its `surface` is built there now (unrun; rule 6 refuses it, the
        prune drops it); `"harness"` — its `surface` is a harness surface (unrun, *the
        harness cannot drive it here*); `None` — the record stands as it is. A
        pre-registry record (no `surface`) and one naming an undeclared key stand."""
        if not is_unbuilt(record, app) or app not in APPS:
            return None
        entry = self.registry.get(record.get("surface") or "")
        if entry is None:
            return None
        if app in entry.built:
            return "stale"
        return "harness" if entry.kind == "harness" else None

    def ungated(self, test: str, record: dict, app: str) -> bool:
        """An ungated failure: a `failed`/`error` record on a column that has not
        built a product surface the map says the test requires — read as unbuilt,
        listed as *unbuilt (ungated)*, never a real red. Read for the record itself:
        a failure whose run reached no gate for the surface on a test that no longer
        asks for it is the red it says it is."""
        return record.get("outcome") in ("failed", "error") and bool(
            self.unbuilt(test, app, record=record))


def build_declared(registry: dict, surface_marks: dict, ledger: dict) -> Declared:
    """The registry with the map over `surface_marks` × `ledger` and its borrowed part."""
    smap = surface_map(surface_marks, ledger)
    return Declared(registry=registry, surfaces=smap,
                    borrowed=borrowed_map(surface_marks, smap))


def load_declared(features: Path = FEATURES, surface_marks: dict | None = None,
                  ledger: dict | None = None) -> Declared:
    """The registry beside `features` and the map over `surface_marks` × the ledger."""
    ledger = load_ledger(features) if ledger is None else ledger
    return build_declared(load_surface_registry(surfaces_path(features)),
                          surface_marks or {}, ledger)


def live_records(by_platform: dict, app: str, declared: Declared | None) -> dict:
    """One test's `{platform: record}` with every record the registry reads as unrun
    (stale, or held by a harness surface) dropped — what a column's cell is judged on.
    The nest file is not a column, so its records pass through untouched."""
    if declared is None:
        return by_platform
    return {p: r for p, r in by_platform.items() if declared.record_reading(r, app) is None}


#: The trees a test's staleness is counted over (delayed-testing.md § The last-green
#: directory, *Staleness is read in commits*): the suite, the shared code, the nest,
#: the strings and the harness — plus its own app's tree (`app_tree`).
SHARED_TEST_TREES = ("tests/e2e-unified/", "libs/", "bins/fauna-nest/", "i18n/", "scripts/",
                     "justfile")
#: The apps whose tree is not `apps/fauna-<app>/`: macos and ios share one project.
_APP_DIRS = {"macos": "apps/fauna-apple/", "ios": "apps/fauna-apple/"}


def app_tree(app: str | None) -> str | None:
    """`apps/<its dir>/` for an app column, `None` for the nest or no app. A kids
    column's tree is its parent's: the flavor is built from the same project."""
    if not app or app == NEST:
        return None
    app = parent_app(app)
    return _APP_DIRS.get(app, f"apps/fauna-{app}/")


def load_last_green(platform: str, features: Path = FEATURES) -> dict:
    """The published last-green snapshot's records for one platform,
    `{node id with its parametrization: record}` — `{}` when that platform has never
    published (delayed-testing.md § The last-green directory, *The published
    snapshot*). The record's shape is `tests/e2e-unified/helpers/last_green.py`'s."""
    path = features / "last-green" / f"{platform}.json"
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise CatalogError(f"last-green/{path.name}: {exc}") from exc
    return data.get("tests") or {}


# ── two sources, one observation (§ Cell semantics, *Two sources*) ─────────
#: A directory outcome as the ledger spells it: `(outcome, skip_class)` — only the
#: outcomes the ledger records from the SAME report every time. The directory also
#: notes reds and errors the ledger deliberately declines (a setup-time
#: `NestModeError`, an own-artifact test naming no artifact, a `--strict-app`
#: refusal the ledger records as `skipped: unbuilt`), so a directory red or error
#: cannot say whether the ledger would have recorded it: it never supersedes a
#: ledger record. The inverse of `tests/e2e-unified/helpers/last_green.py::classify`.
_DIRECTORY_OUTCOMES = {
    "green": ("passed", None),
    "skip:unbuilt": ("skipped", "unbuilt"),
    "skip:absence": ("skipped", "absence"),
}
#: The nest-mode tokens a directory key's parametrization can carry — the default
#: mode is unstamped (testing.md § *Mode mechanics*), so a key with none of these
#: is the default `standalone` run.
_MODE_TOKENS = ("docker", "live")
_PARAMS = re.compile(r"\[(.*)\]$")


def _split_directory_key(key: str) -> tuple:
    """`(ledger node id, param tokens)` for a directory key — the ledger drops the
    parametrization exactly as conftest's `_repo_node_id` does (greedy, from the
    first `[` after `::`), so the two key spaces meet on the stripped id."""
    path, sep, rest = key.partition("::")
    match = _PARAMS.search(rest)
    if not match:
        return key, ()
    return path + sep + rest[:match.start()], tuple(match.group(1).split("-"))


def _directory_index(snapshot: dict) -> dict:
    """`{ledger node id: [(param tokens, record)]}` over one platform's snapshot."""
    out: dict = {}
    for key, record in snapshot.items():
        node_id, tokens = _split_directory_key(key)
        out.setdefault(node_id, []).append((tokens, record))
    return out


def _mode_of(tokens: tuple) -> str | None:
    return next((m for m in _MODE_TOKENS if m in tokens), None)


#: A convention-16 seat set's token (`2seat`, `3seat`); the token after it is the
#: set's seat MODES joined by `+` (`native+native`) — `sync_seats.seat_set_param_id`.
_SEAT_TOKEN = re.compile(r"^\d+seat$")
SEAT_MODES_PATH = REPO / "tests" / "e2e-unified" / "helpers" / "seat_modes.py"
_seat_modes_module = None


def _seat_modes():
    """`tests/e2e-unified/helpers/seat_modes.py`, loaded by path — the ONE mode→app
    table conftest's `_apps_for_seat_pair` reads, stdlib-only so the catalog stays
    parse-only (no test helper package is imported)."""
    global _seat_modes_module
    if _seat_modes_module is None:
        import importlib.util
        spec = importlib.util.spec_from_file_location("_fauna_seat_modes", SEAT_MODES_PATH)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _seat_modes_module = module
    return _seat_modes_module


def _seat_modes_of(tokens: tuple) -> tuple | None:
    """The seat modes a seat-set key carries (`("native", "native")`), or None when
    the key is not a seat set's."""
    for i, token in enumerate(tokens):
        if _SEAT_TOKEN.match(token):
            return tuple(tokens[i + 1].split("+")) if i + 1 < len(tokens) else ()
    return None


def seat_key_apps(modes: tuple, platform: str | None) -> frozenset:
    """The apps a seat-set key WITNESSES on the snapshot's `platform` — conftest's
    `_apps_for_seat_pair(under_test=True)` over the same table, the snapshot's
    platform name (`linux`, `macos`, `windows`) mapped to the `sys.platform` value
    the table keys on. Empty for a platform or a mode the table does not know."""
    table = _seat_modes()
    sys_platform = table.SNAPSHOT_SYS_PLATFORM.get(platform or "")
    if sys_platform is None:
        return frozenset()
    return frozenset(table.apps_for_seat_set(list(modes), sys_platform, under_test=True))


def token_columns(tokens: tuple) -> set:
    """The columns a directory key's parametrization names. The key's tokens are its
    ids split on `-`, so a kids channel arrives as two (`android`, `kids`): that pair
    names the kids column and never its parent's, whose own record it is not."""
    out, i = set(), 0
    while i < len(tokens):
        pair = "-".join(tokens[i:i + 2])
        if pair in COLUMN_PARENT:
            out.add(pair)
            i += 2
            continue
        if tokens[i] in APP_COLUMNS:
            out.add(tokens[i])
        i += 1
    return out


def directory_records_for(candidates: list, app: str, nest_mode: str | None,
                          platform: str | None = None) -> list:
    """The directory records answering ONE ledger record of `app` (or the nest file)
    in `nest_mode` on `platform` — every variant the ledger collapsed into it
    (`[linux-eml]`, `[linux-mbox]`, … are one ledger record, worst outcome winning),
    or `[]` when the keys cannot say which, and the ledger then stands.

    An app file's record is answered by the keys carrying its app token, and by the
    seat-set keys (`[2seat-native+native]`) whose modes resolve to it on `platform`
    (`seat_key_apps`: a device-only seat credits only itself); an unparametrized key
    answers when no key names an app (a test the run attributed to the app without
    an app axis) — never a seat-set key, whose app is its modes' alone. The nest
    file's record is answered by keys naming no app and no seat set. Of those, the
    keys stamped with the record's own mode (the default mode is unstamped); a
    `docker` record with none is answered by a lone unstamped key, since an
    own-artifact test records `docker` under any key — never the reverse."""
    def key_apps(tokens: tuple) -> set:
        modes = _seat_modes_of(tokens)
        return token_columns(tokens) if modes is None else set(seat_key_apps(modes, platform))

    if app == NEST:
        named = [(t, r) for t, r in candidates
                 if _seat_modes_of(t) is None and not token_columns(t)]
    else:
        named = [(t, r) for t, r in candidates if app in key_apps(t)]
        if not named and not any(key_apps(t) or _seat_modes_of(t) is not None
                                 for t, _ in candidates):
            named = list(candidates)
    want = nest_mode if nest_mode in _MODE_TOKENS else None
    exact = [(t, r) for t, r in named if _mode_of(t) == want]
    if not exact and want is not None and len(named) == 1 and _mode_of(named[0][0]) is None:
        exact = named
    return [r for _, r in exact]


def is_ancestor(older: str, newer: str, repo: Path = REPO) -> bool:
    """`older` is a PROPER ancestor of `newer` in this checkout's history. False when
    git cannot say (a commit unknown here) — the caller then keeps the ledger."""
    if not older or not newer or older == newer:
        return False
    try:
        run = subprocess.run(
            ["git", "-C", str(repo), "merge-base", "--is-ancestor", older, newer],
            capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return False
    return run.returncode == 0


def directory_is_newer(ledger_record: dict, directory_record: dict,
                       ancestor=is_ancestor) -> bool:
    """Is the directory's newest observation later than the ledger record's?

    The ledger keeps the day only and the directory the second (delayed-testing.md
    § The last-green directory, *The record*), so a later DAY decides alone; on the
    same day the merge bases decide — the directory's (its last green's, which a
    green `last` shares) must descend from the ledger record's. Both bases sit on
    `origin/main` and the snapshot is committed after its runs, so every checkout
    holding the snapshot reads the same answer. Anything else keeps the ledger."""
    last = directory_record.get("last") or {}
    day = str(last.get("time") or "")[:10]
    ledger_day = str(ledger_record.get("date") or "")
    if not day or not ledger_day or day < ledger_day:
        return False
    if day > ledger_day:
        return True
    green = directory_record.get("last_green") or {}
    if last.get("outcome") != "green" or green.get("time") != last.get("time"):
        return False
    return bool(ancestor(ledger_record.get("base") or "", green.get("base") or ""))


def directory_as_ledger_record(ledger_record: dict, directory_records: list) -> dict | None:
    """The directory's newest observation of every variant, in the ledger's record
    shape — or None when the variants disagree or one carries nothing the ledger may
    take (`_DIRECTORY_OUTCOMES`); the ledger then stands.

    The record is the OLDEST variant's (its commit, its day: the lower bound on what
    the run saw), dirty when any variant's green was. The run identity the directory
    does not keep — the version, the nest mode, the artifact — is the ledger record's:
    same test, same platform, same key; so is its `surface` / `surfaces` reach, so the
    registry reads the record as it read the one it supersedes. `source: last-green`
    marks it."""
    if not directory_records:
        return None
    kinds = {(r.get("last") or {}).get("outcome") for r in directory_records}
    if len(kinds) != 1 or (kind := kinds.pop()) not in _DIRECTORY_OUTCOMES:
        return None
    outcome, skip_class = _DIRECTORY_OUTCOMES[kind]
    is_green = outcome == "passed"
    oldest = min(directory_records, key=lambda r: str((r.get("last") or {}).get("time") or ""))
    last = oldest.get("last") or {}
    green = oldest.get("last_green") or {}
    commit = last.get("commit") or ""
    dirty = is_green and any((r.get("last_green") or {}).get("dirty")
                             for r in directory_records)
    version = ledger_record.get("version") or ""
    nest_mode = ledger_record.get("nest_mode") or "standalone"
    metadata = ".".join([commit[:8]] + (["dirty"] if dirty else [])) if commit else ""
    record = dict(ledger_record)
    record.update({
        "outcome": outcome,
        "skip_class": skip_class,
        "commit": commit,
        "base": green.get("base") if is_green else None,
        "date": str(last.get("time") or "")[:10],
        "dirty": bool(dirty),
        "image_digest": None,
        "stamp": f"{version}-dev{'+' + metadata if metadata else ''} {nest_mode}",
        "source": "last-green",
    })
    return record


def overlay_last_green(ledger: dict, features: Path = FEATURES, repo: Path = REPO,
                       ancestor=None) -> dict:
    """Replace, IN PLACE, every ledger record the published snapshot holds a newer
    observation of (§ Cell semantics, *Two sources*). Returns `ledger`.

    Conftest writes both from one event (delayed-testing.md § The last-green
    directory, *one event, two projections*), but a session's ledger write lands only
    when it commits it while the check publishes the directory from the machine's
    store — so the committed ledger can hold an older outcome than the snapshot. A
    record is replaced when EVERY directory variant answering it is newer and they
    agree on an outcome the ledger takes. Only records the ledger already holds are
    replaced: the slug a test answers for is the ledger's knowledge, never the
    directory's. The snapshot in git is read, never the machine-local store, so every
    checkout renders the same matrix. A replaced passing record that is dirty marks
    its feature's cell dirty, as a dirty ledger pass would."""
    if ancestor is None:
        asked: dict = {}

        def ancestor(older, newer):
            # One git call per pair: a sweep's records share a handful of bases.
            if (older, newer) not in asked:
                asked[(older, newer)] = is_ancestor(older, newer, repo)
            return asked[(older, newer)]
    indexes: dict = {}
    for app, features_map in ledger.items():
        for entry in features_map.values():
            for node_id, by_platform in (entry.get("tests") or {}).items():
                for platform, record in list(by_platform.items()):
                    if platform not in indexes:
                        indexes[platform] = _directory_index(load_last_green(platform, features))
                    candidates = indexes[platform].get(node_id)
                    if not candidates:
                        continue
                    found = directory_records_for(candidates, app, record.get("nest_mode"), platform)
                    if not found or not all(directory_is_newer(record, r, ancestor)
                                            for r in found):
                        continue
                    replaced = directory_as_ledger_record(record, found)
                    if replaced is None:
                        continue
                    by_platform[platform] = replaced
                    if replaced["dirty"] and isinstance(entry.get("cell"), dict):
                        entry["cell"]["dirty"] = True
    return ledger


def behind_commits(record: dict, app: str | None, repo: Path = REPO,
                   ref: str = "origin/main") -> int | None:
    """How far `record`'s last green is behind: the `ref` commits since its `base`
    that touch the test's trees. `None` when it has no green, no base, or git cannot
    count (the base unknown to this checkout); the day age is the caller's to show."""
    base = ((record or {}).get("last_green") or {}).get("base")
    if not base:
        return None
    trees = list(SHARED_TEST_TREES) + ([app_tree(app)] if app_tree(app) else [])
    try:
        counted = subprocess.run(
            ["git", "-C", str(repo), "rev-list", "--count", f"{base}..{ref}", "--", *trees],
            capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    if counted.returncode != 0:
        return None
    return int(counted.stdout.strip() or 0)


def newest_verified(releases: list) -> dict | None:
    """The release table's most recent **verified** row — the pin's one source.

    "Verified" and not merely "recorded" is the whole distinction § Dimension 6's
    update rule is built on (re-ruled 2026-10-02, when the private `:latest` was
    retired and the pin stopped following a promote): a build whose verify gate went
    red never proved it serves anywhere, and pinning it would aim the skew grid at an
    artifact nobody runs. Newest is by the `verified` date, ties broken by position —
    the table is appended in act order, so the later row is the later act.
    """
    best = None
    for index, row in enumerate(releases):
        if not row.get("verified"):
            continue
        key = (row.get("verified") or "", index)
        if best is None or key > best[0]:
            best = (key, row)
    return best[1] if best else None


def pin_toml(row: dict) -> str:
    """`pinned-previous-build.toml`'s full body for one verified release row.

    The file's *semantics* are unchanged and still owned by version-compatibility.md
    § Dimension 6; what changed in step 4 is only that it is derived rather than
    hand-copied. The header says so, because the next person to meet this file will
    otherwise edit it — its previous header told them to.
    """
    commit = row.get("commit") or ""
    image = row.get("image") or f"{NEST_IMAGE_REPO}:sha-{commit}"
    released = row.get("verified") or row.get("date") or ""
    return f"""# Pinned previous build — the most recent VERIFIED staged build, as a repo constant.
#
# GENERATED by `just features-render` from the feature catalog's release table — its
# newest row carrying a `verified` date. Do not edit by hand: `just release-verified
# <commit>` is the act that advances it, and `just features-lint` rule 5 reds if this
# file drifts from the table. Owner of the table:
# docs/goal/architecture/feature-catalog.md § Tag gate and release table.
#
# Convention owner (semantics — unchanged by the derivation):
# docs/goal/architecture/version-compatibility.md § Dimension 6. Consumed by the
# tier_3 real-binary version-skew + client at-rest upgrade-in-place tests.
#
# "Previous build" = the most recent verified staged build: a build whose verify gate
# went green against the staging box, not merely dispatched (image-pipeline.md § Image
# tags & channels). `commit` is that build's headSha, `nest_image` its immutable ghcr
# tag. Previous CLIENT binaries are rebuilt from source at `commit` (no client release
# artifacts exist pre-1.0), cached under /work/tmp/fauna-prev-builds/<commit>/<profile>/
# on the Linux dev machine.
#
# Advancing the pin past a ratified in-place compat break needs no further edit: the
# grid's expected-incompatible windows are keyed on this commit's ancestry
# (helpers/prev_build.py::RATIFIED_INPLACE_BREAKS), so the cells re-assert on their
# own. The publish gate's `secret_scan` entry for the new 40-hex run is landed by
# `just release-verified` in the same act, so a pin advance never reds the merge.

commit = "{commit}"
nest_image = "{image}"
released = "{released}"
"""


def render_pin(features: Path = FEATURES) -> str | None:
    """The pin file's derived body, or `None` while no verified row exists.

    `None` is not "empty" — it means the table cannot yet answer the question, and
    the committed pin (hand-written since 2026-07-12) stands untouched. The render
    takes the file over on the first `just release-verified` and never gives it back.
    """
    row = newest_verified(load_releases(features))
    return pin_toml(row) if row else None


def write_pin(features: Path = FEATURES, repo: Path = REPO) -> str | None:
    """Write the derived pin; return `PIN_REL` if it changed, else `None`."""
    content = render_pin(features)
    if content is None:
        return None
    path = repo / PIN_REL
    if path.exists() and path.read_text(encoding="utf-8") == content:
        return None
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return PIN_REL


def check_pin(features: Path = FEATURES, repo: Path = REPO) -> str | None:
    """`PIN_REL` if the committed pin differs from the derived one, else `None`."""
    content = render_pin(features)
    if content is None:
        return None
    path = repo / PIN_REL
    if not path.exists() or path.read_text(encoding="utf-8") != content:
        return PIN_REL
    return None


# ── cell computation ────────────────────────────────────────────────────────
def _fold_platform_records(by_platform: dict) -> dict:
    """One test's multiple platform records fold to ONE synthetic record for cell
    computation (§ Cell semantics: cross-platform disagreement is a *mixed* outcome
    like any other) — passing only if EVERY platform passed, else a representative
    non-passing record, so one failing platform is never masked by a passing sibling.
    The representative is a real red first, so an `unbuilt` skip on one platform
    never hides a failure on another (§ Cell semantics, the *unbuilt* row).
    `compute_cell` reads only `outcome` and `skip_class` off the result."""
    non_passing = [r for r in by_platform.values() if r.get("outcome") not in PASSING]
    non_passing.sort(key=lambda r: not is_real_red(r))
    return non_passing[0] if non_passing else next(iter(by_platform.values()))


def ledger_for(surface: str, app: str) -> str:
    """The ledger file that answers for one surface on `app`'s column — the app's own
    file for the `app` surface, `nest.json` for the `nest` one, whichever column is
    asking (§ The two surfaces: a nest record counts for every column)."""
    return app if surface == "app" else NEST


def _ledger_entry(page: Page, ledger: dict, surface: str, app: str) -> dict:
    return (ledger.get(ledger_for(surface, app)) or {}).get(page.slug) or {}


def recorded_tests(page: Page, ledger: dict, surface: str, app: str) -> dict:
    """`{node id: {platform: record}}` as the answering ledger file holds it for this
    page — UNFOLDED. `surface_view` folds it to judge a cell; a reader diagnosing a
    red wants each platform's own record (its outcome, stamp and date), which the
    fold keeps only one representative of (§ The ledger, *Platform keying*)."""
    return _ledger_entry(page, ledger, surface, app).get("tests") or {}


def mapped_tests(page: Page | None) -> frozenset:
    """Every test id the page's contract maps, on either surface — `frozenset()` for a
    slug that has no page. The set a ledger record under this slug must belong to."""
    if page is None:
        return frozenset()
    return frozenset(t for o in page.outcomes for t in o.tests)


#: The ledger platforms (`helpers/feature_ledger.host_platform`) each app column can be
#: run on — the machines whose `--app sweep` set can name the app (`conftest.sweep_apps`),
#: by the platform each machine records: Linux linux, macOS macos, Windows windows. A
#: record keyed by any other platform is one no run can ever refresh (§ The ledger,
#: *Stale records*, ruling (e)) — web runs on Linux alone (build-system.md § The Deno
#: build sandbox). The nest surface (`nest.json`) is not a column: its file is pruned
#: by the VENUES a feature's nest set holds instead (`NEST_VENUE_PLATFORMS`, ruling (g)).
COLUMN_PLATFORMS = {
    "web": frozenset({"linux"}),
    "linux": frozenset({"linux"}),
    "tui": frozenset({"linux", "macos", "windows"}),
    "android": frozenset({"linux", "macos"}),
    "macos": frozenset({"macos"}),
    "ios": frozenset({"macos"}),
    "windows": frozenset({"windows"}),
}
#: A kids channel runs where its parent runs — the parent's driver, on its venue.
COLUMN_PLATFORMS.update({kids: COLUMN_PLATFORMS[parent] for kids, parent in COLUMN_PARENT.items()})

#: Every platform a run records (`helpers/feature_ledger.host_platform`).
PLATFORMS = frozenset({"linux", "macos", "windows"})

#: The platforms a NEST witness can run on, by the venue its node id sits under — the
#: nest file's half of the platform rule (§ The ledger, *Stale records*, rulings (g)
#: and (h)). A `tests/platform/docker/` module boots its own container (*Own-artifact
#: records*), which needs a docker daemon: Linux and the hosted runner have one (both
#: record `linux`); macOS has none by design (its setup notes say so) and Windows's setup
#: installs none. A `tests/platform/<os>/` module drives that OS's own artifact — the
#: linux or windows sync agent, the macOS File Provider, the iOS simulator (which runs
#: on macOS) — and is a `skipif` everywhere else. Either way the module skips at setup
#: through a plain `skipif`, which the writer records as nothing — so no run elsewhere
#: can ever see such a witness, and the platforms a feature's nest set can be observed
#: on follow from this table alone (`legs`). A venue not listed runs everywhere.
NEST_VENUE_PLATFORMS = {
    "tests/e2e-unified/tests/platform/docker/": frozenset({"linux"}),
    # The fediverse interop venue brings up its pinned peer with `docker compose`
    # (its conftest), so it needs the daemon exactly as the docker venue does.
    "tests/e2e-unified/tests/platform/fediverse/": frozenset({"linux"}),
    "tests/e2e-unified/tests/platform/linux/": frozenset({"linux"}),
    "tests/e2e-unified/tests/platform/windows/": frozenset({"windows"}),
    "tests/e2e-unified/tests/platform/macos/": frozenset({"macos"}),
    "tests/e2e-unified/tests/platform/ios/": frozenset({"macos"}),
}


def venue_platforms(node_id: str) -> frozenset:
    """The platforms a witness can run on by its venue — every platform unless its
    node id sits under a venue `NEST_VENUE_PLATFORMS` narrows."""
    for prefix, platforms in NEST_VENUE_PLATFORMS.items():
        if node_id.startswith(prefix):
            return platforms
    return PLATFORMS


def witness_platforms(node_id: str, platform_marks: dict | None = None) -> frozenset:
    """A witness's HOST PLATFORMS (§ The ledger, *Stale records*, ruling (i)): its
    venue's (`venue_platforms`) intersected with its `@pytest.mark.host_platform`
    marker's (`platform_marks`, the third half of `scan_marks`) — every platform with
    neither. Decided from declarations alone, never from one machine's collection."""
    venue = venue_platforms(node_id)
    marked = (platform_marks or {}).get(node_id)
    return venue if marked is None else venue & frozenset(marked)


def candidate_platforms(app: str) -> frozenset:
    """The platforms a surface's cell can be stamped on before its witnesses narrow
    them: every platform for the nest file, the column's runners for an app file
    (`COLUMN_PLATFORMS`); an unknown file name gets every platform."""
    return PLATFORMS if app == NEST else COLUMN_PLATFORMS.get(app, PLATFORMS)


def platform_sets(page: Page | None, app: str = NEST, app_marks: dict | None = None,
                  platform_marks: dict | None = None) -> dict:
    """`{platform: frozenset(node ids)}` — each candidate platform's set for one
    surface of a feature: its applicable citations (the nest witnesses, or the column's
    `page.surface_tests("app", app, app_marks)`) whose host platforms include it
    (`witness_platforms`; rulings (h) and (i)). Empty sets for a page with no witness."""
    if page is None:
        tests = []
    elif app == NEST:
        tests = list(page.surface_tests(NEST))
    else:
        tests = list(page.surface_tests("app", app, app_marks))
    return {p: frozenset(t for t in tests if p in witness_platforms(t, platform_marks))
            for p in sorted(candidate_platforms(app))}


def legs(page: Page | None, app: str = NEST, app_marks: dict | None = None,
         platform_marks: dict | None = None) -> tuple:
    """The LEGS of one surface's set (§ The ledger, *Stale records*, rulings (h) and
    (i)): one per distinct platform set that no other platform's set contains
    outright, each a frozenset of the platforms sharing it. A platform whose set a
    sibling's contains is redundant — it owes nothing a sibling does not observe in
    full — and belongs to no leg; an empty set is no leg. The surface is complete when
    every leg holds a stamp (`helpers/feature_ledger.fold_platform_cells`); a cell
    stamp keyed by a platform on no leg is one no run should ever write
    (`stale_records`). One definition for every file: the nest file's candidates are
    `PLATFORMS`, an app column's `COLUMN_PLATFORMS[app]`.

    Measured 2026-10-31 on the nest half: an ordinary page has one leg holding all
    three platforms; a page with a docker witness one leg, `linux`; `notifications`
    two legs, `linux` and `windows`, `macos` redundant under both. Measured 2026-11-02
    on the app half: a single-platform column has one leg of one platform; `tui` on
    identity-protected-on-this-device two legs, `linux` and `windows`, `macos`
    redundant under both. Sorted for a stable render."""
    by_set: dict = {}
    for platform, tests in platform_sets(page, app, app_marks, platform_marks).items():
        if tests:
            by_set.setdefault(tests, set()).add(platform)
    maximal = [s for s in by_set if not any(s < other for other in by_set)]
    return tuple(sorted((frozenset(by_set[s]) for s in maximal), key=sorted))


def set_platforms(page: Page | None, app: str = NEST, app_marks: dict | None = None,
                  platform_marks: dict | None = None) -> frozenset:
    """The platforms that may hold a cell stamp for this surface of the feature: the
    union of its legs — a platform whose set a sibling's contains outright can complete
    nothing a sibling does not, so its stamp is one no run should write (ruling (g)'s
    reading, kept). The candidate platforms for a page with no witness there."""
    found = legs(page, app, app_marks, platform_marks)
    if not found:
        return candidate_platforms(app)
    return frozenset().union(*found)


def scope_by_platform(page: Page | None, app: str = NEST, app_marks: dict | None = None,
                      platform_marks: dict | None = None) -> dict:
    """`{platform: set(node ids)}` the ledger writer judges a cell against on each
    platform (`helpers/feature_ledger.cell_complete`, rulings (h) and (i)): that
    platform's set, for the platforms on a leg; a redundant platform gets no entry, so
    a run there never stamps a cell the prune would drop."""
    sets = platform_sets(page, app, app_marks, platform_marks)
    return {p: set(sets[p]) for p in sorted(set_platforms(page, app, app_marks, platform_marks))
            if sets[p]}


def legs_by_slug(features: Path = FEATURES, marks: tuple | None = None) -> dict:
    """`{(file, slug): legs(page, file, …)}` for every page under `features` and every
    ledger file (every column and `nest`) — what a re-folder that holds no `Page`
    objects (the merge-gate harvest, the rebase resolver) passes to
    `fold_platform_cells` (rulings (h) and (i)). `marks` is `scan_marks`' answer; by
    default the tree beside `features` is scanned (~2 s)."""
    app_marks, _surfaces, platform_marks = marks or scan_marks(features.parent.parent)
    return {(app, page.slug): legs(page, app, app_marks, platform_marks)
            for page in load_pages(features) for app in (*APPS, NEST)}


def misfolded_cells(pages: list, ledger: dict, app_marks: dict | None = None,
                    platform_marks: dict | None = None) -> list:
    """`[(file, slug)]` for every cell, in any ledger file, whose top-level fields
    disagree with the fold of its own `platforms` over the surface's legs (rulings (h)
    and (i)) — a contract or marker that gained or lost a leg since the cell was last
    written, or a re-fold made without the legs. The cell is re-folded by
    `prune_ledger`; lint rule 6 refuses a ledger that holds one."""
    by_slug = {page.slug: page for page in pages}
    fold = _fold_platform_cells()
    out = []
    for app, features in sorted(ledger.items()):
        if app != NEST and app not in APPS:
            continue
        for slug, entry in sorted(features.items()):
            cell = entry.get("cell") or {}
            platforms = cell.get("platforms")
            if not platforms or slug not in by_slug:
                continue
            found = legs(by_slug[slug], app, app_marks, platform_marks)
            if fold(platforms, legs=found) != cell:
                out.append((app, slug))
    return out


def stale_unbuilt_records(ledger: dict, registry: dict) -> list:
    """`[(app file, slug, node id, platform)]` for every `skipped: unbuilt` record whose
    `surface` the registry lists as built on the record's column (§ Cell semantics,
    *Staleness is decided by the registry*) — rule 6 refuses them, the prune drops
    them, the render reads them as unrun (`Declared.record_reading`, the one test). A
    pre-registry record (no `surface`) is left for the migration's last step."""
    probe = Declared(registry=registry)
    return sorted((app, slug, node_id, platform)
                  for app, features in ledger.items() if app in APPS
                  for slug, entry in features.items()
                  for node_id, by_platform in (entry.get("tests") or {}).items()
                  for platform, record in by_platform.items()
                  if probe.record_reading(record, app) == "stale")


def surfaceless_unbuilt_records(ledger: dict) -> list:
    """`[(app file, slug, node id, platform)]` for every `skipped: unbuilt` record with
    no `surface` key — the pre-registry shape the free-text gate wrote (§ Cell semantics,
    *Not built is declared*). NOT stale by `stale_records`' reading (the lint leaves them
    standing); the migration's one-time prune drops them all at once
    (`features-prune --unbuilt-without-surface`), and a gate that still fires on that
    column writes the record back with its key on the next run."""
    return sorted((app, slug, node_id, platform)
                  for app, features in ledger.items() if app in APPS
                  for slug, entry in features.items()
                  for node_id, by_platform in (entry.get("tests") or {}).items()
                  for platform, record in by_platform.items()
                  if record.get("skip_class") == "unbuilt" and not record.get("surface"))


def stale_records(pages: list, ledger: dict, registry: dict | None = None,
                  app_marks: dict | None = None, platform_marks: dict | None = None) -> list:
    """`[(app file, slug, node id, platform)]` for every stale record (§ The ledger,
    *Stale records*), sorted so the lint's report and the prune's are stable:

    - `platform` `None` — the test's records on every platform: its feature's contract
      no longer maps it (re-marked, renamed, deleted, or filed under a feature whose page
      is gone). Decided from the contract alone — the contract and lint rule 2 already
      hold every mapped test to exist and carry its marker — never from one machine's
      collection, so a windows-only test is not stale on Linux;
    - `platform` named — that one platform's record, on a platform the witness cannot
      run on for this file (`candidate_platforms` ∩ `witness_platforms`: the column's
      runners, its venue, its `host_platform` marker), or — `node id` `None`, the
      feature cell's own stamp — on a platform on no leg of that surface's set
      (`set_platforms`, rulings (e), (g), (h) and (i), one rule for every file) — or,
      with `registry`, a `skipped: unbuilt` record the registry has made stale
      (`stale_unbuilt_records`).

    `app_marks` / `platform_marks` are `scan_marks`' first and third halves; without
    them every test reads as unmarked (every column, every platform)."""
    by_slug = {page.slug: page for page in pages}
    out = []
    for app, features in sorted(ledger.items()):
        for slug, entry in sorted(features.items()):
            page = by_slug.get(slug)
            mapped = mapped_tests(page)
            tests = entry.get("tests") or {}
            cell_runnable = set_platforms(page, app, app_marks, platform_marks)
            cell_platforms = (entry.get("cell") or {}).get("platforms") or {}
            out += [(app, slug, None, p) for p in sorted(cell_platforms)
                    if p not in cell_runnable]
            for t in sorted(tests):
                if t not in mapped:
                    out.append((app, slug, t, None))
                    continue
                test_runnable = candidate_platforms(app) & witness_platforms(t, platform_marks)
                out += [(app, slug, t, p) for p in sorted(tests[t]) if p not in test_runnable]
    if registry:
        seen = set(out)
        # A record its feature no longer maps is already removed whole.
        unmapped = {(a, s, t) for a, s, t, p in out if p is None}
        out += [r for r in stale_unbuilt_records(ledger, registry)
                if r not in seen and r[:3] not in unmapped]
        out.sort(key=lambda r: (r[0], r[1], r[2] or "", r[3] or ""))
    return out


def _fold_platform_cells():
    """The cell fold is the ledger writer's (§ The ledger, *Platform keying*) — one
    definition, imported the way `gate-check-ledger-harvest.py` reaches it."""
    import sys
    e2e = str(REPO / "tests" / "e2e-unified")
    if e2e not in sys.path:
        sys.path.insert(0, e2e)
    from helpers.feature_ledger import fold_platform_cells  # noqa: PLC0415 — a test-tree import, not a dep
    return fold_platform_cells


def prune_ledger(features: Path = FEATURES, *, unbuilt_without_surface: bool = False,
                 marks: tuple | None = None) -> list:
    """Remove every `stale_records` record from the ledger files and return them;
    with `unbuilt_without_surface`, also every `skipped: unbuilt` record that names
    no surface (`surfaceless_unbuilt_records`, the migration's one-time last step).

    A feature whose page is gone loses its whole entry (its `cell` names a stamp for a
    contract nobody holds); a feature that still has a page keeps its `cell`, which is
    judged on mapped tests only. An unrunnable platform's cell stamp is dropped and the
    cell re-folded over the platforms left (or removed when none is). A cell whose top
    level disagrees with the fold of its platforms over its surface's legs
    (`misfolded_cells`, rulings (h) and (i)) is re-folded in the same write; it is not
    a record and is not in the returned list. `marks` is `scan_marks`' answer; by
    default the tree beside `features` is scanned. The files are rewritten exactly as the
    run-time writer writes them (`indent=2, sort_keys`), so an untouched file stays
    byte-identical."""
    loaded = load_pages(features)
    pages = {p.slug for p in loaded}
    by_slug = {p.slug: p for p in loaded}
    ledger = load_ledger(features, last_green=False)
    app_marks, _surfaces, platform_marks = marks or scan_marks(features.parent.parent)
    stale = stale_records(loaded, ledger, load_surface_registry(surfaces_path(features)),
                          app_marks, platform_marks)
    if unbuilt_without_surface:
        seen = set(stale)
        # A record its feature no longer maps is already removed whole.
        unmapped = {(a, s, t) for a, s, t, p in stale if p is None}
        stale += [r for r in surfaceless_unbuilt_records(ledger)
                  if r not in seen and r[:3] not in unmapped]
        stale.sort(key=lambda r: (r[0], r[1], r[2] or "", r[3] or ""))
    ledger_dir = features / "ledger"
    misfolded = set(misfolded_cells(loaded, ledger, app_marks, platform_marks))
    touched = {app for app, _, _, _ in stale} | {app for app, _ in misfolded}
    for app in sorted(touched):
        path = ledger_dir / f"{app}.json"
        data = json.loads(path.read_text(encoding="utf-8"))
        feats = data.get("features") or {}
        for a, slug, node_id, platform in stale:
            if a != app or slug not in feats:
                continue
            if slug not in pages:
                feats.pop(slug)
            elif node_id is None:
                cell = feats[slug]["cell"]
                cell["platforms"].pop(platform, None)
                if cell["platforms"]:
                    feats[slug]["cell"] = _fold_platform_cells()(
                        cell["platforms"],
                        legs=legs(by_slug[slug], app, app_marks, platform_marks))
                else:
                    feats[slug].pop("cell")
            elif platform is None:
                feats[slug]["tests"].pop(node_id, None)
            else:
                by_platform = feats[slug]["tests"][node_id]
                by_platform.pop(platform, None)
                if not by_platform:
                    feats[slug]["tests"].pop(node_id)
        for a, slug in sorted(misfolded):
            cell = (feats.get(slug) or {}).get("cell") or {}
            if a == app and cell.get("platforms"):
                feats[slug]["cell"] = _fold_platform_cells()(
                    cell["platforms"], legs=legs(by_slug[slug], app, app_marks, platform_marks))
        path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return stale


def surface_view(page: Page, ledger: dict, surface: str, app: str,
                 app_marks: dict | None = None, declared: Declared | None = None) -> tuple:
    """`(participates, tests, records, cell)` for one of a feature's witness surfaces.

    Public because it is `compute_cell`'s whole input: the maintainers' view that
    explains a cell by § Closing a gap's three causes reads exactly this rather than
    re-deriving which tests a column's cell is judged on.

    `participates` is "this surface has at least one mapped test" — a surface whose
    outcomes are all `(none)` has nothing to witness and must not, on its own, empty
    the cell; that is what the contract's *partial* already says.

    The `app` surface is narrowed to the witnesses THIS column can have (§ Cell
    semantics, *for that app*): a windows-marked test is not part of tui's set, and
    counting it would leave tui's cell permanently unstampable — no machine collects
    all seven apps. The `nest` surface is never narrowed: a nest record counts for
    every column by definition, so it is scoped by the contract alone.

    `records` is one FOLDED record per test (`_fold_platform_records`), never the
    raw `{platform: record}` map — every caller here judges pass/fail, and the fold
    is where cross-platform disagreement becomes a mixed outcome. With `declared`, an
    app record the registry reads as unrun (`live_records`) is dropped before the
    fold, so a test whose every record is stale or harness-held reads as unrun.
    """
    tests = (page.surface_tests(surface, app, app_marks) if surface == "app"
             else page.surface_tests(surface))
    if not tests:
        return False, [], {}, None
    entry = _ledger_entry(page, ledger, surface, app)
    recorded = entry.get("tests") or {}
    if surface == "app":
        recorded = {t: live for t, by_platform in recorded.items()
                    if (live := live_records(by_platform, app, declared))}
    return True, tests, {t: _fold_platform_records(recorded[t]) for t in tests if t in recorded}, \
        entry.get("cell")


def unbuilt_witnesses(app_tests: list, app_records: dict, app: str,
                      declared: Declared | None) -> dict:
    """`{node id: route}` for every `[app]` witness *unbuilt* for the column (§ Cell
    semantics, *One state, two routes*): `"recorded"` — its folded record is
    `skipped: unbuilt`; `"declared"` — no record, and the registry says the column
    has not built a product surface the map says it requires; `"ungated"` — a
    `failed`/`error` record under such a declaration. A record that PASSED refutes a
    declaration rather than being read through it (lint rule 7 reports the stale
    declaration), so it is never unbuilt here."""
    out = {}
    for test in app_tests:
        record = app_records.get(test)
        if record is not None and is_unbuilt(record, app):
            out[test] = "recorded"
        elif declared is None:
            continue
        elif record is None:
            if declared.unbuilt(test, app):
                out[test] = "declared"
        elif declared.ungated(test, record, app):
            out[test] = "ungated"
    return out


def compute_cell(page: Page, app: str, ledger: dict, app_marks: dict | None = None,
                 declared: Declared | None = None) -> Cell:
    """The mark for one (feature, app column), computed — never typed (§ Cell semantics).

    `app_marks` (`features_catalog.app_marks()`) is what makes the column's scope its
    own; without it every witness reads as applicable to every column, which is the
    honest answer for a caller that has no marker knowledge — and the answer for an
    unmarked test either way. `declared` (`load_declared`) adds the registry's route
    to *unbuilt*; without it the cell reads the recorded route alone.
    """
    if app in page.absences:
        return Cell("absent", "", "")

    app_part, app_tests, app_records, app_cell = surface_view(
        page, ledger, "app", app, app_marks, declared)
    nest_part, nest_tests, nest_records, nest_cell = surface_view(
        page, ledger, NEST, app, app_marks)

    if not (app_part or nest_part):
        return Cell("none", "", "")

    records = list(app_records.values()) + list(nest_records.values())
    stamp, date, dirty = _older_stamp(app_cell, nest_cell)
    # An unbuilt `[app]` witness — recorded, declared or ungated — is read BEFORE the
    # nest conjunct: the column cannot reach ✅ until the app builds, whatever the nest
    # half says. A real red anywhere on the column beside it is ⚠ — the failure is the
    # more urgent signal; an ungated failure is read as unbuilt, never as that red.
    unbuilt = unbuilt_witnesses(app_tests, app_records, app, declared)
    if unbuilt:
        ungated = {t for t, route in unbuilt.items() if route == "ungated"}
        reds = [r for t, r in app_records.items() if t not in ungated] + \
            list(nest_records.values())
        if any(is_real_red(r) for r in reds):
            return Cell("partial", stamp, date, dirty)
        return Cell("unbuilt", stamp, date, dirty)
    # The nest half is a conjunct on every column: a participating surface with no
    # record leaves the cell empty however green the other half is.
    if (app_part and not app_records) or (nest_part and not nest_records):
        return Cell("none", "", "")

    mapped = len(app_tests) + len(nest_tests)
    passed = [r for r in records if r.get("outcome") in PASSING]

    if not passed:
        return Cell("failing", stamp, date, dirty)
    if page.full_for(app, app_marks) and len(records) == mapped and len(passed) == len(records):
        return Cell("full", stamp, date, dirty)
    return Cell("partial", stamp, date, dirty)


def _older_stamp(*cells) -> tuple:
    """Each cell shows the *older* of its two stamps (§ Cell semantics), and that
    stamp's `dirty` — absent on a cell written before the field existed, which reads
    as clean (§ The ledger, *Dirty trees*)."""
    present = [c for c in cells if c and c.get("stamp")]
    if not present:
        return "", "", False
    oldest = min(present, key=lambda c: (c.get("date") or "9999-99-99", c.get("stamp") or ""))
    return oldest.get("stamp") or "", oldest.get("date") or "", bool(oldest.get("dirty"))


# ── render ──────────────────────────────────────────────────────────────────
_LEGEND = f"""\
{MARKS['full']} every outcome in the feature's contract is mapped to a test and every
one of those tests passed for that app &middot; {MARKS['partial']} the contract has an
outcome no test witnesses yet, the mapped tests are mixed, or a test needed something
not yet built while another failed &middot;
{MARKS['failing']} tests witness this feature and none of them passed, and none of them
reported missing build work &middot; {MARKS['unbuilt']} at its last run, one of the
feature's tests needed something not yet built for that app (a part of the app, or the
test tooling that drives it), and nothing else on that cell failed — the work the mark
points to is building, not fixing &middot; *(blank)* our records hold no run for this feature and app &middot; {MARKS['absent']}
the feature is absent on that app by design, and the page says where that is written.

Each cell carries the stamp of the nest artifact its tests were witnessed against (where
one complete run spanned two artifacts, the older of them): `<version> <nest mode>` means
the released artifact carrying that version, and `<version>-dev+<commit> <nest mode>`
means a build of that commit. `.dirty` after the commit means the run's working tree
carried uncommitted changes when it was collected, so that commit is a lower bound on the
code it saw — the next run from a clean tree supersedes it.
"""


def _cells_for(page: Page, ledger: dict, app_marks: dict | None = None,
               declared: Declared | None = None) -> dict:
    return {app: compute_cell(page, app, ledger, app_marks, declared) for app in APPS}


def _matrix_md(pages: list, ledger: dict, releases: list, app_marks: dict | None = None,
               declared: Declared | None = None, drafts: dict | None = None) -> str:
    out = ["# Feature matrix",
           "",
           "Generated by `just features-render` from every page in this directory and",
           "the ledger the test runs write. Nothing here is typed by hand.",
           "",
           _LEGEND]
    by_section: dict = {}
    for page in pages:
        by_section.setdefault(page.section, []).append(page)
    for section in sorted(by_section, key=lambda s: (_section_rank(s), s)):
        out.append(f"## {section[:1].upper()}{section[1:]}")
        out.append("")
        out.append("| Feature | " + " | ".join(APPS) + " |")
        out.append("|---" * (len(APPS) + 1) + "|")
        for page in by_section[section]:
            out.append(_matrix_row(page, ledger, app_marks, declared, drafts))
        out.append("")
    out.append("## Releases")
    out.append("")
    if releases:
        out.append(_RELEASE_LEGEND)
        out.append("")
        out.append("| Version | Commit | Image digest | Public commit | Date | Verified "
                   "| Candidate run | Promoted | Apps |")
        out.append("|---|---|---|---|---|---|---|---|---|")
        for row in releases:
            public = (row.get("public_commit") or "")[:12]
            out.append("| {} | `{}` | `{}` | {} | {} | {} | {} | {} | {} |".format(
                row.get("version", ""), (row.get("commit") or "")[:12],
                row.get("image_digest", ""), f"`{public}`" if public else "",
                row.get("date", ""),
                row.get("verified") or "", row.get("candidate_run") or "",
                "yes" if row.get("promoted") else "no",
                _shipped_apps(row)))
    else:
        out.append("No build has been recorded yet.")
    out.append("")
    return "\n".join(out) + "\n"


#: § Release train. The train is the default and needs no per-row prose; what a reader
#: has to be told is what a *non-empty* Apps column means, because that is the store
#: reship carve-out becoming visible rather than being inferred from a store's history.
_RELEASE_LEGEND = (
    "One row per nest image build the catalog has something to say about. "
    "**Commit** is the development commit the release is keyed on; **Public commit**, "
    "when present, is the commit in the public repository whose tree — published from "
    "that development commit — the image was built from. "
    "**Verified** is the date the build's verify gate went green on the staging box. "
    "**Candidate run** is the date of its newest release-candidate run, the record the "
    "tag gate reads. **Promoted** says the build became the public `:latest`. "
    "A release is a train — the nest and all seven apps ship together at "
    "one version — so **Apps** is empty on an ordinary release and names only an app "
    "whose shipped version differs, which happens when a store-mechanical re-upload "
    "ships that one app at a later patch."
)


def _shipped_apps(row: dict) -> str:
    """The per-app shipped-version cell: empty for a plain train, `app v` per exception."""
    shipped = row.get("shipped_apps") or {}
    if not isinstance(shipped, dict) or not shipped:
        return ""
    return ", ".join(f"{app} {shipped[app]}" for app in sorted(shipped))


def _matrix_row(page: Page, ledger: dict, app_marks: dict | None = None,
                declared: Declared | None = None, drafts: dict | None = None) -> str:
    title = (drafts or {}).get(page.slug, {}).get("title", page.title)
    label = f"[{title}]({page.slug}.md)"
    if page.nest_only:
        cell = compute_cell(page, APPS[0], ledger, app_marks, declared)
        body = _cell_text(cell) + " (the nest — every app)"
        return f"| {label} | " + " | ".join([body] + [""] * (len(APPS) - 1)) + " |"
    cells = _cells_for(page, ledger, app_marks, declared)
    return f"| {label} | " + " | ".join(_cell_text(cells[app]) for app in APPS) + " |"


def _cell_text(cell: Cell) -> str:
    if cell.state == "none":
        return ""
    return f"{cell.mark} {cell.stamp}".strip()


def _status_block(page: Page, ledger: dict, app_marks: dict | None = None,
                  declared: Declared | None = None) -> str:
    lines = ["| App | Status | Stamp |", "|---|---|---|"]
    # The same per-column scope the matrix row reads — without the marks every
    # column is judged on all seven apps' witnesses and the two disagree.
    cells = _cells_for(page, ledger, app_marks, declared)
    for app in APPS:
        cell = cells[app]
        state = "no run recorded" if cell.state == "none" else cell.state
        lines.append(f"| {app} | {cell.mark} {state} | {cell.stamp} |".replace("|  |", "| |"))
    lines.append("")
    lines.append("| Outcome | Surface | Witness | Newest outcome |")
    lines.append("|---|---|---|---|")
    for outcome in page.outcomes:
        if not outcome.tests:
            lines.append(f"| {outcome.number} | {outcome.surface} | (none) | — |")
        for test in outcome.tests:
            # A maintainer-only witness renders in the same excised spelling the
            # contract uses, so the shipped status table never names a test the
            # public tree does not carry (§ The coverage contract, *Maintainer-only
            # witnesses*) — and rule 5 keeps the two spellings in step.
            witness = (maintainer_only_witness(test) if test in outcome.maintainer_only
                       else f"`{test}`") + _surface_note(test, declared)
            lines.append(
                f"| {outcome.number} | {outcome.surface} | {witness} | "
                f"{_witness_summary(page, outcome.surface, test, ledger)} |")
        # A per-outcome absence is part of what the page says about the outcome, so
        # it is visible beside its witnesses — never only in the front-matter.
        excused = [app for app in APPS if page.absent_from(app, outcome)]
        if excused:
            lines.append(f"| {outcome.number} | {outcome.surface} | "
                         f"absent by design on {', '.join(excused)} | — |")
    return "\n".join(lines)


def _surface_note(test: str, declared: Declared | None) -> str:
    """The registry surfaces a witness requires that some app has not built, named
    beside it (§ Cell semantics, *Not built is declared*), so a reader of an
    *unbuilt* column sees which surface holds it short and where: ` — requires
    `events-day-cell` (not built: android, windows)`. A surface built everywhere, or
    one the registry does not declare, says nothing."""
    if declared is None:
        return ""
    parts = []
    for key in sorted(declared.surfaces.get(test, ())):
        entry = declared.registry.get(key)
        if entry is None:
            continue
        short = [a for a in APPS if a not in entry.built]
        if short:
            kind = "" if entry.kind == "product" else " (test harness)"
            parts.append(f"`{key}`{kind} (not built: {', '.join(short)})")
    return f" — requires {'; '.join(parts)}" if parts else ""


def _witness_summary(page: Page, surface: str, test: str, ledger: dict) -> str:
    """Per app, per PLATFORM — the page's own `## Status` block is where a
    cross-platform disagreement the folded matrix cell papers over is visible
    (§ The ledger, *Platform keying*): `tui (linux): passed, tui (windows): failed`
    reads honestly where a single folded `tui: ...` could not say both."""
    scope = [NEST] if surface == NEST else list(APPS)
    seen = []
    for name in scope:
        by_platform = ((ledger.get(name) or {}).get(page.slug) or {}).get("tests", {}).get(test)
        if by_platform:
            for platform in sorted(by_platform):
                seen.append(f"{name} ({platform}): {by_platform[platform].get('outcome', '?')}")
    return ", ".join(seen) if seen else "—"


def _apply_status(text: str, block: str, path: Path) -> str:
    start = text.find(BEGIN)
    end = text.find(END)
    if start == -1 or end == -1 or end < start:
        raise CatalogError(
            f"{path.name}: the `## Status` section has no `{BEGIN}` / `{END}` fence "
            "— the render owns that block (feature-catalog.md § The page)")
    return text[:start] + BEGIN + "\n" + block + "\n" + text[end:]


def _matrix_json(pages: list, ledger: dict, releases: list, app_marks: dict | None = None,
                 declared: Declared | None = None, drafts: dict | None = None) -> dict:
    by_section: dict = {}
    for page in pages:
        by_section.setdefault(page.section, []).append(page)
    sections = []
    for section in sorted(by_section, key=lambda s: (_section_rank(s), s)):
        entries = []
        for page in by_section[section]:
            cells = _cells_for(page, ledger, app_marks, declared)
            words = (drafts or {}).get(page.slug, {})
            entries.append({
                "slug": page.slug,
                "title": words.get("title", page.title),
                "page": f"docs/features/{page.slug}.md",
                "goal": page.goal,
                "guide": page.guide,
                "what": words.get("what", page.what),
                "full": page.full,
                "nest_only": page.nest_only,
                "absences": dict(sorted(page.absences.items())),
                "outcome_absences": {app: {str(n): cite for n, cite in numbers.items()}
                                     for app, numbers in page.outcome_absences.items()},
                "cells": {app: {"state": c.state, "stamp": c.stamp, "date": c.date,
                                "dirty": c.dirty}
                          for app, c in cells.items()},
            })
        sections.append({"name": section, "features": entries})
    return {
        "schema": 1,
        "apps": list(APPS),
        # Additive: the kids channels' parents, so the site can group a kids column
        # with the app it is a build of (§ Cell semantics).
        "parents": dict(COLUMN_PARENT),
        "marks": dict(MARKS),
        "sections": sections,
        "releases": releases,
        "versions": released_versions(releases),
    }


def released_versions(releases: list) -> list:
    """The site's Releases table: one line per released version, in table order.

    The release table holds one row per BUILD, and a version's private verified builds
    carry its number as well as the build its candidate run tested — so listing rows
    listed "0.1.5" once per build. A version is released by its candidate run (the tag
    gate reads nothing else, release-table.md § Tag gate and release table), so only
    those rows count: the date is the version's newest candidate run, and it is in
    production once any of its builds was promoted."""
    by_version: dict = {}
    for row in releases:
        if not row.get("candidate_run"):
            continue
        line = by_version.setdefault(row["version"], {
            "version": row["version"], "date": "", "promoted": False})
        line["date"] = max(line["date"], row["candidate_run"])
        line["promoted"] = line["promoted"] or bool(row.get("promoted"))
    return list(by_version.values())


def draft_placeholders(features: Path, pages: list) -> dict:
    """`{slug: {"title": …, "what": …}}` — the placeholder for every page whose words are
    still a draft (§ Render, the placeholder paragraph; public-texts.md § Classes owns
    the draft record and the placeholder's shape).

    The record is the public-texts inventory beside the catalog's tree, read through
    `_public_texts.withheld_members` — the same reading the public sync's transform
    applies. Both are unshipped, so the import is lazy and a tree without either (the
    public one, a fixture world) has no drafts and renders every page verbatim."""
    root = features.parent.parent
    try:
        import _public_texts  # noqa: PLC0415 — unshipped; beside this module in the repo
    except ImportError:
        return {}
    if not (root / _public_texts.INVENTORY).is_file():
        return {}
    by_path = {}
    for page in pages:
        try:
            by_path[page.path.resolve().relative_to(root.resolve()).as_posix()] = page
        except ValueError:
            continue
    inv = _public_texts.load(root)
    return {by_path[rel].slug: {"title": _public_texts.draft_placeholder_title(rel),
                                "what": _public_texts.DRAFT_PLACEHOLDER_SENTENCE}
            for rel in _public_texts.withheld_members(inv, by_path)}


def render_all(features: Path = FEATURES, app_marks: dict | None = None,
               declared: Declared | None = None) -> dict:
    """`{relative path: content}` for everything the render owns. Pure; writes nothing.

    The app and surface markers are derived from the tree beside the catalog, and the
    registry is read from it, so a fixture world is narrowed by its OWN tests and
    registry exactly as the repo is by its — the render must not be a path that only
    the real repo exercises. A caller passing `app_marks` and no `declared` gets the
    registry with the map read from the ledger alone.
    """
    pages = load_pages(features)
    ledger = load_ledger(features)
    releases = load_releases(features)
    surface_marks: dict = {}
    if app_marks is None:
        marks, surface_marks, _platform_marks = scan_marks(features.parent.parent)
    else:
        marks = app_marks
    if declared is None:
        declared = load_declared(features, surface_marks, ledger)
    drafts = draft_placeholders(features, pages)
    out = {
        "MATRIX.md": _matrix_md(pages, ledger, releases, marks, declared, drafts),
        "matrix.json": json.dumps(_matrix_json(pages, ledger, releases, marks, declared,
                                               drafts),
                                  indent=2, sort_keys=True, ensure_ascii=False) + "\n",
    }
    for page in pages:
        out[page.path.name] = _apply_status(
            page.path.read_text(encoding="utf-8"),
            _status_block(page, ledger, marks, declared), page.path)
    return out


def write_all(features: Path = FEATURES) -> list:
    """Write the render; return the relative names whose content actually changed."""
    features.mkdir(parents=True, exist_ok=True)
    changed = []
    for name, content in sorted(render_all(features).items()):
        path = features / name
        if path.exists() and path.read_text(encoding="utf-8") == content:
            continue
        path.write_text(content, encoding="utf-8")
        changed.append(name)
    return changed


def check_all(features: Path = FEATURES) -> list:
    """The names whose committed content differs from a fresh render (rule 5)."""
    stale = []
    for name, content in sorted(render_all(features).items()):
        path = features / name
        if not path.exists() or path.read_text(encoding="utf-8") != content:
            stale.append(name)
    return stale

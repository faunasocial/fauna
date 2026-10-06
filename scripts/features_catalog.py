#!/usr/bin/env python3
"""The feature catalog's parse / compute / render core.

Owner of the *format*: `docs/goal/architecture/feature-catalog.md` — this module is
that document made executable, and every rule below cites the section it implements.
Two thin CLIs sit on top: `scripts/features-render.py` (writes) and
`scripts/features_lint.py` (the six rules of § The catalog lint).

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
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
FEATURES = REPO / "docs" / "features"

#: Column order, fixed here so the matrix, the JSON and every page's status block
#: agree. It is the order the seed matrix used.
APPS = ("web", "linux", "windows", "macos", "ios", "android", "tui")

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
    """
    marks = (app_marks or {}).get(node_id)
    return not marks or app in marks


def scan_app_marks(repo: Path = REPO) -> dict:
    """`{node id: (apps)}` for the whole e2e tree — the input `applicable` wants.

    Kept here rather than in the render so the ledger writer and the audit reach it
    the same way. Parse-only and ~2 s over the real tree, the same `ast` scan the
    lint already pays (§ Retiring the hand matrix: a merge-gated artifact is derived
    by a parse, never by a pytest collection, which differs per machine).
    """
    import sys
    scripts = str(Path(__file__).resolve().parent)
    if scripts not in sys.path:
        sys.path.insert(0, scripts)
    import features_scan
    scanned = features_scan.scan_tree(root=repo / "tests" / "e2e-unified", repo=repo)
    return {node_id: facts.apps for node_id, facts in scanned.items() if facts.apps}

#: The version-skew pin the release table derives (§ Tag gate and release table → *The
#: pin is derived from it*). Repo-relative because it lives OUTSIDE the catalog: it is
#: test-infrastructure data whose semantics are owned by version-compatibility.md
#: § Dimension 6, and all this module does is stop it being a second hand-kept copy of
#: a fact the table already holds.
PIN_REL = "tests/e2e-unified/pinned-previous-build.toml"

#: The immutable per-commit tag the release pipeline pushes (build-system.md § Image
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
    "none": "",
    "absent": "—",    # em dash
}

#: Outcomes that count as a witness having succeeded (§ Cell semantics).
PASSING = frozenset({"passed", "xpassed"})

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
        other apps. A blocked `[nest]` outcome is `(none)`, and blocks all seven
        columns alike, because a nest record is never narrowed. An outcome this
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
        to another — an unwitnessed one blocks all seven alike and belongs to whoever
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
    state: str            # full | partial | failing | none | absent
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

    return Page(
        path=path, slug=slug, title=front["title"], section=front["section"].lower(),
        goal=front["goal"], guide=front["guide"], absences=absences,
        what="\n".join(sections["What a user gets"][1]).strip(),
        stamp_date=stamp_date, stamp_commit=stamp_commit, outcomes=outcomes,
        outcome_absences=_check_outcome_absences(scoped, absences, outcomes, path))


#: A per-outcome `absences` key: `web (outcome 4)`, `ios (outcomes 4, 5)`.
_OUTCOME_ABSENCE_RE = re.compile(
    r"^(?P<app>[a-z]+)\s+\(outcomes?\s+(?P<nums>\d+(?:\s*,\s*\d+)*)\)$")


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
            if re.fullmatch(r"[a-z]+", key):
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


def load_ledger(features: Path = FEATURES) -> dict:
    """`{app: {slug: {"cell": {...}, "tests": {node id: {platform: record}}}}}`,
    missing files absent (§ The ledger, *Platform keying*: one test can carry more
    than one platform's outcome, so the render folds across them — never reads a
    bare record straight off `tests[node id]`)."""
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


def newest_promoted(releases: list) -> dict | None:
    """The release table's most recent **promoted** row — the pin's one source.

    "Promoted" and not merely "recorded" is the whole distinction § Dimension 6's
    update rule is built on: a candidate that went red at the verify gate never
    reached a user, and pinning it would aim the skew grid at an artifact nobody
    runs. Newest is by `date`, ties broken by position — the table is appended in
    run order, so the later row is the later run.
    """
    best = None
    for index, row in enumerate(releases):
        if not row.get("promoted"):
            continue
        key = (row.get("date") or "", index)
        if best is None or key > best[0]:
            best = (key, row)
    return best[1] if best else None


def pin_toml(row: dict) -> str:
    """`pinned-previous-build.toml`'s full body for one promoted release row.

    The file's *semantics* are unchanged and still owned by version-compatibility.md
    § Dimension 6; what changed in step 4 is only that it is derived rather than
    hand-copied. The header says so, because the next person to meet this file will
    otherwise edit it — its previous header told them to.
    """
    commit = row.get("commit") or ""
    image = row.get("image") or f"{NEST_IMAGE_REPO}:sha-{commit}"
    released = row.get("date") or ""
    return f"""# Pinned previous build — the most recent PRODUCTION release, as a repo constant.
#
# GENERATED by `just features-render` from the feature catalog's release table — its
# newest `promoted: true` row. Do not edit by hand: `just release-promoted <commit>`
# is the act that advances it, and `just features-lint` rule 5 reds if this file
# drifts from the table. Owner of the table: docs/goal/architecture/feature-catalog.md
# § Tag gate and release table.
#
# Convention owner (semantics — unchanged by the derivation):
# docs/goal/architecture/version-compatibility.md § Dimension 6. Consumed by the
# tier_3 real-binary version-skew + client at-rest upgrade-in-place tests.
#
# "Previous build" = the most recent production release: a build that was *promoted*
# to `:latest`, not merely dispatched (build-system.md § Image tags & channels).
# `commit` is that build's headSha, `nest_image` its immutable ghcr tag. Previous
# CLIENT binaries are rebuilt from source at `commit` (no client release artifacts
# exist pre-1.0), cached under /work/tmp/fauna-prev-builds/<commit>/<profile>/ on the
# Linux dev machine.
#
# Advancing the pin past a ratified in-place compat break needs no further edit: the
# grid's expected-incompatible windows are keyed on this commit's ancestry
# (helpers/prev_build.py::RATIFIED_INPLACE_BREAKS), so the cells re-assert on their
# own. The publish gate's `secret_scan` entry for the new 40-hex run is landed by
# `just release-promoted` in the same act, so a pin advance never reds the merge.

commit = "{commit}"
nest_image = "{image}"
released = "{released}"
"""


def render_pin(features: Path = FEATURES) -> str | None:
    """The pin file's derived body, or `None` while no promoted row exists.

    `None` is not "empty" — it means the table cannot yet answer the question, and
    the committed pin (hand-written since 2026-07-12) stands untouched. The render
    takes the file over on the first promotion and never gives it back.
    """
    row = newest_promoted(load_releases(features))
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
    `compute_cell` reads only `outcome` off the result."""
    non_passing = [r for r in by_platform.values() if r.get("outcome") not in PASSING]
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


def surface_view(page: Page, ledger: dict, surface: str, app: str,
                 app_marks: dict | None = None) -> tuple:
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
    is where cross-platform disagreement becomes a mixed outcome.
    """
    tests = (page.surface_tests(surface, app, app_marks) if surface == "app"
             else page.surface_tests(surface))
    if not tests:
        return False, [], {}, None
    entry = _ledger_entry(page, ledger, surface, app)
    recorded = entry.get("tests") or {}
    return True, tests, {t: _fold_platform_records(recorded[t]) for t in tests if t in recorded}, \
        entry.get("cell")


def compute_cell(page: Page, app: str, ledger: dict, app_marks: dict | None = None) -> Cell:
    """The mark for one (feature, app column), computed — never typed (§ Cell semantics).

    `app_marks` (`features_catalog.app_marks()`) is what makes the column's scope its
    own; without it every witness reads as applicable to every column, which is the
    honest answer for a caller that has no marker knowledge — and the answer for an
    unmarked test either way.
    """
    if app in page.absences:
        return Cell("absent", "", "")

    app_part, app_tests, app_records, app_cell = surface_view(
        page, ledger, "app", app, app_marks)
    nest_part, nest_tests, nest_records, nest_cell = surface_view(
        page, ledger, NEST, app, app_marks)

    if not (app_part or nest_part):
        return Cell("none", "", "")
    # The nest half is a conjunct on every column: a participating surface with no
    # record leaves the cell empty however green the other half is.
    if (app_part and not app_records) or (nest_part and not nest_records):
        return Cell("none", "", "")

    records = list(app_records.values()) + list(nest_records.values())
    mapped = len(app_tests) + len(nest_tests)
    passed = [r for r in records if r.get("outcome") in PASSING]

    stamp, date, dirty = _older_stamp(app_cell, nest_cell)
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
outcome no test witnesses yet, or the mapped tests are mixed &middot;
{MARKS['failing']} tests witness this feature and none of them passed &middot;
*(blank)* our records hold no run for this feature and app &middot; {MARKS['absent']}
the feature is absent on that app by design, and the page says where that is written.

Each cell carries the stamp of the nest artifact its tests were witnessed against (where
one complete run spanned two artifacts, the older of them): `<version> <nest mode>` means
the released artifact carrying that version, and `<version>-dev+<commit> <nest mode>`
means a build of that commit. `.dirty` after the commit means the run's working tree
carried uncommitted changes when it was collected, so that commit is a lower bound on the
code it saw — the next run from a clean tree supersedes it.
"""


def _cells_for(page: Page, ledger: dict, app_marks: dict | None = None) -> dict:
    return {app: compute_cell(page, app, ledger, app_marks) for app in APPS}


def _matrix_md(pages: list, ledger: dict, releases: list, app_marks: dict | None = None) -> str:
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
            out.append(_matrix_row(page, ledger, app_marks))
        out.append("")
    out.append("## Releases")
    out.append("")
    if releases:
        out.append(_RELEASE_LEGEND)
        out.append("")
        out.append("| Version | Commit | Image digest | Date | Promoted | Apps |")
        out.append("|---|---|---|---|---|---|")
        for row in releases:
            out.append("| {} | `{}` | `{}` | {} | {} | {} |".format(
                row.get("version", ""), (row.get("commit") or "")[:12],
                row.get("image_digest", ""), row.get("date", ""),
                "yes" if row.get("promoted") else "no",
                _shipped_apps(row)))
    else:
        out.append("No release-candidate run has been recorded yet.")
    out.append("")
    return "\n".join(out) + "\n"


#: § Release train. The train is the default and needs no per-row prose; what a reader
#: has to be told is what a *non-empty* Apps column means, because that is the store
#: reship carve-out becoming visible rather than being inferred from a store's history.
_RELEASE_LEGEND = (
    "One row per release-candidate run. **Promoted** is the one that became "
    "production. A release is a train — the nest and all seven apps ship together at "
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


def _matrix_row(page: Page, ledger: dict, app_marks: dict | None = None) -> str:
    label = f"[{page.title}]({page.slug}.md)"
    if page.nest_only:
        cell = compute_cell(page, APPS[0], ledger, app_marks)
        body = _cell_text(cell) + " (the nest — every app)"
        return f"| {label} | " + " | ".join([body] + [""] * (len(APPS) - 1)) + " |"
    cells = _cells_for(page, ledger, app_marks)
    return f"| {label} | " + " | ".join(_cell_text(cells[app]) for app in APPS) + " |"


def _cell_text(cell: Cell) -> str:
    if cell.state == "none":
        return ""
    return f"{cell.mark} {cell.stamp}".strip()


def _status_block(page: Page, ledger: dict, app_marks: dict | None = None) -> str:
    lines = ["| App | Status | Stamp |", "|---|---|---|"]
    # The same per-column scope the matrix row reads — without the marks every
    # column is judged on all seven apps' witnesses and the two disagree.
    cells = _cells_for(page, ledger, app_marks)
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
                       else f"`{test}`")
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


def _matrix_json(pages: list, ledger: dict, releases: list, app_marks: dict | None = None) -> dict:
    by_section: dict = {}
    for page in pages:
        by_section.setdefault(page.section, []).append(page)
    sections = []
    for section in sorted(by_section, key=lambda s: (_section_rank(s), s)):
        entries = []
        for page in by_section[section]:
            cells = _cells_for(page, ledger, app_marks)
            entries.append({
                "slug": page.slug,
                "title": page.title,
                "page": f"docs/features/{page.slug}.md",
                "goal": page.goal,
                "guide": page.guide,
                "what": page.what,
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
        "marks": dict(MARKS),
        "sections": sections,
        "releases": releases,
    }


def render_all(features: Path = FEATURES, app_marks: dict | None = None) -> dict:
    """`{relative path: content}` for everything the render owns. Pure; writes nothing.

    The app markers are derived from the tree beside the catalog, so a fixture world
    is narrowed by its OWN tests exactly as the repo is by its — the render must not
    be a path that only the real repo exercises.
    """
    pages = load_pages(features)
    ledger = load_ledger(features)
    releases = load_releases(features)
    marks = scan_app_marks(features.parent.parent) if app_marks is None else app_marks
    out = {
        "MATRIX.md": _matrix_md(pages, ledger, releases, marks),
        "matrix.json": json.dumps(_matrix_json(pages, ledger, releases, marks),
                                  indent=2, sort_keys=True, ensure_ascii=False) + "\n",
    }
    for page in pages:
        out[page.path.name] = _apply_status(
            page.path.read_text(encoding="utf-8"), _status_block(page, ledger, marks), page.path)
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

#!/usr/bin/env python3
"""Render the feature catalog: `docs/features/MATRIX.md`, every page's `## Status`
block, `docs/features/matrix.json`, and — once the release table records a promoted
release — `tests/e2e-unified/pinned-previous-build.toml`, which stops being a second
hand-kept copy of a fact the table already holds (§ Tag gate and release table).

`docs/goal/architecture/feature-catalog.md` § Render owns the contract. The render is
**idempotent** and not `build-if-stale`-gated, unlike every other generator in the
tree: a page is both a source of the render and the target of its own `## Status`
block, so an mtime gate cannot express the dependency. Instead it is sub-second and
runs unconditionally, and `just features-lint` rule 5 checks the committed output
equals a fresh render — the same shape as the `i18n-generate` / `i18n-check` split.

  just features-render            # write
  just features-render --check    # report staleness, write nothing (exit 1 if stale)
  just features-parity            # why each column is short of full; writes nothing
  just features-parity --column tui   # what ONE column is short of, for routing
  just features-parity --column tui --max-reach 4   # that column's trickle-down batch
  just features-parity --by-column    # every column's backlog, one table
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import features_catalog as fc


def _witness_columns(outcome, app_marks) -> str:
    """Which columns an outcome's citations can ever speak for — its `blocking` seen
    from the other side. `(none)` and a nest outcome are named as themselves: neither
    is a per-column fact, and printing six column names for either would read as one."""
    if not outcome.tests:
        return "(none)"
    if outcome.surface == fc.NEST:
        return "[nest]"
    cols = [a for a in fc.APPS if any(fc.applicable(t, a, app_marks) for t in outcome.tests)]
    return ",".join(sorted(cols)) if cols else "(none)"


def _parity(features: Path, slug: str | None) -> int:
    """§ Cell semantics, read as a question about the CONTRACT rather than the run.

    Fullness is per column, so a column short of full is short of it for a nameable
    reason — an outcome no test that can speak for that column witnesses. This asks
    only the parse: which columns *could* a run ever complete, on any machine, in any
    mode? A page full on no column at all is the sharpest answer the catalog gives —
    no run anywhere can stamp it, so the blank cell is a contract fact rather than a
    coverage one, and no amount of running tests will change it.
    """
    pages = fc.load_pages(features)
    if slug:
        pages = [p for p in pages if p.slug == slug]
        if not pages:
            print(f"features-render: no page `{slug}` in {features}", file=sys.stderr)
            return 1
    app_marks = fc.scan_app_marks()

    nest_only, every, some, none_at_all = [], [], [], []
    for page in pages:
        if page.nest_only:
            nest_only.append(page)
            continue
        cols = page.completable(app_marks)
        # A page's own columns exclude its declared absences — an `—` column is
        # neither completable nor short, so "full on all" means all it has.
        own = [a for a in fc.APPS if a not in page.absences]
        (every if len(cols) == len(own) else some if cols else none_at_all).append(
            (page, cols))

    print("feature catalog — per-column contract fullness (parse only, no run)\n")
    print(f"{len(pages)} page(s): {len(nest_only)} nest-only · {len(every)} full on every "
          f"column they have · {len(some)} full on some · {len(none_at_all)} full on NO column\n")

    def detail(page, cols):
        blocked = [a for a in fc.APPS if a not in cols and a not in page.absences]
        absent = [a for a in fc.APPS if a in page.absences]
        print(f"  {page.slug}")
        print(f"      full on: {','.join(cols) if cols else '(no column)'}"
              f"   short on: {','.join(blocked)}"
              + (f"   absent by design: {','.join(absent)}" if absent else ""))
        for outcome in page.outcomes:
            excused = [a for a in fc.APPS if page.absent_from(a, outcome)]
            if excused:
                print(f"      {outcome.number:>2} absent by design on: {','.join(excused)}")
        # Every outcome that blocks ANY column, not just the first one's — a page
        # short on all seven is usually short for a DIFFERENT outcome per column,
        # and printing one column's blockers would read as the whole story.
        blocks_per_outcome = {
            o.number: [a for a in fc.APPS
                       if o.number in {b.number for b in page.blocking(a, app_marks)}]
            for o in page.outcomes}
        for outcome in page.outcomes:
            if not blocks_per_outcome[outcome.number]:
                continue
            print(f"      {outcome.number:>2} [{outcome.surface}] {outcome.text[:72]}")
            print(f"          witnessed on: {_witness_columns(outcome, app_marks)}")

    if none_at_all:
        print(f"FULL ON NO COLUMN ({len(none_at_all)}) — no run, on any machine, in any "
              "mode, can\ncomplete these: the contract itself is what blanks them.\n")
        for page, cols in none_at_all:
            detail(page, cols)
        print()
    if some and not slug:
        print(f"FULL ON SOME COLUMN ({len(some)}) — the blocked columns are the "
              "cross-app parity\nbacklog, computed rather than surveyed:\n")
        for page, cols in some:
            print(f"  {page.slug:<34} full on {','.join(cols)}")
    elif some:
        for page, cols in some:
            detail(page, cols)
    return 0


def _column_detail(features: Path, column: str) -> int:
    """The parity view transposed: what ONE column is short of, across every page.

    `_parity` is page-major, which is what a reader of one page wants. Routing the
    backlog wants the transpose, because a trickle-down pass and the queue that
    receives its capture are both per app (`testing.md` § Default app and nest
    mode). The split
    it prints is the routing decision itself: an outcome a sibling column already
    witnesses is a **lift** this column owes, with the reference named; an outcome no
    column witnesses is an unwritten test owned by the feature's area, which no app
    queue can close by lifting anything.
    """
    if column not in fc.APPS:
        print(f"features-render: `{column}` is not a column; expected one of "
              f"{', '.join(fc.APPS)}", file=sys.stderr)
        return 1
    pages = fc.load_pages(features)
    app_marks = fc.scan_app_marks()
    blockers = fc.column_backlog(pages, column, app_marks)

    owed = [b for b in blockers if b.owed_here]
    nobody = [b for b in blockers if not b.owed_here]
    short_pages = {b.page.slug for b in blockers}
    print(f"feature catalog — what column `{column}` is short of (parse only, no run)\n")
    print(f"{column}: short on {len(short_pages)} of {len(pages)} page(s) · "
          f"{len(owed)} outcome(s) a sibling column already witnesses · "
          f"{len(nobody)} witnessed by nobody\n")

    def group(items, heading, blurb):
        if not items:
            return
        by_page = {}
        for b in items:
            by_page.setdefault(b.page.slug, []).append(b)
        print(f"{heading} ({len(by_page)} page(s), {len(items)} outcome(s)) — {blurb}\n")
        for slug, group_blockers in by_page.items():
            print(f"  {slug}")
            for b in group_blockers:
                print(f"      {b.outcome.number:>2} [{b.outcome.surface}] "
                      f"{b.outcome.text[:68]}")
                if b.witnesses:
                    print(f"          lift from: {','.join(b.witnesses)}")
        print()

    group(owed, "OWED HERE",
          f"a sibling column already has the witness, so the\nrepair is a lift into "
          f"{column} and `lift from` names the reference.")
    group(nobody, "WITNESSED BY NOBODY",
          "no column witnesses these, so they are unwritten\ntests owned by each "
          "feature's area — never this column's parity debt.")
    return 0


def _column_trickle_down(features: Path, column: str, max_reach: int) -> int:
    """One app queue's batch: the gaps `column` owes whose reach is at most the cap.

    `--column` alone lists everything the column is short of, wide gaps included, and
    a per-app row that derived its scope from that would do a cross-app row's work
    once per short column. This is the list such a row re-measures its premise with:
    every line is a lift into this column, the reference columns named.
    """
    if column not in fc.APPS:
        print(f"features-render: `{column}` is not a column; expected one of "
              f"{', '.join(fc.APPS)}", file=sys.stderr)
        return 1
    pages = fc.load_pages(features)
    gaps = fc.column_trickle_down(pages, column, max_reach, fc.scan_app_marks())
    print(f"feature catalog — `{column}`'s trickle-down batch at reach <= {max_reach} "
          f"(parse only, no run)\n")
    print(f"{column}: {len(gaps)} gap(s) on {len({g.page.slug for g in gaps})} page(s) "
          f"— each a lift, reference columns named; a missing witness very often\n"
          f"hides a missing implementation, so check the app before writing the test.\n")
    for g in gaps:
        print(f"  {g.page.slug} {g.outcome.number} (reach {g.reach}) — lift from "
              f"{','.join(g.witnesses)}\n      {g.outcome.text[:90]}")
    return 0


def _column_summary(features: Path) -> int:
    """One routing table: every column's backlog, and the reach that routes it.

    The per-column counts alone would route the backlog wrong. A column's debt is the
    right unit for DOING the work and the wrong one for MOVING it: an outcome
    witnessed on a single app is short on six columns, so sending each column's list
    to its own app queue mints that one piece of work six times — the per-feature × 6
    shape `testing.md` § Default app and nest mode refuses. So this prints both units.
    `Gap.reach`
    splits each column's debt into **stragglers** (reach 1-2: this column is genuinely
    behind its siblings, ordinary trickle-down for its queue) and **wide** gaps
    (reach 5+: almost nothing has the witness, so they are one cross-app row each, not
    six per-app ones).
    """
    pages = fc.load_pages(features)
    app_marks = fc.scan_app_marks()
    gaps = fc.gap_reach(pages, app_marks)
    reach_of = {k: g.reach for k, g in gaps.items()}

    print("feature catalog — per-column backlog and its reach (parse only, no run)\n")
    print(f"{'column':<9}{'short pages':>12}{'owed':>7}{'straggler':>11}{'wide':>7}"
          f"{'nobody':>8}{'absent':>8}")
    for app in fc.APPS:
        blockers = fc.column_backlog(pages, app, app_marks)
        owed = [b for b in blockers if b.owed_here]
        reaches = [reach_of[(b.page.slug, b.outcome.number)] for b in owed]
        print(f"{app:<9}{len({b.page.slug for b in blockers}):>12}{len(owed):>7}"
              f"{sum(1 for r in reaches if r <= 2):>11}"
              f"{sum(1 for r in reaches if r >= 5):>7}"
              f"{len(blockers) - len(owed):>8}"
              f"{sum(1 for pg in pages if app in pg.absences):>8}")

    slots = sum(g.reach for g in gaps.values())
    print(f"\n{len(gaps)} distinct gap(s) cause {slots} column-slot(s) of debt — by reach:\n")
    for r in range(1, len(fc.APPS)):
        at = [g for g in gaps.values() if g.reach == r]
        if at:
            print(f"  reach {r}: {len(at):>3} gap(s), {sum(g.reach for g in at):>3} slot(s)")

    widest = sorted(gaps.values(), key=lambda g: (-g.reach, g.page.slug, g.outcome.number))
    wide = [g for g in widest if g.reach >= 5]
    if wide:
        print(f"\nWIDEST ({len(wide)} gap(s), {sum(g.reach for g in wide)} slots) — almost no "
              "column has the witness, so each is\nONE cross-app row, never one row per "
              "short column:\n")
        for g in wide:
            print(f"  {g.page.slug} {g.outcome.number}: witnessed on "
                  f"{','.join(g.witnesses)} — short on {g.reach}")

    print("\n`owed` counts outcomes a sibling column already witnesses; `nobody` counts "
          "outcomes\nno column witnesses — unwritten tests owned by each feature's area, "
          "never parity\nwork. Per column:\n\n  just features-parity --column <name>")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true",
                        help="report what a fresh render would change; write nothing")
    parser.add_argument("--column", metavar="APP",
                        help="the parity view transposed: what one column is short "
                             "of, split into what it owes and what nobody witnesses")
    parser.add_argument("--max-reach", type=int, metavar="N",
                        help="with --column: only the gaps that column owes whose "
                             "reach is at most N — one app queue's trickle-down batch")
    parser.add_argument("--by-column", action="store_true",
                        help="every column's backlog as one routing table")
    parser.add_argument("--parity", nargs="?", const="", metavar="SLUG",
                        help="report which columns each contract can ever complete, and "
                             "the outcomes that block the rest; write nothing. With a "
                             "SLUG, that one page in full")
    parser.add_argument("--features", type=Path, default=fc.FEATURES,
                        help="the catalog directory (default docs/features/)")
    parser.add_argument("--repo", type=Path, default=fc.REPO,
                        help="repo root, for the derived version-skew pin "
                             "(default: this checkout)")
    args = parser.parse_args(argv)

    try:
        if args.column and args.max_reach is not None:
            return _column_trickle_down(args.features, args.column, args.max_reach)
        if args.column:
            return _column_detail(args.features, args.column)
        if args.by_column:
            return _column_summary(args.features)
        if args.parity is not None:
            return _parity(args.features, args.parity or None)
        if args.check:
            stale = [f"docs/features/{name}" for name in fc.check_all(args.features)]
            pin = fc.check_pin(args.features, args.repo)
            if pin:
                stale.append(pin)
            if not stale:
                print("features-render: up to date")
                return 0
            print(f"features-render: {len(stale)} file(s) stale", file=sys.stderr)
            for name in stale:
                print(f"  {name}", file=sys.stderr)
            print("\nRun `just features-render` and commit the result.", file=sys.stderr)
            return 1

        changed = [f"docs/features/{name}" for name in fc.write_all(args.features)]
        pin = fc.write_pin(args.features, args.repo)
        if pin:
            changed.append(pin)
        if changed:
            print(f"features-render: wrote {len(changed)} file(s)")
            for name in changed:
                print(f"  {name}")
        else:
            print("features-render: up to date")
        return 0
    except fc.CatalogError as exc:
        print(f"features-render: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

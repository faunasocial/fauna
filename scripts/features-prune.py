#!/usr/bin/env python3
"""Prune the feature ledger: remove every record whose test its feature's contract no
longer maps (re-marked, renamed, deleted, or filed under a feature whose page is gone),
every record on a platform its app's column can no longer run on (web on windows) — in
the nest file, a stamp on a platform that cannot complete the feature's nest set, or a
`tests/platform/docker/` witness's record on a platform with no docker daemon — and
every `skipped: unbuilt` record whose `surface` the surface registry now lists as built
on its column (§ Cell semantics, *Staleness is decided by the registry*) — and re-fold
every nest cell whose top level is not the fold of its platforms over the feature's
legs (§ The ledger, *Stale records*, ruling (h)).

`docs/goal/architecture/feature-catalog.md` § The ledger, *Stale records* owns the rule;
`just features-lint` rule 6 refuses a ledger that holds one. The run-time writer never
prunes — a run sees only what its own machine collects.

  just features-prune            # remove them, print what went
  just features-prune --check    # report them, write nothing (exit 1 if any)
  just features-prune --unbuilt-without-surface
                                 # also drop every `skipped: unbuilt` record with no
                                 # `surface` key — the surface-registry migration's
                                 # one-time last step; a gate that still fires on a
                                 # column re-records there with its key
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import features_catalog as fc


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true",
                        help="report stale records, write nothing (exit 1 if any)")
    parser.add_argument("--unbuilt-without-surface", action="store_true",
                        help="also drop every `skipped: unbuilt` record that names no "
                        "surface (the surface-registry migration's one-time last step)")
    parser.add_argument("--features", type=Path, default=fc.FEATURES)
    args = parser.parse_args(argv)
    try:
        # The files as written — what the prune rewrites and lint rule 6 judges — never
        # the two-source reading, whose top levels the last-green overlay can move.
        ledger = fc.load_ledger(args.features, last_green=False)
        pages = fc.load_pages(args.features)
        marks = fc.scan_marks(args.features.parent.parent)
        app_marks, _surfaces, platform_marks = marks
        # A cell whose top level is not the fold of its platforms over its surface's
        # legs (§ The ledger, *Stale records*, rulings (h) and (i)) is re-folded, not
        # dropped — reported beside the stale records, under its own verb.
        misfolded = fc.misfolded_cells(pages, ledger, app_marks, platform_marks)
        if args.check:
            stale = fc.stale_records(pages, ledger,
                                     fc.load_surface_registry(fc.surfaces_path(args.features)),
                                     app_marks, platform_marks)
            if args.unbuilt_without_surface:
                stale += [r for r in fc.surfaceless_unbuilt_records(ledger) if r not in set(stale)]
                stale.sort(key=lambda r: (r[0], r[1], r[2] or "", r[3] or ""))
        else:
            stale = fc.prune_ledger(args.features,
                                    unbuilt_without_surface=args.unbuilt_without_surface,
                                    marks=marks)
    except fc.CatalogError as exc:
        print(f"features-prune: {exc}", file=sys.stderr)
        return 2
    for app, slug, node_id, platform in stale:
        print(f"{app}\t{slug}\t{node_id or '(cell)'}\t{platform or '(every platform)'}")
    for app, slug in misfolded:
        print(f"{app}\t{slug}\t(cell)\t(re-folded over its legs)")
    verb = "found" if args.check else "removed"
    print(f"features-prune: {verb} {len(stale)} stale record(s)"
          + (f", {len(misfolded)} cell(s) to re-fold" if misfolded else ""), file=sys.stderr)
    if (stale or misfolded) and not args.check:
        print("run `just features-render` and commit the ledger with its render", file=sys.stderr)
    return 1 if ((stale or misfolded) and args.check) else 0


if __name__ == "__main__":
    raise SystemExit(main())

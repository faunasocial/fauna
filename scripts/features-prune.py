#!/usr/bin/env python3
"""Prune the feature ledger: remove every record whose test its feature's contract no
longer maps (re-marked, renamed, deleted, or filed under a feature whose page is gone).

`docs/goal/architecture/feature-catalog.md` § The ledger, *Stale records* owns the rule;
`just features-lint` rule 6 refuses a ledger that holds one. The run-time writer never
prunes — a run sees only what its own machine collects.

  just features-prune            # remove them, print what went
  just features-prune --check    # report them, write nothing (exit 1 if any)
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
    parser.add_argument("--features", type=Path, default=fc.FEATURES)
    args = parser.parse_args(argv)
    try:
        if args.check:
            stale = fc.stale_records(fc.load_pages(args.features), fc.load_ledger(args.features))
        else:
            stale = fc.prune_ledger(args.features)
    except fc.CatalogError as exc:
        print(f"features-prune: {exc}", file=sys.stderr)
        return 2
    for app, slug, node_id in stale:
        print(f"{app}\t{slug}\t{node_id}")
    verb = "found" if args.check else "removed"
    print(f"features-prune: {verb} {len(stale)} stale record(s)", file=sys.stderr)
    if stale and not args.check:
        print("run `just features-render` and commit the ledger with its render", file=sys.stderr)
    return 1 if (stale and args.check) else 0


if __name__ == "__main__":
    raise SystemExit(main())

"""Write one store submission's state into installer/store-review.toml.

The record's one writer (docs/goal/architecture/installers/store-review.md
§ The record → the writing rule). Usage:

  just store-review <store> <version> <state> --read YYYY-MM-DD
      [--track <t>] [--build <b>] [--flavour <f>] [--since YYYY-MM-DD] [--note "…"]
  just store-review --check

The first write of a (store, version) needs --track, --build, --flavour and
--note; a later one replaces state, since, read and note in place. --since
defaults to --read when the state changes, and stays put when it does not.
Writes the file and stops: the commit is the session's own.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import sys

import _store_review as sr


def _date(text: str) -> _dt.date:
    try:
        return _dt.date.fromisoformat(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} is not a YYYY-MM-DD date") from None


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="just store-review", description=__doc__.split("\n\n")[0])
    ap.add_argument("store", nargs="?", choices=sr.STORES)
    ap.add_argument("version", nargs="?")
    ap.add_argument("state", nargs="?", choices=sr.STATES)
    ap.add_argument("--read", type=_date, help="the date of the console reading")
    ap.add_argument("--since", type=_date, help="the date the state was entered (default: --read on a change)")
    ap.add_argument("--track")
    ap.add_argument("--build")
    ap.add_argument("--flavour", choices=sr.FLAVOURS)
    ap.add_argument("--note")
    ap.add_argument("--check", action="store_true", help="only verify the record; write nothing")
    args = ap.parse_args(argv)

    text = sr.RECORD.read_text()
    if args.check:
        errors = sr.check_text(text)
        for e in errors:
            print(f"store-review: {e}", file=sys.stderr)
        if not errors:
            print(f"store-review: {sr.RECORD.relative_to(sr.REPO)} OK")
        return 1 if errors else 0

    if not (args.store and args.version and args.state and args.read):
        ap.error("<store> <version> <state> and --read are required (or --check)")
    errors = sr.check_text(text)
    if errors:
        for e in errors:
            print(f"store-review: refusing to write over a broken record: {e}", file=sys.stderr)
        return 1
    try:
        subs, verb = sr.apply(
            sr.load(),
            store=args.store,
            version=args.version,
            state=args.state,
            read=args.read,
            since=args.since,
            track=args.track,
            build=args.build,
            flavour=args.flavour,
            note=args.note,
        )
    except sr.RecordError as err:
        print(f"store-review: refused: {err}", file=sys.stderr)
        return 1
    sr.RECORD.write_text(sr.render(subs))
    sub = next(s for s in subs if s["store"] == args.store and s["version"] == args.version)
    print(
        f"store-review: {verb} {sub['store']} {sub['version']} ({sub['track']}, build {sub['build']}): "
        f"{sub['state']} since {sub['since']}, read {sub['read']}. Commit it."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

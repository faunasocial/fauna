"""Write one store submission's state into installer/store-review.toml.

The record's one writer (docs/goal/architecture/installers/store-review.md
§ The record → the writing rule). Usage:

  just store-review <store> <version> <state> --read YYYY-MM-DD
      [--track <t>] [--build <b>] [--flavour <f>] [--since YYYY-MM-DD] [--note "…"]
      [--at YYYY-MM-DDTHH:MMZ]
  just store-review --check

The first write of a (store, version) needs --track, --build, --flavour and
--note; a later one replaces state, since, read and note in place. --since
defaults to --read when the state changes, and stays put when it does not.
In the same act it writes the submission's artifacts into the release table
(docs/features/ledger/releases.json `artifacts`; release-table.md § Tag gate and
release table, *The artifacts of a version*): in-review, approved or rejected →
submitted, serving on the store's public track → live, withdrawn → withdrawn,
built, uploaded or serving a testing track → built, superseded → nothing.
A submit (in-review) and an available (serving on the public track) are also
stamped to the MINUTE on the version's last release attempt
(supply-chain/attempts/<version>.toml; product-version.md § Bumping, the 24-hour
paragraph, which `fauna-publish attempt review` reads): --at is the minute the
state was entered (`stated`); without it a state this write changes is stamped
now, the minute it was read (`read`, an upper bound). The first stamp stands.
Writes the files and stops: the commit is the session's own.
"""
from __future__ import annotations

import argparse
import datetime as _dt
import importlib.util
import re
import sys
from pathlib import Path

import _store_review as sr

try:  # The release table is the private tree's (its ledger never ships): absent, no artifact act.
    import release_artifacts as ra
except ImportError:
    ra = None


def _date(text: str) -> _dt.date:
    try:
        return _dt.date.fromisoformat(text)
    except ValueError:
        raise argparse.ArgumentTypeError(f"{text!r} is not a YYYY-MM-DD date") from None


def _minute(text: str) -> str:
    m = re.fullmatch(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2})(:\d{2})?Z", text)
    if not m:
        raise argparse.ArgumentTypeError(f"{text!r} is not a YYYY-MM-DDTHH:MMZ minute (UTC)")
    return f"{m.group(1)}{m.group(2) or ':00'}Z"


def _attempt_module():
    """The release attempt's record tool beside this script, or None where the tree carries none."""
    path = Path(__file__).resolve().parent / "publish" / "release_attempt.py"
    if not path.exists():
        return None
    spec = importlib.util.spec_from_file_location("release_attempt", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


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
    ap.add_argument("--at", type=_minute,
                    help="the minute (UTC) the state was entered, stamped on the release attempt")
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
    before = next((s["state"] for s in sr.loads(text)
                   if s["store"] == args.store and s["version"] == args.version), None)
    try:
        subs, verb = sr.apply(
            sr.loads(text),
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
    sub = next(s for s in subs if s["store"] == args.store and s["version"] == args.version)
    # The release table's artifacts list, in the same act (release-table.md § Tag gate and
    # release table, *The artifacts of a version*): both are computed before either is written.
    acts: list[str] = []
    if ra is not None:
        try:
            releases, artifacts = ra.load(ra.TABLE)
            acts = ra.record_store_submission(artifacts, sub)
        except (ra.Refused, ValueError) as err:
            print(f"store-review: refused (the release table's artifacts): {err}", file=sys.stderr)
            return 1
    sr.RECORD.write_text(sr.render(subs))
    if ra is not None:
        ra.save(releases, artifacts, ra.TABLE)
    print(
        f"store-review: {verb} {sub['store']} {sub['version']} ({sub['track']}, build {sub['build']}): "
        f"{sub['state']} since {sub['since']}, read {sub['read']}."
    )
    for act in acts:
        print(f"store-review: artifacts: {act}")
    written = [sr.RECORD.relative_to(sr.REPO)] + ([ra.TABLE.relative_to(ra.REPO)] if ra else [])
    # The minute stamp on the release attempt (product-version.md § Bumping, the 24-hour
    # paragraph): stated with --at, else read now when this write changed the state.
    state = sr.artifact_state(sub)
    mod = _attempt_module() if (args.at or before != sub["state"]) else None
    if mod is not None and state in mod.STORE_STAMP_STATES:
        root = sr.REPO / "supply-chain" / "attempts"
        stamps = mod.stamp_store(args.version, args.store, list(sr.STORE_ARTIFACTS[args.store]),
                                 state, at=args.at, root=root)
        for r in stamps:
            print(f"store-review: attempt: {r['artifact']} {r['state']} at {r['at']} ({r['how']})")
        if stamps:
            written.append(mod.path_for(args.version, root).relative_to(sr.REPO))
    print(f"store-review: commit {' and '.join(map(str, written))} together.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

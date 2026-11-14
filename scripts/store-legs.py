"""The store legs' shared command line (docs/goal/architecture/installers/store-review.md § The store legs).

  store-legs.py text <store> <name> <version>   print the text a leg sends, or refuse (exit 1)
  store-legs.py furthest <store>                print `testing` or `public`
  store-legs.py add <artifact> <legs|->         validate each leg (a JSON object per line) and append it to store-legs.json

A refusal prints one `store-legs: <what> FAILED — <why>` line on stderr and
exits 1; a usage error exits 2. The grammar is `scripts/_store_legs.py`'s.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import _store_legs as sl  # noqa: E402

USAGE = __doc__.split("\n\n")[1]


def main(argv: list[str]) -> int:
    if not argv:
        print(USAGE, file=sys.stderr)
        return 2
    cmd, rest = argv[0], argv[1:]
    try:
        if cmd == "text" and len(rest) == 3:
            sys.stdout.write(sl.read_text(*rest) + "\n")
            return 0
        if cmd == "furthest" and len(rest) == 1:
            print(sl.furthest(rest[0]))
            return 0
        if cmd == "add" and len(rest) == 2:
            raw = sys.stdin.read() if rest[1] == "-" else Path(rest[1]).read_text(encoding="utf-8")
            try:
                legs = [json.loads(line) for line in raw.splitlines() if line.strip()]
            except json.JSONDecodeError as exc:
                raise sl.LegError(f"a leg is not JSON: {exc}") from exc
            for leg in legs:  # validate all before appending any
                bad = sl.leg_problems(leg)
                if bad:
                    raise sl.LegError("the leg is refused: " + "; ".join(bad))
            data = sl.load_artifact(Path(rest[0]))
            for leg in legs:
                data = sl.add_leg(Path(rest[0]), leg)
                print(f"store-legs: add OK — {leg['store']} {leg['version']} {leg['state']} on {leg['track']}")
            print(f"store-legs: {len(data['legs'])} leg(s) in {rest[0]}")
            return 0
    except sl.LegError as exc:
        print(f"store-legs: {cmd} FAILED — {exc}", file=sys.stderr)
        return 1
    print(USAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

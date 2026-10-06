#!/usr/bin/env python3
"""check_dagcbor_bypass_ratchet.py — pins the set of production sites that
decode dag-cbor bytes via raw `serde_ipld_dagcbor::from_slice`/`from_reader`
instead of `fauna-cbor`'s pre-parse validator
(`docs/goal/architecture/serialization.md` § How decode-strictness is
actually enforced).

That section states the validator is the security boundary "for every decode
routed through `fauna-cbor`" and enumerates six sites that bypass it, each
safe today for a site-specific reason (self-encoded bytes, an `Ipld` target's
own duplicate-key rejection, a length/version-bounded header with no CID
derivation). A SEVENTH bare `from_slice`/`from_reader` outside
`libs/fauna-cbor/` must not inherit that boundary claim by silent default — it
needs the same kind of argument, in the goal doc, before it ships.

Enforcement is a DOWN-ONLY RATCHET on the per-file count of such call sites,
same discipline as `check_sleep_ratchet.py` / `check_app_gate_ratchet.py`: a
file's count may never rise past its committed baseline (missing entry = 0,
so a brand-new file starts clean). A rise means either revert the new call
site or route it through `fauna-cbor::decode_strict`/`decode_lenient`, or —
if it is genuinely safe by a new argument — extend
`docs/goal/architecture/serialization.md`'s enumeration and widen the
baseline in the same commit.

Usage:
  check_dagcbor_bypass_ratchet.py                    # run the gate
  check_dagcbor_bypass_ratchet.py --list              # print current sites
  check_dagcbor_bypass_ratchet.py --update-baseline   # regenerate the
                                                       # baseline from the
                                                       # current tree;
                                                       # refuses on a rise
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCAN_DIRS = ["libs", "bins", "apps"]
EXCLUDE_PREFIX = "libs/fauna-cbor/"
BASELINE_PATH = REPO / "scripts" / "dagcbor_bypass_ratchet_baseline.json"

CALL = re.compile(r"\bserde_ipld_dagcbor::from_(slice|reader)\b")


def _scan_files() -> list[Path]:
    files = []
    for sub in SCAN_DIRS:
        base = REPO / sub
        if base.is_dir():
            files.extend(sorted(base.rglob("*.rs")))
    return files


def bypass_sites(path: Path) -> list[tuple[int, str]]:
    """Line numbers + snippets of every raw `serde_ipld_dagcbor::from_*` call
    in `path` — a real call, not a doc-comment mention (`///` / `//!` lines
    and block-comment bodies are skipped so the doc's own citations of the
    symbol don't count as call sites)."""
    hits = []
    try:
        lines = path.read_text().splitlines()
    except (OSError, UnicodeDecodeError):
        return hits
    for i, line in enumerate(lines, start=1):
        stripped = line.strip()
        if stripped.startswith("///") or stripped.startswith("//!"):
            continue
        if CALL.search(line):
            hits.append((i, stripped))
    return hits


def current_counts() -> dict[str, list[tuple[int, str]]]:
    """{repo-relative path: hits}, POSIX-separated keys, excluding fauna-cbor
    itself (the validator's own implementation and its baseline-measurement
    tests are the sanctioned raw-decode callers)."""
    counts = {}
    for path in _scan_files():
        rel = path.relative_to(REPO).as_posix()
        if rel.startswith(EXCLUDE_PREFIX):
            continue
        hits = bypass_sites(path)
        if hits:
            counts[rel] = hits
    return counts


def load_baseline() -> dict[str, int]:
    if not BASELINE_PATH.exists():
        return {}
    return json.loads(BASELINE_PATH.read_text())


def run_gate() -> int:
    baseline = load_baseline()
    counts = current_counts()

    risen = []
    improved = []
    for file, hits in counts.items():
        n = len(hits)
        base_n = baseline.get(file, 0)
        if n > base_n:
            risen.append((file, base_n, n, hits))
        elif n < base_n:
            improved.append((file, base_n, n))
    for file, base_n in baseline.items():
        if file not in counts and base_n > 0:
            improved.append((file, base_n, 0))

    if risen:
        print(
            "check-dagcbor-bypass-ratchet: FAIL — raw serde_ipld_dagcbor::from_* "
            "call sites outside libs/fauna-cbor/ rose past the committed baseline "
            "(serialization.md § How decode-strictness is actually enforced):",
            file=sys.stderr,
        )
        for file, base_n, n, hits in sorted(risen):
            print(f"\n  {file}: {base_n} -> {n}", file=sys.stderr)
            for line_no, snippet in hits:
                print(f"    {file}:{line_no}: {snippet}", file=sys.stderr)
        print(
            "\nFix: route the decode through fauna_cbor::decode_strict/decode_lenient "
            "so the pre-parse validator covers it, or — if this site is genuinely safe "
            "for a new site-specific reason (self-encoded bytes, an Ipld target's own "
            "duplicate-key rejection, a length/version-bounded header with no CID "
            "derivation, ...) — add it to serialization.md's enumeration with that "
            "reason, then widen the baseline in the same commit "
            "(`just dagcbor-bypass-ratchet-update`). Never widen the baseline to "
            "launder a new, unargued bypass in.",
            file=sys.stderr,
        )
        return 1

    print(
        "check-dagcbor-bypass-ratchet: OK — no file exceeds its baseline "
        f"({BASELINE_PATH.relative_to(REPO)})."
    )
    if improved:
        print(
            f"  {len(improved)} file(s) improved (fewer raw call sites than "
            "baseline) — run `just dagcbor-bypass-ratchet-update` to shrink the "
            "ratchet."
        )
    return 0


def update_baseline() -> int:
    if BASELINE_PATH.exists() and run_gate() != 0:
        print(
            "check-dagcbor-bypass-ratchet: refusing --update-baseline — the gate is "
            "currently failing against the existing baseline. A down-only ratchet "
            "never writes a rise; fix or argue the new site(s) first.",
            file=sys.stderr,
        )
        return 1
    counts = current_counts()
    new_baseline = {file: len(hits) for file, hits in counts.items()}
    BASELINE_PATH.parent.mkdir(parents=True, exist_ok=True)
    BASELINE_PATH.write_text(json.dumps(new_baseline, indent=2, sort_keys=True) + "\n")
    print(
        f"check-dagcbor-bypass-ratchet: wrote {len(new_baseline)} file(s) to "
        f"{BASELINE_PATH.relative_to(REPO)} — commit it alongside your fix."
    )
    return 0


def list_sites() -> int:
    counts = current_counts()
    if not counts:
        print("check-dagcbor-bypass-ratchet: no raw call sites found.")
        return 0
    for file in sorted(counts):
        for line_no, snippet in counts[file]:
            print(f"{file}:{line_no}: {snippet}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) > 1:
        if argv[1] == "--update-baseline":
            return update_baseline()
        if argv[1] == "--list":
            return list_sites()
        print(f"check-dagcbor-bypass-ratchet: unknown argument {argv[1]!r}", file=sys.stderr)
        return 2
    return run_gate()


if __name__ == "__main__":
    sys.exit(main(sys.argv))

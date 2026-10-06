#!/usr/bin/env python3
"""Sync a freshly-generated tree into its checked-in destination, writing ONLY
the files whose bytes actually changed.

Why this exists — the mtime contract
------------------------------------
Several `build-if-stale` gates watch a whole source tree. Every wasm chunk, for
instance, is gated on `--source libs` (justfile, `wasm-core` and its eight
siblings) with only `*/pkg/*` and `*/pkg-test/*` excluded. So ANY generator that
drops output inside such a tree makes every one of those gates stale the moment
it runs — even when it regenerates byte-identical content, because the gate
compares mtimes, not bytes.

That is not hypothetical: `uniffi-bindgen-go` rewrites `libs/fauna-mail-go/`
unconditionally, so running `just mail-bridge-build` after `just web-test`
invalidated all nine wasm chunks and the next `--client web` e2e silently paid a
full ~10-minute wasm rebuild it had already paid once (observed 2026-07-24; the symptom reads as "the e2e harness is slow", not as a
build-ordering bug, which is what makes it expensive to diagnose).

The repo's own generators already avoid this by comparing content before
writing — `scripts/providers-generate.py` prints `unchanged <path>` and leaves
the file alone, and `i18n/generator/generate.py` prints `UNCHANGED:`; the wasm
chunks' `scripts/sync-wasm-static.py` mirror is the same idiom for artifacts.
This helper extends it to generators that emit a whole DIRECTORY and cannot be
taught to compare per file, by staging them outside the watched tree and
syncing here.

Preferred over widening each gate's `--exclude` list: an exclude is a blocklist
that must be maintained forever (it has already been patched twice, for `pkg`
and `pkg-test`), and every generator added later silently re-opens the hole.
Flipping the gates to an allowlist would be worse still — a blocklist miss
over-rebuilds (slow but correct), an allowlist miss under-rebuilds (a FALSE
GREEN, the failure the justfile's `wasm-onboarding` comment records as having
been live in the fleet).

Usage:
    sync-generated-tree.py <staged-dir> <dest-dir> [--keep name ...]

`--keep` names destination-relative paths that are NOT generator output and must
survive the sync (e.g. a hand-written `go.mod` skeleton beside generated code).
Anything else in the destination that the generator no longer emits is removed,
so a dropped binding cannot linger and keep compiling.

Exit status is 0 on success; the summary line reports how much actually moved,
so a caller can see at a glance whether downstream gates were disturbed.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path


def _relative_files(root: Path) -> set[Path]:
    return {p.relative_to(root) for p in root.rglob("*") if p.is_file()}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("staged", type=Path, help="directory the generator just wrote")
    ap.add_argument("dest", type=Path, help="checked-in destination directory")
    ap.add_argument(
        "--keep",
        action="append",
        default=[],
        metavar="REL_PATH",
        help="destination-relative path to preserve even though the generator "
             "does not emit it (repeatable)",
    )
    args = ap.parse_args()

    staged: Path = args.staged
    dest: Path = args.dest

    if not staged.is_dir():
        print(f"sync-generated-tree: staged dir does not exist: {staged}", file=sys.stderr)
        return 1
    dest.mkdir(parents=True, exist_ok=True)

    keep = {Path(k) for k in args.keep}
    new_files = _relative_files(staged)
    old_files = _relative_files(dest)

    wrote = unchanged = removed = 0

    for rel in sorted(new_files):
        src = staged / rel
        out = dest / rel
        src_bytes = src.read_bytes()
        # The whole point: an identical file is left completely untouched, so its
        # mtime — and every staleness gate watching this tree — stays put.
        if out.is_file() and out.read_bytes() == src_bytes:
            unchanged += 1
            continue
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(src_bytes)
        shutil.copymode(src, out)
        print(f"wrote {dest / rel}")
        wrote += 1

    # Drop output the generator no longer emits, so a removed binding cannot
    # linger in the tree and keep satisfying an import.
    for rel in sorted(old_files - new_files - keep):
        (dest / rel).unlink()
        print(f"removed {dest / rel}")
        removed += 1

    print(
        f"sync-generated-tree {dest}: {wrote} wrote, {unchanged} unchanged, "
        f"{removed} removed"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

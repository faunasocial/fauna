#!/usr/bin/env -S uv run --quiet --no-project
"""Mirror a wasm-pack `pkg` dir's artifacts into the SPA's `static/` dir.

Usage:
    sync-wasm-static.py --from <pkg-dir> --to <static-dir> --stem <name> [-q]

Copies `<stem>_bg.wasm`, `<stem>.js`, `<stem>.d.ts`, `<stem>_bg.wasm.d.ts`
from <pkg-dir> to <static-dir>, but ONLY where the destination's bytes differ.

Why a content compare rather than a plain `cp`
----------------------------------------------
`static/` is a shared, mutable slot: two producers write the SAME four paths for
a given stem — the prod chunk (`wasm-onboarding`) and the test-helpers chunk
(`wasm-onboarding-test`). Whichever ran last owns the slot, so every producer
must RE-ASSERT its own artifacts on every run; a build-gated copy lets the other
variant's files survive (see `build-system.md` § WASM chunking convention →
Chunks with two producers).

But `static/` is also a `--source` of the `web` / `web-test` build-if-stale
gates. An unconditional `cp` would bump its mtime on every run and make the SPA
rebuild every time — trading a correctness bug for a load regression, which is
the opposite of what this dir is for. Comparing bytes keeps the mtime (and so
the SPA's no-op) stable whenever nothing actually changed.

Exits non-zero if a source artifact is missing — that means the build step that
should have produced it did not run, and silently shipping the previous
variant's file is exactly the failure this script exists to prevent.
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

# The four artifacts wasm-pack emits per chunk that the SPA consumes.
_SUFFIXES = ("_bg.wasm", ".js", ".d.ts", "_bg.wasm.d.ts")


def _same_bytes(a: Path, b: Path) -> bool:
    if not b.exists():
        return False
    if a.stat().st_size != b.stat().st_size:
        return False
    return a.read_bytes() == b.read_bytes()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="src", required=True, help="wasm-pack out-dir")
    ap.add_argument("--to", dest="dst", required=True, help="SPA static dir")
    ap.add_argument("--stem", required=True, help="artifact stem, e.g. fauna_wasm_onboarding")
    ap.add_argument("-q", "--quiet", action="store_true")
    args = ap.parse_args()

    src_dir, dst_dir = Path(args.src), Path(args.dst)
    dst_dir.mkdir(parents=True, exist_ok=True)

    missing = [s for s in _SUFFIXES if not (src_dir / f"{args.stem}{s}").is_file()]
    if missing:
        print(
            f"[sync-wasm-static] {args.stem}: missing in {src_dir}: "
            f"{', '.join(args.stem + m for m in missing)}",
            file=sys.stderr,
        )
        return 1

    changed = []
    for suffix in _SUFFIXES:
        name = f"{args.stem}{suffix}"
        src, dst = src_dir / name, dst_dir / name
        if _same_bytes(src, dst):
            continue
        shutil.copyfile(src, dst)
        changed.append(name)

    # A re-assert is never routine: it means another variant had overwritten this
    # chunk's slot in static/. Always report it, even under -q; -q suppresses only
    # the no-op line, which is what makes the recipes quiet in the common case.
    if changed:
        print(
            f"[sync-wasm-static] {args.stem}: re-asserted {len(changed)} artifact(s) → {dst_dir}",
            flush=True,
        )
    elif not args.quiet:
        print(f"[sync-wasm-static] {args.stem}: static/ already current", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())

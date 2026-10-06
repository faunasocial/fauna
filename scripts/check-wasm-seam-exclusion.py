#!/usr/bin/env python3
"""Assert the wasm e2e seam surface is absent from the PRODUCTION chunk flavor.

Usage:
    check-wasm-seam-exclusion.py [--prod-pkg DIR] [--test-pkg DIR] [--stem NAME]

This is the headless witness for `testing.md` § *Cross-app e2e conventions*
point 15 ("the automation surface … is **absent from the release/production
artifact**, verifiable by a `strings`/grep of the built artifact") on the web/wasm
leg. Before it existed, the only evidence anywhere in the fleet was a one-off
manual `strings -a` table covering 7 of ~17 symbols on 1 of 7 apps — the
"excused test" class this project treats as iron-clad, and precisely what let
`libs/fauna-wasm` keep shipping the seams for months after the convention was
ratified.

What it asserts, and why in this order
--------------------------------------
The expected seam list is **derived from the difference between the two flavors'
generated `.d.ts`**, never hard-coded. A hard-coded list is a maintenance trap:
add a seam, forget the list, and the check passes while the artifact regresses.
The diff *is* the seam surface, by construction.

  1. The diff is NON-EMPTY. A gate that excludes nothing is a gate that is not
     wired up — this is what catches "the feature flag stopped being consulted".
  2. Every name in the diff LOOKS like a test seam (see `_SEAM_RE`). A production
     export that vanished from the prod flavor means a `#[cfg]` landed on the
     wrong item — a functional regression that would otherwise surface as a
     runtime `undefined is not a function` in the browser.
  3. NOTHING in the prod glue or prod `.d.ts` matches `_SEAM_RE`. This is the
     actual security assertion, and it is pattern-based on purpose: a NEW seam
     added without a `#[cfg(feature = "test-helpers")]` fails here automatically,
     with no list for anyone to update.

Assertion 3 reads the **JS glue**, not just the `.wasm`. That is where the
callable surface lives: `wasm-opt` strips the binary's name section, so the
snake_case Rust symbols are absent from `fauna_wasm_bg.wasm` even in a bundle
that exports every seam — a `strings` check on the `.wasm` alone reports a
false green. The `.js` glue's export table is the artifact an attacker (or a
stray page script) actually calls.

Exit 0 = the production flavor is clean. Non-zero prints every offending symbol.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# A test-seam export, by naming convention. Kept deliberately broad: this is the
# tripwire for seams nobody remembered to gate, so it should over-match rather
# than under-match. Every seam in `libs/fauna-wasm/src` is named to satisfy it —
# if you add one that does not, rename the seam, do not loosen this regex.
#
# ⚠ Do NOT anchor these alternatives with `\b`. wasm-bindgen emits an internal
# export per method, prefixed with the lowercased type name and an underscore
# (`wasmconversationsmanager_injectSendFailure`). `_` is a word character, so a
# leading `\b` does not match there — the first version of this regex had one and
# classified that symbol as "not a seam", which assertion (2) then reported as a
# misplaced `#[cfg]`. Matching mid-identifier is correct here: these fragments are
# distinctive enough that over-matching costs nothing, and under-matching is what
# lets an ungated seam through assertion (3).
_SEAM_RE = re.compile(
    r"""
      ForTest             # installMockBackendsForTest, …_injectPostsForTest, …
    | [iI]nject[A-Z]       # injectSendFailure, wasmconversationsmanager_inject…
    | [sS]eedResolved      # seedResolvedLinkPreview*
    | [rR]einstallReal     # reinstallRealFaunaMls*
    | _for_test            # the snake_case Rust name, should the glue carry it
    """,
    re.VERBOSE,
)

# `export class Foo {` / `  someMethod(` — enough to enumerate the surface a
# `.d.ts` declares without parsing TypeScript.
_DTS_NAME_RE = re.compile(r"^\s*(?:readonly\s+)?([A-Za-z_$][\w$]*)\s*[(:]", re.MULTILINE)


def _names(dts: Path) -> set[str]:
    return set(_DTS_NAME_RE.findall(dts.read_text(encoding="utf-8", errors="replace")))


def _hits(path: Path) -> list[str]:
    """Every distinct seam-looking identifier appearing in `path`."""
    text = path.read_text(encoding="utf-8", errors="replace")
    found = {
        m.group(0)
        for m in re.finditer(r"[A-Za-z_$][\w$]*", text)
        if _SEAM_RE.search(m.group(0))
    }
    return sorted(found)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--prod-pkg", default="libs/fauna-wasm/pkg")
    ap.add_argument("--test-pkg", default="libs/fauna-wasm/pkg-test")
    ap.add_argument("--stem", default="fauna_wasm")
    args = ap.parse_args()

    prod, test = Path(args.prod_pkg), Path(args.test_pkg)
    prod_dts, prod_js = prod / f"{args.stem}.d.ts", prod / f"{args.stem}.js"
    test_dts = test / f"{args.stem}.d.ts"

    for p in (prod_dts, prod_js, test_dts):
        if not p.is_file():
            print(
                f"[wasm-seam-check] missing {p} — build both flavors first "
                f"(`just wasm-core` and `just wasm-core-test`)",
                file=sys.stderr,
            )
            return 2

    failures: list[str] = []

    # (1)+(2): the flavor diff is the seam surface.
    diff = sorted(_names(test_dts) - _names(prod_dts))
    if not diff:
        failures.append(
            "the two flavors declare the SAME surface — the `test-helpers` gate "
            "excluded nothing. Either the feature is being enabled for the "
            "production build (check `libs/fauna-wasm/Cargo.toml`'s dep lines for "
            "a stray `test-helpers`), or `pkg-test/` is a stale copy of `pkg/`."
        )
    else:
        mislabeled = [n for n in diff if not _SEAM_RE.search(n)]
        if mislabeled:
            failures.append(
                "these exports are in the TEST flavor only but do not look like "
                "test seams — a `#[cfg(feature = \"test-helpers\")]` is probably on "
                f"the wrong item: {', '.join(mislabeled)}"
            )

    # (3) the security assertion: no seam-shaped symbol in the production artifact.
    for artifact in (prod_dts, prod_js):
        hits = _hits(artifact)
        if hits:
            failures.append(
                f"{artifact} still carries {len(hits)} seam symbol(s): "
                f"{', '.join(hits)}"
            )

    if failures:
        print("[wasm-seam-check] FAILED", file=sys.stderr)
        for f in failures:
            print(f"  * {f}", file=sys.stderr)
        return 1

    print(
        f"[wasm-seam-check] OK — production {args.stem} exports none of the "
        f"{len(diff)} test seam(s): {', '.join(diff)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

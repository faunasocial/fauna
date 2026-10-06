#!/usr/bin/env python3
"""Rewrite `uniffi-bindgen-go`'s bare cross-namespace imports into
module-qualified ones, in a freshly-generated Go binding tree.

The class this closes
---------------------
`uniffi-bindgen-go` emits one Go package per UniFFI namespace, and when an
export in namespace A returns a type from namespace B it writes the import as
the bare package NAME:

    import (
        "bytes"
        "fauna_core"      <-- not a resolvable Go module path
        "fmt"
    )

That resolves only for a GOPATH-era top-level package. In our single-module
tree (`libs/fauna-mail-go`, module `github.com/faunasocial/fauna/libs/fauna-mail-go`)
it does not, and `go build` fails with `package fauna_core is not in std`. Two
generated packages carried this breakage undetected for weeks — harmless only
because nothing imported them, which is precisely what made them landmines: the
first session to need one discovers it.

The previous containment was per-export: gate each offending
`#[uniffi::export]` behind a feature the Go `--no-default-features` build drops
(`libs/fauna-ffi/src/provisioning.rs`'s `provisioning_elapsed` is the
precedent). That works but is one-by-one forever, costs the Swift/Kotlin
surface those exports legitimately serve, and the NEXT cross-namespace export
reintroduces the same red.

Rewriting the import closes the whole class instead: the package *name* is the
last path element either way, so every `fauna_core.Foo` reference still
resolves untouched, no export is gated, and no capability is lost. Adapting
generator output to our tree layout is what this pipeline already does
elsewhere (staging dir, byte-compare sync, skeleton `go.mod`) — see the
justfile's `mail-bridge-ffi` / `_mail-bridge-ffi-bindgen` recipes.

Where it runs
-------------
BOTH generator paths, or the drift check reds:

  * `_mail-bridge-ffi-bindgen` — over the cargo-target staging dir, before
    `sync-generated-tree.py` mirrors it into `libs/fauna-mail-go`.
  * `mail-bridge-ffi-check` — over its own throwaway bindgen output, before the
    `diff -rq` against the tracked tree. The tracked tree carries qualified
    imports, so the check must qualify its comparand the same way.

Sorting: gofmt sorts imports within a group by path, and the tracked tree is
gofmt-clean today. A qualified path sorts differently from the bare name it
replaces, so any block we touch is re-sorted per group — otherwise this script
would be the thing that makes the generated tree gofmt-dirty.

Verification: after rewriting, every import declaration is re-scanned and a
surviving bare `fauna_*` import is a hard error. That is the pin the tracked
tree cannot express on its own — a generator upgrade that emits an import spec
in a shape the rewriter does not recognize fails HERE, loudly, at generation
time, instead of silently landing an uncompilable binding for a later session
to discover.

Usage:
    qualify-go-binding-imports.py <tree-dir> [--module PATH]

Exit status 0 on success (including "nothing to do"); 1 if a bare
cross-namespace import survives.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# Must match the skeleton `go.mod` the `mail-bridge-ffi` recipe writes into
# libs/fauna-mail-go — pinned by
# tests/e2e-unified/tests/test_go_binding_import_qualification.py, which reads
# that go.mod rather than trusting this constant.
MODULE_PATH = "github.com/faunasocial/fauna/libs/fauna-mail-go"

# A generated namespace package: `fauna_` + lowercase snake, and crucially NO
# slash — an already-qualified path never matches, which is what makes this
# script idempotent.
_BARE = re.compile(r"^fauna_[a-z0-9_]+$")

# One import spec. The optional leading word covers both an alias inside a
# block (`_ "x"`, `alias "x"`) and the `import` keyword of a single-declaration
# form (`import "C"`), so one pattern handles every place a path can appear.
_SPEC = re.compile(
    r'^(?P<indent>\s*)'
    r'(?P<lead>(?:[A-Za-z_][A-Za-z0-9_]*|\.)\s+)?'
    r'"(?P<path>[^"]*)"(?P<tail>.*)$'
)


def _qualify(path: str, module: str) -> str | None:
    """The module-qualified form of a bare namespace import, else None."""
    return f"{module}/{path}" if _BARE.match(path) else None


def import_regions(lines: list[str]) -> list[tuple[int, int]]:
    """Half-open [start, end) line ranges that hold import specs.

    Go has exactly two import forms, so recognizing both is exhaustive: a
    parenthesized block (whose body is the region) and a single declaration
    (whose own line is the region). An import the rewriter fails to match is
    therefore still inside a region the verification pass reads.
    """
    regions: list[tuple[int, int]] = []
    i = 0
    while i < len(lines):
        stripped = lines[i].strip()
        if stripped == "import (":
            start = i + 1
            j = start
            while j < len(lines) and lines[j].strip() != ")":
                j += 1
            regions.append((start, j))
            i = j + 1
            continue
        if re.match(r"^import\s", stripped):
            regions.append((i, i + 1))
        i += 1
    return regions


def _sort_block(lines: list[str], start: int, end: int) -> None:
    """Re-sort each blank-line-separated group of an import block by path.

    gofmt's own rule: sort within a group, never regroup. Only blocks we
    actually rewrote are sorted, so this is a no-op everywhere else.
    """
    group: list[tuple[str, str]] = []
    group_start = start

    def flush() -> None:
        if group:
            group.sort(key=lambda kv: kv[0])
            for offset, (_, text) in enumerate(group):
                lines[group_start + offset] = text
            group.clear()

    for idx in range(start, end):
        m = _SPEC.match(lines[idx]) if lines[idx].strip() else None
        if m is None:
            # A blank line, or a line that is not a spec (a standalone
            # comment): close the group here so nothing sorts across it.
            flush()
            group_start = idx + 1
            continue
        group.append((m.group("path"), lines[idx]))
    flush()


def qualify_tree(root: Path, module: str) -> tuple[int, int, list[str]]:
    """Rewrite every .go file under `root`. Returns (files, imports, errors)."""
    files = imports = 0
    errors: list[str] = []

    for path in sorted(root.rglob("*.go")):
        text = path.read_text()
        lines = text.splitlines()
        rewrote_here = 0

        for start, end in import_regions(lines):
            touched = False
            for idx in range(start, end):
                m = _SPEC.match(lines[idx])
                if m is None:
                    continue
                qualified = _qualify(m.group("path"), module)
                if qualified is None:
                    continue
                lead = m.group("lead") or ""
                lines[idx] = (
                    f'{m.group("indent")}{lead}"{qualified}"{m.group("tail")}'
                )
                rewrote_here += 1
                touched = True
            if touched and end - start > 1:
                _sort_block(lines, start, end)

        if rewrote_here:
            trailer = "\n" if text.endswith("\n") else ""
            path.write_text("\n".join(lines) + trailer)
            files += 1
            imports += rewrote_here
            print(f"qualified {rewrote_here} import(s) in {path}")

        # Verification reads the rewritten lines rather than trusting the loop
        # above: a spec shape the rewriter does not match is still inside an
        # import region, so it is caught here instead of at some later compile.
        for start, end in import_regions(lines):
            for idx in range(start, end):
                for found in re.findall(r'"([^"]*)"', lines[idx]):
                    if _BARE.match(found):
                        errors.append(f'{path}:{idx + 1}: bare import "{found}"')

    return files, imports, errors


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("tree", type=Path, help="generated Go binding tree to rewrite")
    ap.add_argument(
        "--module",
        default=MODULE_PATH,
        help=f"Go module path the tree lives under (default: {MODULE_PATH})",
    )
    args = ap.parse_args(argv[1:])

    if not args.tree.is_dir():
        print(f"qualify-go-binding-imports: no such tree: {args.tree}", file=sys.stderr)
        return 1

    files, imports, errors = qualify_tree(args.tree, args.module)

    if errors:
        w = sys.stderr.write
        w("ERROR: bare cross-namespace import(s) survived qualification:\n")
        for e in errors:
            w(f"  {e}\n")
        w("  uniffi-bindgen-go emitted an import spec in a shape this script does\n")
        w("  not rewrite — teach scripts/qualify-go-binding-imports.py the new shape.\n")
        w("  Do NOT work around it by gating the export: that is the per-export\n")
        w("  containment this script exists to replace.\n")
        return 1

    print(
        f"qualify-go-binding-imports {args.tree}: {imports} import(s) qualified "
        f"across {files} file(s)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

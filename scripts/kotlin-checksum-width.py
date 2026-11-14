"""Read each uniffi API checksum at its native width in generated Kotlin.

Usage: kotlin-checksum-width.py <kotlin-out-dir>

uniffi 0.31.2's Kotlin generator declares every `*_checksum_*` function as
returning `Int` and compares it with the unsigned constant. The native function
returns a u16, and on aarch64 Linux — the host JVM the android unit tests run
on, and arm64 Android devices — the upper half of the return register is not
defined: the build leaves a checksum over 32767 sign-extended, the comparison
fails, and every binding class throws "UniFFI API checksum mismatch" from its
initializer (the android host tests, 1404 of them, went red that way the day
the bump landed). This rewrite restores the shape the 0.31.0 generator wrote —
a `Short` return and an `N.toShort()` comparison — which JNA reads at 16 bits.

Run on the out-dir right after every `uniffi-bindgen generate --language
kotlin` whose bindings load a library (the android flavor stage and the host
stage in the justfile). Idempotent: a tree already in the 16-bit shape is left
as it is. A file whose declarations and comparisons disagree in count is
refused, never half-written. Remove this step once the generator writes the
16-bit shape again (a later uniffi, taken in the owner's bump round).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

_DECL = re.compile(r"(fun uniffi_\w+_checksum_\w+\(\s*\)): Int\b")
_CMP = re.compile(r"(lib\.uniffi_\w+_checksum_\w+\(\) != )(\d+)\)")


def rewrite(text: str) -> tuple[str, int, int]:
    """Return the rewritten text and the declaration and comparison counts."""
    decls = len(_DECL.findall(text))
    cmps = len(_CMP.findall(text))
    text = _DECL.sub(r"\1: Short", text)
    text = _CMP.sub(r"\1\2.toShort())", text)
    return text, decls, cmps


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__.splitlines()[2], file=sys.stderr)
        return 2
    root = Path(argv[1])
    bad: list[str] = []
    changed = 0
    for kt in sorted(root.rglob("*.kt")):
        old = kt.read_text()
        new, decls, cmps = rewrite(old)
        if decls != cmps:
            bad.append(f"{kt}: {decls} Int checksum declaration(s), {cmps} Int comparison(s)")
            continue
        if new != old:
            kt.write_text(new)
            changed += 1
    for line in bad:
        print(f"kotlin-checksum-width: refused — {line}", file=sys.stderr)
    if bad:
        return 1
    print(f"kotlin-checksum-width: {changed} file(s) rewritten to 16-bit checksum reads under {root}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

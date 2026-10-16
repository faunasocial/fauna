"""A private cdylib slot is replaced by rename, never rewritten in place.

`just e2e-ffi` (and `mail-bridge-ffi`) copy a fresh cargo build into a private
slot that a LIVE process has `dlopen`ed: `fauna_ffi.py` re-runs the recipe on
every import, so any session in the same checkout that edits `libs/` and runs
one test rebuilds the slot while a long run still has the old library mapped.
A plain `cp SRC DST` onto an existing file truncates and rewrites DST's inode —
the very pages the running process maps — and the next call into it segfaults.
The tier_2/3 web measuring run of 2026-10-08 died that way, exit 139 in
`fauna_ffi.build_post` on its first test.

Copying to a temporary name beside the slot and `mv`-ing it over the slot
swaps the directory entry instead: a process holding the old inode keeps it
until it exits, and every new load sees the new file.

Pure text analysis of the justfile — no build.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_JUSTFILE = Path(__file__).resolve().parents[3] / "justfile"


def _slot_copies() -> list[str]:
    text = _JUSTFILE.read_text(encoding="utf-8")
    return [
        line.strip()
        for line in text.splitlines()
        if re.search(r'\bcp\b[^\n]*"\$CARGO_LIB"', line)
    ]


def test_the_slot_recipes_are_found():
    # Two recipes copy into a private slot today; a pin that matches nothing
    # would pass vacuously.
    assert len(_slot_copies()) >= 2, _slot_copies()


@pytest.mark.parametrize("line", _slot_copies())
def test_a_slot_copy_lands_by_rename(line):
    assert not re.search(r'\bcp\s+"\$CARGO_LIB"\s+"\$LIB"', line), (
        "this copy rewrites the loaded library's inode in place — copy to a "
        f'temporary name and `mv -f` it over "$LIB": {line}'
    )
    assert re.search(r'\bmv -f "\$LIB\.\$\$\.tmp" "\$LIB"', line), (
        f'expected the copy to finish with `mv -f "$LIB.$$.tmp" "$LIB"`: {line}'
    )

"""The held failure's mark (mac-simulator-hold.md § Per-core tokens, *Under a pass
hold the pass's own simulators run at the floor* → *A red under the hold is no red*).

While this machine's schedule word is `held`, the pass's own simulators run at a
quarter share, so a test the background tester runs then may fail on the slowed
device rather than on the code. The conftest samples the word at an item's setup
start, its call start and at a failing report; a failure during which the word read
`held` at any of the three is MARKED — one `[e2e-held] <key>` line on stdout — and
records nothing in the last-green directory or the feature ledger. The check
scripts read the marks from the run's log, keep the item out of the shard's reds
and run it again once the word has lifted (`delayed-e2e-pick.py held`). A green
under the hold is a green and is recorded as ever.

Only the tester's runs mark (`FAUNA_RUN_ORIGIN=tester`): a session's own run is
never under the pass hold. Pure state here; the conftest wires the word reader
(build-slot.py's `_gate_check_schedule`, the one reading of the word)."""

from __future__ import annotations

import os

#: The line's prefix, which `delayed-e2e-pick.py held` reads back from the log.
MARK = "[e2e-held]"
#: The word that marks: the broker's `held` (gate-check-schedule.md § The four
#: states of a pass).
HELD_WORD = "held"


def enabled() -> bool:
    """Only the background tester's runs are ever under a pass hold."""
    return os.environ.get("FAUNA_RUN_ORIGIN", "").strip() == "tester"


def read_word(reader) -> str:
    """The schedule word through `reader` (a callable, or None where the slot tool
    is absent); any failure reads `day` — the fallback every number has, so a
    reader that breaks never marks a real red away."""
    if reader is None:
        return "day"
    try:
        word = reader()
    except Exception:  # noqa: BLE001 - a broken reader is no hold
        return "day"
    return word if isinstance(word, str) else "day"


def mark_line(key: str) -> str:
    return f"{MARK} {key}"


class HeldMarks:
    """Which items saw the word `held`, and which failures were marked."""

    def __init__(self) -> None:
        self._saw_held: set = set()
        self.marked: set = set()

    def sample(self, nodeid: str, word: str) -> None:
        """One sample of the word for an item (its setup start, its call start)."""
        if word == HELD_WORD:
            self._saw_held.add(nodeid)

    def judge(self, nodeid: str, failed: bool, word: str) -> bool:
        """A report of the item: the word sampled once more when it failed, and
        True when this report is the item's FIRST marked failure (the caller
        prints the line then). A passing report never marks."""
        if not failed or nodeid in self.marked:
            return False
        self.sample(nodeid, word)
        if nodeid not in self._saw_held:
            return False
        self.marked.add(nodeid)
        return True

    def is_marked(self, nodeid: str) -> bool:
        return nodeid in self.marked

    def reset(self) -> None:
        self._saw_held.clear()
        self.marked.clear()

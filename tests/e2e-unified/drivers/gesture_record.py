"""The driver's gesture record — what a journey test proves it walked.

Owner: `docs/goal/behavior/main-use-cases.md` § The journeys, point 2: the
driver records the element id of every gesture it performs, and a test marked
`@pytest.mark.use_case("<id>")` ends by comparing that record with the
catalogue's steps (`helpers/use_case_walk.py::assert_use_case_walked`).

A gesture is a driver call that acts on an element by id: `click`,
`double_click`, `type_text`, `clear_and_type` (and `fill`, which delegates to
it), `press_key`, `select`, `set_input_files`. A read (`get_text`,
`is_visible`, a wait) records nothing, and neither does `set_state` — a state
write standing in for a gesture is exactly what the comparison exists to catch.

`PlatformDriver.__init_subclass__` wraps every gesture method a driver class
defines with `recorded`, so all seven drivers inherit the record without any of
them remembering to call it. Only the OUTERMOST gesture records: a driver whose
`clear_and_type` clicks the field first, or whose override calls
`super().click`, is still one gesture. A gesture that raises records nothing —
the record holds what was performed.

Entries carry a process-wide sequence number, so a journey compares only the
gestures made since its own test began (`mark_test_start`, called by the
suite's autouse fixture for a marked test) even when its driver is a
session-scoped one that earlier tests drove too.
"""

from __future__ import annotations

import contextlib
import functools
import itertools
from collections import deque
from dataclasses import dataclass

#: The driver methods that act on an element by id. `fill` is absent on
#: purpose: it delegates to `clear_and_type`, which records.
GESTURE_METHODS = (
    "click",
    "double_click",
    "type_text",
    "clear_and_type",
    "press_key",
    "select",
    "set_input_files",
)

#: How many gestures one driver keeps. A session-scoped driver serves many
#: tests; a journey needs only its own, far fewer than this.
RECORD_CAP = 10_000

_seq = itertools.count(1)
_test_start = 0


@dataclass(frozen=True)
class Gesture:
    seq: int
    method: str
    element: str
    setup: bool


def _state(driver) -> dict:
    st = driver.__dict__.get("_gesture_state")
    if st is None:
        st = {"record": deque(maxlen=RECORD_CAP), "depth": 0, "setup": 0}
        driver.__dict__["_gesture_state"] = st
    return st


def recorded(method_name: str, fn):
    """Wrap a driver's gesture method so its outermost call is recorded."""

    @functools.wraps(fn)
    def wrapper(self, element_id, *args, **kwargs):
        st = _state(self)
        st["depth"] += 1
        try:
            result = fn(self, element_id, *args, **kwargs)
        finally:
            st["depth"] -= 1
        if st["depth"] == 0:
            st["record"].append(
                Gesture(next(_seq), method_name, str(element_id), st["setup"] > 0)
            )
        return result

    wrapper.__gesture_recorded__ = True
    return wrapper


def mark_test_start() -> None:
    """Begin a journey: only gestures after this call are compared."""
    global _test_start
    _test_start = next(_seq)


def gestures_since_test_start(driver) -> list[Gesture]:
    return [g for g in _state(driver)["record"] if g.seq > _test_start]


@contextlib.contextmanager
def setup_gestures(driver):
    """Declare the gestures inside the block as setup, not the use case's steps.

    A journey arranges its world as any test does; gestures it makes between
    two catalogued steps for that purpose (dismissing a sheet the fixture left,
    picking a seeded folder) are allowed only inside this block.
    """
    st = _state(driver)
    st["setup"] += 1
    try:
        yield
    finally:
        st["setup"] -= 1

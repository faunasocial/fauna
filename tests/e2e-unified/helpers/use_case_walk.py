"""`assert_use_case_walked` — a journey proves it walked its use case's steps.

Owner: `docs/goal/behavior/main-use-cases.md` § The journeys, point 2. A test
marked `@pytest.mark.use_case("<id>")` ends with
`assert_use_case_walked(driver, "<id>")`, which compares the gestures the
driver recorded since the test began (`drivers/gesture_record.py`) with the
use case's steps in `use-cases.yaml`.

The rule, in the order it is checked:

- every catalogued step was performed, in order;
- between the first step and the last, every gesture is either the next step,
  a repeat on the element of the step just walked (a click that focuses a
  field before typing into it, an Enter that commits it), or a gesture the test
  declared inside `with setup_gestures(driver):`;
- gestures before the first step (arranging the world) and after the last
  (reading the outcome) are the test's own business.

An element matches a step on its id. A step written `base[<what>]` — angle
brackets, a placeholder — matches any gesture on `base`, indexed or not; a step
written `base[literal]` (`settings-nav-row[folders]`) matches that exact id, or
a gesture on the bare `base` when the driver addressed the row by index.

A failure names the use case, its steps, the recorded gestures and where the
walk broke (convention 6: the message diagnoses itself).
"""

from __future__ import annotations

import functools
import re
from dataclasses import dataclass
from pathlib import Path

import yaml

from drivers.gesture_record import gestures_since_test_start, setup_gestures  # noqa: F401

CATALOGUE = Path(__file__).resolve().parent.parent / "use-cases.yaml"

_INDEXED = re.compile(r"^(?P<base>[^\[]+)\[(?P<what>.*)\]$")


@dataclass(frozen=True)
class Recorded:
    element: str
    setup: bool = False


def _split(element: str) -> tuple[str, str | None]:
    m = _INDEXED.match(element)
    return (m["base"], m["what"]) if m else (element, None)


def step_matches(step: str, element: str) -> bool:
    s_base, s_what = _split(step)
    e_base, e_what = _split(element)
    if s_base != e_base:
        return False
    if s_what is None or (s_what.startswith("<") and s_what.endswith(">")):
        return True
    return e_what is None or e_what == s_what


@dataclass
class _Attempt:
    walked: int
    broke_at: int | None  # index into the non-setup record, or None at its end
    reason: str


def _walk_from(steps: list[str], moves: list[str], start: int) -> _Attempt:
    k = 0
    i = start
    while i < len(moves) and k < len(steps):
        el = moves[i]
        if step_matches(steps[k], el):
            k += 1
        elif k > 0 and step_matches(steps[k - 1], el):
            pass  # a repeat on the element of the step just walked
        else:
            return _Attempt(k, i, f"undeclared gesture on {el!r} between step {k} "
                                  f"({steps[k - 1]!r}) and step {k + 1} ({steps[k]!r})")
        i += 1
    if k == len(steps):
        return _Attempt(k, None, "")
    return _Attempt(k, None, f"step {k + 1} ({steps[k]!r}) was never performed "
                             f"after step {k} ({steps[k - 1]!r})")


def compare_walk(steps: list[str], record: list[Recorded]) -> str | None:
    """None when `record` walks `steps`; else why not, in one sentence."""
    if not steps:
        return "the use case has no steps"
    moves = [r.element for r in record if not r.setup]
    starts = [i for i, el in enumerate(moves) if step_matches(steps[0], el)]
    if not starts:
        return f"step 1 ({steps[0]!r}) was never performed"
    best: _Attempt | None = None
    for start in starts:
        attempt = _walk_from(steps, moves, start)
        if not attempt.reason:
            return None
        if best is None or attempt.walked > best.walked:
            best = attempt
    assert best is not None
    return best.reason


@functools.lru_cache(maxsize=1)
def _catalogue() -> dict:
    return yaml.safe_load(CATALOGUE.read_text())


def use_case(use_case_id: str) -> dict:
    for case in _catalogue().get("use_cases") or []:
        if case.get("id") == use_case_id:
            return case
    raise AssertionError(f"use case {use_case_id!r} is not in {CATALOGUE.name}")


def steps_of(use_case_id: str) -> list[str]:
    return [str(s["element"]) for s in use_case(use_case_id)["steps"]]


def failure_message(use_case_id: str, steps: list[str], record: list[Recorded],
                    reason: str) -> str:
    shown = [f"({r.element})" if r.setup else r.element for r in record]
    return (
        f"use case {use_case_id!r} was not walked: {reason}.\n"
        f"  catalogued steps ({len(steps)}): {steps}\n"
        f"  recorded gestures ({len(record)}, setup in parentheses): {shown}\n"
        f"  A step reached by set_state or an API call records nothing: drive the "
        f"gesture. A path the UI no longer offers is corrected in {CATALOGUE.name}."
    )


def assert_use_case_walked(driver, use_case_id: str) -> None:
    steps = steps_of(use_case_id)
    record = [Recorded(g.element, g.setup) for g in gestures_since_test_start(driver)]
    reason = compare_walk(steps, record)
    if reason is not None:
        raise AssertionError(failure_message(use_case_id, steps, record, reason))

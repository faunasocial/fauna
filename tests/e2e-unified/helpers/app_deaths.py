"""An app found dead of a crash at the harness's close fails the phase that found it.

Convention 6's close rider (`docs/goal/architecture/e2e-self-diagnosing-failures.md`
§ The convention): an app's death before the harness closes it is a failure of
the test that drove it. A driver that, at its close of an app, finds the app
already exited by a crash records the finding here with `record()`, carrying
its own diagnosis (the exit phrase, the app's crash reports); this module's
phase wrappers — registered as a plugin by conftest's `pytest_configure` —
raise it as `AppDiedBeforeClose` at the end of the setup, call or teardown
phase it was found in, so it reads as that phase's ERROR or FAILED.

**Why a registry, and not a raise at the close.** A close runs inside a
teardown, and the callers of teardown are many: the multi-seat helpers wrap
each seat's in `except Exception: pass` (`helpers/fleet.py`,
`helpers/room_seats.py`), so a raise there would vanish exactly as the death
did, and others go on cleaning up after it (a seat's surviving sync agent, the
session cache's other drivers), which a raise would skip. A recording reaches
the phase whatever its caller does with exceptions, and the close finishes its
own cleanup first.

Measured 2026-10-10 on Windows: a playing video's teardown killed the windows
app with `0xC000027B` after three of five passing tests, and the run read
`5 passed`.
"""

from __future__ import annotations

import re

import pytest

#: The name conftest registers this module under.
PLUGIN_NAME = "fauna-app-deaths"

#: The deaths found since the last phase ended, in the order found.
_found: list[str] = []

#: The node id of the last test whose teardown finished. A death found in a
#: setup is most often the state the test before it left — the playing video a
#: reset navigates away from — and that test's report is already written, so
#: the setup's failure names it.
_last_finished: str | None = None


class AppDiedBeforeClose(AssertionError):
    """The app under test had died of a crash before the harness closed it.

    An `AssertionError`, because it is the test's failure — the app it drove
    crashed — not a harness fault."""


#: The launches whose death a failed phase already reported (see `tag`).
_reported: set[str] = set()

_TAG_RE = re.compile(r"\[app launch (\S+)\]")


def tag(launch: object) -> str:
    """The token a driver puts on a death it reports some other way — windows'
    ack-timeout verdict `app process: DEAD — …` — naming the launch that died
    (the bridge's `launched_at`, unique per launch). A phase that FAILS with
    the token in its error has reported that death, so a close that finds the
    same dead launch afterwards does not fail the next test for it again: the
    measured double count was an ack timeout that already said DEAD, then the
    `app` fixture's relaunch erroring the innocent test after it. A token in
    an error that was swallowed — the reset the fixture recovers from — marks
    nothing, which is the case this module exists for."""
    return f"[app launch {launch}]"


def record(text: str, *, launch: object = None) -> None:
    """Record a death a close found. ``text``'s first line is the verdict;
    ``launch`` names the dead launch (see `tag`).

    Printed at once as well, so the finding is in the phase's captured output
    beside the step that met it, even if a later step of the same phase hangs."""
    first = text.splitlines()[0]
    if launch is not None and str(launch) in _reported:
        print(f"[e2e] app death already reported by an earlier failure — {first}", flush=True)
        return
    _found.append(text)
    print(f"[e2e] APP DIED BEFORE ITS CLOSE — {first}", flush=True)


def _mark_reported(exc: BaseException) -> None:
    """Every launch a failing phase's error (and the errors it chains) names."""
    seen: set[int] = set()
    while exc is not None and id(exc) not in seen:
        seen.add(id(exc))
        _reported.update(_TAG_RE.findall(str(exc)))
        exc = exc.__cause__ or exc.__context__


def drain() -> list[str]:
    """Every death recorded since the last drain, and forget them."""
    found = list(_found)
    _found.clear()
    return found


def _raise_found(phase: str) -> None:
    found = drain()
    if not found:
        return
    text = found[0] if len(found) == 1 else (
        f"{len(found)} apps died before their close in this {phase} phase — "
        + found[0].splitlines()[0]
        + "\n\n"
        + "\n\n".join(found)
    )
    if phase == "setup" and _last_finished:
        first, _, rest = text.partition("\n")
        text = (
            f"{first}\nfound in this test's setup; the last test to finish before it "
            f"was {_last_finished}, whose app state the setup was clearing\n{rest}"
        )
    raise AppDiedBeforeClose(text)


def _phase(phase: str):
    """The body of each phase wrapper: run the phase, then raise what it found.

    A phase that already failed keeps its failure as the death's ``__context__``
    — a reset the dead app never acknowledged, a skip saying the bridge could
    not recover — so the report leads with the cause and still shows the
    symptom. A session exit or an interrupt is never replaced: the death was
    already printed when it was recorded."""
    try:
        result = yield
    except (KeyboardInterrupt, SystemExit, pytest.exit.Exception):
        raise
    except BaseException as exc:
        _mark_reported(exc)
        _raise_found(phase)
        raise
    _raise_found(phase)
    return result


@pytest.hookimpl(wrapper=True)
def pytest_runtest_setup(item):
    return (yield from _phase("setup"))


@pytest.hookimpl(wrapper=True)
def pytest_runtest_call(item):
    return (yield from _phase("call"))


@pytest.hookimpl(wrapper=True)
def pytest_runtest_teardown(item, nextitem):
    global _last_finished
    try:
        return (yield from _phase("teardown"))
    finally:
        _last_finished = item.nodeid

r"""tier_3: a stall of the windows app's UI thread names what holds it.

`docs/goal/architecture/e2e-conventions.md` convention 11 (the stall stack) and
`e2e-self-diagnosing-failures.md` convention 6 (a failure names the right
suspect), the windows app's leg. The app is a C# process that beats no
`fauna_e2e_agent` heartbeat, so the FlaUI bridge judges a stall from outside
(`flaui-bridge/StallWatch.cs`): an op still in flight after 25 s is examined, the
verdict decided from the app's CPU and its UI thread's scheduler state, and on the
app-side verdict ALONE the UI thread's native stack is captured by the shared Rust
capture (`fauna-e2e-stall-capture`, `fauna_e2e_agent::capture_thread_of_process`).
The op's failure reply then carries all of it.

The stall is forced inside `fauna_ffi.dll`: the TestAgent's `e2e_stall_ui_thread`
command parks the UI thread in `fauna_e2e_stall_fixture_parks_ui_thread`
(`libs/fauna-ffi/src/e2e_stall.rs`), a frame no other code has. So the first test
proves the capture read THIS thread of THAT process, symbolized from the PDB: no
`ntdll!NtDelayExecution` line can stand in for the fixture's `file:line`.

Measured while building it: UIA gives up on the held thread after ~2 s, so the op
FAILS FAST — a 500 whose UIA timeout the bridge judges on the spot — rather than
staying in flight for the watcher's 25 s.

The second proves the same report rides on a READ that once answered "absent":
`driver.is_absent` (windows' `count == 0`) fails on the held thread instead of
answering True, because no bridge catch that answers for an element swallows a
UIA timeout (`flaui-bridge/StallWatch.cs::UiaSwallow`).

The third proves the other half of the verdict: the same stall with the bridge's
readings faked to the starved-box reading gets the box verdict and no stack — a
stack attached to a starved stall would send a reader hunting a blocking call that
does not exist.

The verdict's every corner is pinned with no app in
`test_bridge_stall_verdict_windows.py`; the capture's own mechanics in
`libs/fauna-e2e-agent/src/stall.rs`'s tests.
"""

from __future__ import annotations

import re
import sys
import time
from collections import Counter
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

#: The bridge's reply budget for these launches, shortened from its 100 s
#: default (`StallWatch.ReplyBudgetEnv`) so each case spends ~40 s.
REPLY_BUDGET_S = 40.0
#: The park outlives the reply, so the op is still held when the bridge answers
#: it, and ends soon after, so the app is usable again for teardown.
PARK_S = REPLY_BUDGET_S + 20.0
#: How long the test keeps issuing ops before the park starts holding one. The
#: park is enqueued on the UI thread by an acked command, so the first op or two
#: may be served before it runs; named and generous (convention 14).
PARK_START_BUDGET_S = 30.0

FIXTURE_FRAME = "fauna_e2e_stall_fixture_parks_ui_thread"
#: The fixture's frame with its source line: the symbolized form only the PDB gives.
FIXTURE_FRAME_WITH_LINE = re.compile(
    re.escape(FIXTURE_FRAME) + r".*e2e_stall\.rs:\d+"
)


@pytest.fixture
def driver():
    d = win.WindowsBridgeDriver()
    yield d
    d.teardown()


def _launch(driver, windows_app_path: str, **env: str) -> None:
    driver.launch({
        "app_path": windows_app_path,
        "args": "",
        "environment": {"FAUNA_E2E_BRIDGE_STALL_REPLY_S": str(REPLY_BUDGET_S), **env},
    })


def _raw_text_read(driver):
    # A raw bridge op: the subject is the bridge's own reply to an app op, with
    # no driver retry layered over it.
    return driver._get("/element/text", {"id": "error-message"})


def _stall_and_fail_an_op(driver, read=_raw_text_read) -> str:
    """Park the UI thread inside `fauna_ffi`, then `read` an element until an op
    is held by the park; return the bridge's failure text for it.

    A `LookupError` (404) is an op the UI thread ANSWERED — the element is simply
    not on this page — so it is not the failure this waits for; nor is any other
    answer. Every answer is tallied, so a route that answers a held UI thread as
    an absent element (a count of 0) says so in the failure."""
    driver.call_command("e2e_stall_ui_thread", {"millis": int(PARK_S * 1000)})
    deadline = time.monotonic() + PARK_START_BUDGET_S
    answers: Counter[str] = Counter()
    while time.monotonic() < deadline:
        try:
            answers[repr(read(driver))] += 1
        except LookupError:
            answers["404 (not on this page)"] += 1
        except RuntimeError as failed:
            return str(failed)
    state = driver._get("/app/state")
    pytest.fail(
        f"the park never failed an op: {sum(answers.values())} op(s) were answered in "
        f"{PARK_START_BUDGET_S:.0f}s after `e2e_stall_ui_thread` acked, as {dict(answers)} "
        "— either the app never took the command, or the route answered the held UI "
        f"thread as if the element were absent. app state: {state}"
    )


def test_an_app_side_stall_names_the_frame_that_holds_the_ui_thread(driver, windows_app_path):
    _launch(driver, windows_app_path)

    report = _stall_and_fail_an_op(driver)
    print(report)

    # UIA gives up on the held thread in ~2 s, so the op fails as a 500 carrying
    # the verdict; a 504 is the watcher's reply to one that stayed in flight.
    assert re.search(r"Bridge error \((500|504)\)", report), report
    assert "UI-THREAD STALL (the windows app" in report, report
    assert "synchronous work ON THE UI THREAD" in report, (
        "a parked UI thread in an app that kept running is the app's own stall:\n" + report
    )
    assert "UI-THREAD STALL STACK (pid " in report, (
        "the app-side verdict must carry the UI thread's stack:\n" + report
    )
    frames = [line for line in report.splitlines() if re.match(r"\s+#\d\d ", line)]
    assert any(FIXTURE_FRAME_WITH_LINE.search(line) for line in frames), (
        f"the stack must name `{FIXTURE_FRAME}` with its e2e_stall.rs line — OS frames "
        "alone (ntdll's NtDelayExecution) do not say what holds the thread:\n" + report
    )


def test_a_held_ui_thread_is_not_read_as_an_absent_element(driver, windows_app_path):
    """`driver.is_absent` — windows' `count == 0` over `/element/count`, the read a
    test makes while it waits for something to disappear — FAILS on a held UI
    thread, carrying the same verdict, rather than answering True: a count of 0
    for a thread that never answered would pass that wait while the app is
    wedged."""
    _launch(driver, windows_app_path)

    report = _stall_and_fail_an_op(driver, lambda d: d.is_absent("error-message"))
    print(report)

    assert re.search(r"Bridge error \((500|504)\)", report), report
    assert "UI-THREAD STALL (the windows app" in report, report
    assert "synchronous work ON THE UI THREAD" in report, report
    assert "UI-THREAD STALL STACK (pid " in report, (
        "the app-side verdict must carry the UI thread's stack on a count too:\n" + report
    )


def test_a_starved_stall_gets_the_box_verdict_and_no_stack(driver, windows_app_path):
    _launch(driver, windows_app_path, FAUNA_E2E_BRIDGE_STALL_READINGS="starved")

    report = _stall_and_fail_an_op(driver)
    print(report)

    assert re.search(r"Bridge error \((500|504)\)", report), report
    assert "readings FAKED by FAUNA_E2E_BRIDGE_STALL_READINGS=starved" in report, report
    assert "the box starved it" in report and "not an app finding" in report, report
    assert "UI-THREAD STALL STACK" not in report and FIXTURE_FRAME not in report, (
        "a starved stall must never carry a stack:\n" + report
    )

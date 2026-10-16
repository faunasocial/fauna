"""tier_1: `drivers/android.py`'s click-as-commit routing, against recorders.

Authority: `docs/goal/ui/devices.md` § Custody facet (piece 3 — the held
custody's adjustable budget, `custody-held-budget-input`). The shared action
layer commits a commit-on-Enter input with the type-then-click idiom
(`actions/custody.py::set_held_budget`). A UiAutomator tap only focuses a
Compose text field, so the android driver turns a click on exactly the
`COMMIT_ON_ENTER_INPUTS` ids into the field's own commit gesture — a hardware
Enter, which a single-line field performs as its IME action (the app half is
Robolectric-pinned by `DevicesRosterContentTest.aHardwareEnterCommitsTheBudgetToo`).
Every other click still taps through the bridge.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


@pytest.fixture
def recorded():
    driver = AndroidBridgeDriver()
    calls: list[tuple] = []
    driver._post_with_scroll = lambda path, body: calls.append(("post", path, body))
    return driver, calls


def test_a_click_on_the_budget_input_presses_enter(recorded):
    driver, calls = recorded
    driver.click("custody-held-budget-input")
    assert calls == [
        ("post", "/element/key", {"id": "custody-held-budget-input", "key": "Enter"}),
    ]


def test_every_other_click_still_taps(recorded):
    driver, calls = recorded
    driver.click("custody-held-stop-button")
    assert calls == [("post", "/element/click", {"id": "custody-held-stop-button", "index": 0})]

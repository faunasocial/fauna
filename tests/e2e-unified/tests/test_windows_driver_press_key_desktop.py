"""tier_1: a key press the bridge could only send as physical input, on a session with
no attached desktop, is a declared environment skip — never a bare HTTP 500.

The FlaUI bridge's `PressKey` has no UIA path: once the element is on a key's route it
always injects the key with `SendInput` (`Actions.cs`, `PhysicalOnly`), which needs an
ATTACHED interactive desktop. On a WTS-disconnected session (`query session` -> `Disc`,
the state the background tester runs in on Windows) it fails with "Access is denied", and
the bridge answers 500 naming that cause. Measured: both conversation lightbox witnesses
went red on the tester at exactly that Escape. The driver therefore asks the session's
own connect state when, and only when, the bridge reports that failure — so:

* on a disconnected session the press is `skip_environment` (convention 7), as the
  driver's `double_click` already declares;
* on an attached session the bridge's error still surfaces, since there it names a
  real input fault;
* a press the bridge refuses before any input ("not actuable", the inert-element 409)
  is never turned into a skip — that refusal needs no desktop and is the observable
  `test_critical_alert_lifetime.py` witnesses.

Pure state: the base `press_key` and the connect-state probe are replaced with canned
answers and the driver is built without launching anything.
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from _pytest.outcomes import Skipped  # noqa: E402

import helpers.windows_session as windows_session  # noqa: E402
from drivers.http_bridge import HttpBridgeDriver  # noqa: E402
from drivers.windows import WindowsBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

PHYSICAL_DENIED = (
    "Bridge error (500): System.InvalidOperationException: press Escape on "
    "image-lightbox has no UIA equivalent and requires the physical SendInput path, "
    "which failed with Win32 error 5 \"Access is denied\" after exhausting its "
    "foreground-retry budget."
)


def _driver(monkeypatch, *, raises, state):
    def press(self, element_id, key, *, scope=None):
        raise raises

    monkeypatch.setattr(HttpBridgeDriver, "press_key", press)
    monkeypatch.setattr(windows_session, "connect_state", lambda: state)
    return object.__new__(WindowsBridgeDriver)


def test_a_physical_press_on_a_disconnected_session_is_an_environment_skip(monkeypatch):
    driver = _driver(
        monkeypatch,
        raises=RuntimeError(PHYSICAL_DENIED),
        state=(windows_session.WTS_DISCONNECTED, "Disconnected"),
    )
    with pytest.raises(Skipped, match="WTS-disconnected"):
        driver.press_key("image-lightbox", "Escape")


def test_a_physical_press_failing_on_an_attached_session_still_fails(monkeypatch):
    driver = _driver(
        monkeypatch,
        raises=RuntimeError(PHYSICAL_DENIED),
        state=(windows_session.WTS_ACTIVE, "Active"),
    )
    with pytest.raises(RuntimeError, match="requires the physical SendInput path"):
        driver.press_key("image-lightbox", "Escape")


def test_a_not_actuable_refusal_is_never_a_skip(monkeypatch):
    refusal = RuntimeError(
        "Bridge error (409): press_key \"Enter\" on critical-alert[0] is not actuable"
    )
    driver = _driver(
        monkeypatch,
        raises=refusal,
        state=(windows_session.WTS_DISCONNECTED, "Disconnected"),
    )
    with pytest.raises(RuntimeError, match="not actuable"):
        driver.press_key("critical-alert", "Enter")


def test_a_press_that_lands_asks_nothing(monkeypatch):
    def probe():
        raise AssertionError("a press that landed must not probe the session")

    monkeypatch.setattr(HttpBridgeDriver, "press_key", lambda self, i, k, *, scope=None: None)
    monkeypatch.setattr(windows_session, "connect_state", probe)
    object.__new__(WindowsBridgeDriver).press_key("image-lightbox", "Escape")


# ── The commit a click makes first: the same rule (test_folders' tester red) ──

COMMIT_DENIED = (
    "Bridge error (500): System.InvalidOperationException: commit of <unknown element> "
    "failed. The UIA focus-shift commit was not used: SetFocus on blur target "
    "<unknown element> threw COMException. The physical-Enter fallback then failed: "
    "Access is denied."
)


def _clicking(monkeypatch, *, raises, state):
    def click(self, element_id, index=0, *, scope=None):
        raise raises

    monkeypatch.setattr(HttpBridgeDriver, "click", click)
    monkeypatch.setattr(windows_session, "connect_state", lambda: state)
    return object.__new__(WindowsBridgeDriver)


def test_a_click_whose_commit_needs_physical_enter_on_a_disconnected_session_skips(monkeypatch):
    driver = _clicking(monkeypatch, raises=RuntimeError(COMMIT_DENIED),
                       state=(windows_session.WTS_DISCONNECTED, "Disconnected"))
    with pytest.raises(Skipped, match="WTS-disconnected"):
        driver.click("folder-member-save-button")


def test_a_click_whose_commit_fails_on_an_attached_session_still_fails(monkeypatch):
    driver = _clicking(monkeypatch, raises=RuntimeError(COMMIT_DENIED),
                       state=(windows_session.WTS_ACTIVE, "Active"))
    with pytest.raises(RuntimeError, match="physical-Enter fallback then failed"):
        driver.click("folder-member-save-button")


def test_any_other_click_error_is_never_a_skip(monkeypatch):
    driver = _clicking(monkeypatch, raises=RuntimeError("Bridge error (404): no element"),
                       state=(windows_session.WTS_DISCONNECTED, "Disconnected"))
    with pytest.raises(RuntimeError, match="no element"):
        driver.click("folder-member-save-button")

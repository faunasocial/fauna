"""tier_3 E2E: a harness-launched FaunaApp never takes the keyboard focus.

e2e convention 10 (`docs/goal/architecture/e2e-launch-isolation.md`, *An app
launch is isolated from the box it runs on*), the windows focus axis. The person
working on Windows does so over RDP in the SAME Windows session that every
harness-launched FaunaApp lives in, so a launch that activates its window, or a
bridge gesture that moves the foreground, takes the keyboard focus from whatever
they are typing into — dozens of times per run. linux closes this channel with a
throwaway Xvfb display; windows closes it at the two places focus can move:

* **the launch** — the driver passes `--e2e-no-activate`
  (`drivers/windows.py::NO_ACTIVATE_ARG`) and a test-agent build SHOWS its window
  without activating it and marks it `WS_EX_NOACTIVATE`
  (`App.xaml.cs::ShowLaunchWindow`) — the second half because WinUI's text peer
  focuses its `TextBox` before a ValuePattern `SetValue`, which activates an
  ordinary inactive window (this case measured it: red at the typing step with
  only the first half in place);
* **the gestures** — every UIA gesture the bridge prefers (Invoke, Toggle,
  SelectionItem, ExpandCollapse, ValuePattern) is a COM call into the provider
  and never moves the foreground. What does move it is recorded by the bridge
  (`Actions.RecordForegroundTake`): the physical `SendInput` fallbacks and UIA
  `SetFocus`, which activates the containing window. Those take the foreground
  by necessity and are convention 10's named fallbacks, not UIA paths.

**Three witnesses, one per link.** The app's own published decision
(`state["launch"]["window_activated"]`) proves the launch took the no-activate
branch; the bridge's `takes` record proves the journey used only UIA gestures;
and the OS's `GetForegroundWindow`, read by the bridge in this same session,
proves the effect — the app never owned the foreground. The last one needs an
attached session: a disconnected one has no foreground to take (it reads 0), so
there it proves nothing and the first two carry the case alone.
"""

from __future__ import annotations

import time

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]


def _launch_decision(driver, timeout: float = 90) -> dict:
    """The app's published launch decision. The key appears only after the
    activation gate at the foot of `OnLaunched` ran — a causal barrier, not a
    settle-wait (convention 14)."""
    state = driver.wait_for_state(
        lambda s: isinstance(s.get("launch"), dict), timeout=timeout
    )
    return state["launch"]


def _assert_landed(feed, text: str) -> None:
    assert feed.wait_for_post_text(text, timeout_s=60), (
        f"the post {text!r} never rendered — the journey did not complete, so the "
        f"foreground checks after it would prove nothing. "
        f"error={feed.driver_error_text()!r}"
    )


def _foreground_line(report: dict) -> str:
    return (
        f"foreground hwnd={report['foreground_hwnd']:#x} "
        f"pid={report['foreground_pid']} (app pid {report['app_pid']})"
    )


def test_a_harness_launched_app_never_takes_the_foreground(logged_in_app):
    """Launch, then a short real journey through UIA gestures only — navigate,
    open the composer (Invoke), type the body (ValuePattern), post (Invoke) —
    and the app never takes the foreground at any point."""
    driver = logged_in_app.driver

    decision = _launch_decision(driver)
    # Whatever the fixture's setup recorded is not this case's subject; the
    # launch's own effect is read from the foreground itself, right now. The
    # EFFECT is asserted before the self-report, so a red names what the person
    # at this desktop actually lost, not only which branch the code took.
    before = driver.foreground_report(clear=True)
    assert not before["app_owns_foreground"], (
        "the launched app owns the foreground: its launch took the keyboard focus "
        f"from the person at this desktop. {_foreground_line(before)}; "
        f"published launch state: {decision!r}"
    )
    assert decision.get("window_activated") is False, (
        "the app ACTIVATED its launch window: a harness launch must only show it "
        "(--e2e-no-activate → AppWindow.Show(activateWindow: false)), or every "
        "launch in a run takes the keyboard focus from the person at this desktop. "
        f"published launch state: {decision!r}"
    )

    feed = logged_in_app.feed
    text = f"no focus steal {time.time_ns()}"
    # One checkpoint per gesture, so a red names the STEP that moved the
    # foreground (convention 6) rather than only that the journey did.
    steps = [
        ("navigate to the feed (nav tab)", feed.navigate),
        ("open the composer (Invoke)", feed.open_composer),
        ("type the body (ValuePattern)",
         lambda: driver.type_text("compose-text-field", text)),
        ("post (Invoke)", lambda: (feed._wait_for_submit_enabled(),
                                   driver.click("post-submit-button"))),
        ("the post lands", lambda: _assert_landed(feed, text)),
    ]
    for label, step in steps:
        step()
        after = driver.foreground_report(clear=True)
        assert after["takes"] == [], (
            f"step {label!r} is a UIA gesture, yet the bridge moved the foreground "
            f"onto the app for: {after['takes']!r}. Each of those took the keyboard "
            "focus from the person at this desktop; a UIA path that reaches a "
            "foregrounding fallback has regressed (flaui-bridge/Actions.cs, "
            "RecordForegroundTake's callers)."
        )
        assert not after["app_owns_foreground"], (
            f"the app took the foreground at step {label!r} with no bridge take "
            f"recorded — the app raised itself. {_foreground_line(after)}"
        )

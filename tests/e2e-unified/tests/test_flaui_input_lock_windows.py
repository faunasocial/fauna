"""The FlaUI bridge's machine-wide physical-input mutex actually protects the victim.

**What this pins.** ``Actions._inputMutex`` (`flaui-bridge/Actions.cs`) serializes the
whole ``ForegroundApp() + SendInput`` section across every FlaUI bridge process on the
desktop. Its claim is *not* "our SendInput succeeds" — ``SendPhysicalInput``'s error-5
retry already covered that — but the INVERSE, which is the damaging one: when a sibling
e2e session wins the foreground race, its in-flight ``Keyboard.Type`` lands in OUR app.
That arrives as a keystroke nobody in this process sent; an ESC closes a ``ContentDialog``
with its confirm handler never run, so a ceremony silently no-ops — modal closed, no RPC,
no error, product code blameless.

**Why it exists at all.** The mutex landed verified not to regress anything and never
verified to do its job, with the gap recorded as "needs a concurrent human-launched
session". A "this needs a human" claim is exactly the kind that has to be attacked before
it is believed, and here it is plainly false: two ``WindowsBridgeDriver`` instances live
happily in one pytest process, so the collision and its fix are headlessly testable. The
only thing a human could add is watching it happen.

**Why the collision is deterministic, not a race we hope to lose.** ``type_physical``
holds the input critical section open for a named delay before injecting, so the test
PINS the interleaving instead of retrying until it happens:

    t=0.0s  driver A: acquire → foreground app A → hold 4s → type "aaaaaaaaaa"
    t=1.5s  driver B: acquire → foreground app B →          type "bbbbbbbbbb"

* lock ON  → B blocks until t≈4s; each app receives exactly its own text.
* lock OFF → B foregrounds app B at t=1.5s and types into it; at t=4s A's ten
  ``a``s are injected while app B owns the foreground, so they land in app B and
  app A receives nothing at all.

**Re-verifying that this test is not vacuous** — a lock test that passes with the lock
disabled proves nothing, so the failing mode is reachable on demand::

    FAUNA_E2E_INPUT_LOCK=off python3 -m pytest \
        tests/e2e-unified/tests/test_flaui_input_lock_windows.py -v --app windows

That flips the assertion: the run is GREEN only if the sibling's keystrokes really did
land in the wrong app. Run it after any change to ``SendPhysicalInput`` /
``WithInputLock`` / ``TypePhysical``. It injects letters (never ESC or Enter) while
foregrounding our own two apps, so a stray keystroke reaching a sibling session's app is
harmless text — but it IS unserialized by construction, so prefer a quiet box.

Goal doc: `docs/goal/architecture/e2e-conventions.md` convention 9 (machine-wide locks —
the windows physical-input mutex) and convention 14, owned by
`e2e-latency-independent-assertions.md` (the assertions below are on CONTENT, never on
wall-clock timing; the one sleep is the critical section being held open on purpose).

tier_2: real driver, real app binaries, no nest — the wizard only walks
``identity_choice`` → ``identity_created`` → ``recovery_kit`` → ``handle_entry``, all
of it client-side.
"""

from __future__ import annotations

import os
import threading
import time

import pytest

from drivers import create_driver
from helpers.windows_session import skip_unless_attached_desktop

pytestmark = [pytest.mark.tier_2, pytest.mark.windows]

# tests/e2e-unified/ui.yaml § onboarding.
CREATE_IDENTITY_BUTTON = "create-identity-button"
SECRET_KEY_DISPLAY = "secret-key-display"
IDENTITY_CONTINUE_BUTTON = "identity-continue-button"
RECOVERY_KIT_SKIP_BUTTON = "recovery-kit-skip-button"
HANDLE_INPUT = "handle-input"

# Distinguishable, and legal handle characters so nothing normalizes them away.
TEXT_A = "aaaaaaaaaa"
TEXT_B = "bbbbbbbbbb"

# How long driver A holds the input critical section open before injecting, and how
# long the test waits before firing driver B into it. Both are margins around a
# construction, not latency budgets: any B-start strictly inside A's hold produces the
# same interleaving, so seconds of scheduling jitter on a loaded box change nothing.
HOLD_MS = 4000
B_START_DELAY_S = 1.5

# Generous ceiling for the app to surface typed characters through UIA. A green run
# pays only real latency (convention 14) — the poll exits as soon as both fields have
# stopped growing at the expected total.
READBACK_BUDGET_S = 15.0


def _to_handle_entry(driver) -> None:
    """Drive a freshly launched app to ``handle_entry`` — no nest involved.

    ``create_identity`` → ``identity_created`` → Continue → ``recovery_kit`` → Skip
    → ``handle_entry`` (`onboarding.md` § The pages: Continue lands on the recovery
    kit, never straight on ``handle_entry``), the same route
    ``OnboardingActions.skip_recovery_kit`` takes. Skip, not "I've saved it": the
    test needs only the handle field, and skipping keeps no minted root around.
    ``handle-input`` is a plain ``TextBox`` (`HandleEntryView.xaml`), so its
    ValuePattern value is the exact record of what keystrokes this app received.
    """
    driver.wait_for(CREATE_IDENTITY_BUTTON, timeout=90)
    driver.click(CREATE_IDENTITY_BUTTON)
    driver.wait_for(SECRET_KEY_DISPLAY, timeout=30)
    driver.click(IDENTITY_CONTINUE_BUTTON)
    driver.wait_for(RECOVERY_KIT_SKIP_BUTTON, timeout=30)
    driver.click(RECOVERY_KIT_SKIP_BUTTON)
    driver.wait_for(HANDLE_INPUT, timeout=30)
    # Start from a known-empty field: the assertions below are exact-equality on the
    # whole value, and a physical click positions the caret wherever it lands.
    driver.clear_and_type(HANDLE_INPUT, "")


def test_physical_input_lock_keeps_a_siblings_keystrokes_out_of_this_app(
    windows_app_path,
):
    # A forgotten `tscon` hand-off leaves the automation session `Disc`, which
    # fails every SendInput call in this test regardless of how correct the
    # foreground handling is (helpers/windows_session.py) — a declared
    # environment skip, not a bug this test should report.
    skip_unless_attached_desktop()

    unlocked = os.environ.get("FAUNA_E2E_INPUT_LOCK", "").lower() == "off"

    # Two independent bridges, each with its own app, its own credential store and
    # its own data dir (the driver defaults both to a fresh mkdtemp — e2e rule 10).
    # Single-instance is disabled under FAUNA_E2E_BRIDGE, so the second app starts
    # instead of activating the first (`SingleInstanceManager.IsE2E`).
    driver_a = create_driver("windows")
    driver_b = create_driver("windows")
    try:
        driver_a.launch({"app_path": windows_app_path})
        driver_b.launch({"app_path": windows_app_path})
        _to_handle_entry(driver_a)
        _to_handle_entry(driver_b)

        failures: dict[str, BaseException] = {}
        reports: dict[str, dict] = {}

        def hold_then_type() -> None:
            try:
                reports["a"] = driver_a.type_physical(
                    HANDLE_INPUT, TEXT_A, pre_delay_ms=HOLD_MS
                )
            except BaseException as exc:  # reported below, never swallowed
                failures["a"] = exc

        holder = threading.Thread(target=hold_then_type, name="input-lock-holder")
        holder.start()
        try:
            # Fire B squarely inside A's hold. With the lock this blocks until A
            # releases; without it, B foregrounds and types immediately — and A's
            # injection then has nowhere to land but B.
            time.sleep(B_START_DELAY_S)  # sleep-ok: constructs the overlap, asserts nothing
            reports["b"] = driver_b.type_physical(HANDLE_INPUT, TEXT_B)
        finally:
            holder.join(timeout=HOLD_MS / 1000.0 + 120)
        assert not holder.is_alive(), (
            "driver A's physical-type never returned — the bridge is wedged, not "
            "merely serialized (the mutex degrades to unlocked after 30s and its "
            "warning is in the bridge log)"
        )
        if "a" in failures:
            raise AssertionError(
                f"driver A's physical-type failed outright: {failures['a']!r}"
            ) from failures["a"]

        deadline = time.monotonic() + READBACK_BUDGET_S
        got_a = got_b = ""
        while time.monotonic() < deadline:
            got_a = driver_a.get_text(HANDLE_INPUT)
            got_b = driver_b.get_text(HANDLE_INPUT)
            if len(got_a) + len(got_b) >= len(TEXT_A) + len(TEXT_B):
                break
            time.sleep(0.25)

        observed = (
            f"app A got {got_a!r}, app B got {got_b!r}; "
            f"foreground reports A={reports.get('a')!r} B={reports.get('b')!r} "
            "(app_hwnd vs. who owned the foreground after ForegroundApp() and at "
            "injection — equal means we owned it, different means we typed into "
            "someone else's window)"
        )
        # Recorded on every run (`-s`), not just failures: the two values ARE the
        # evidence a future session needs to tell "the mutex held" from "the
        # collision never happened", and re-deriving them costs a full run.
        print(f"[input-lock] lock={'off' if unlocked else 'on'}: {observed}")

        if unlocked:
            # Diagnostic mode: the SAME interleaving must corrupt the victim once the
            # mutex is gone. Green here is what makes the locked assertions below
            # meaningful rather than a test that would pass either way. The claim is
            # the corruption, not delivery: unserialized injection can drop a
            # keystroke or two in the foreground handover (measured 2026-09-28: app B
            # got 'bbbbbbbbbbaaaaaaaa' and the readback budget ran out at 18 chars),
            # so the witnesses are A injecting into a foreign window and A's
            # characters — any of them — landing in B while A gets none of its own.
            report_a = reports.get("a") or {}
            assert report_a.get("foreground_at_inject") not in (
                None,
                report_a.get("app_hwnd"),
            ), (
                "FAUNA_E2E_INPUT_LOCK=off, so app B should have owned the foreground "
                "when driver A injected — A still owned it, so this test does NOT "
                "exercise the collision the mutex removes and its locked-mode green "
                f"proves nothing. {observed}"
            )
            assert "a" in got_b and "a" not in got_a, (
                "FAUNA_E2E_INPUT_LOCK=off and driver A injected into a foreign window, "
                "so A's keystrokes should be in app B and none in app A — they are "
                f"not, so the collision corrupted nothing observable. {observed}"
            )
            return

        assert got_a == TEXT_A, (
            "app A must receive exactly its own text; anything else means its "
            "keystrokes were injected while a sibling bridge owned the foreground. "
            f"{observed}"
        )
        assert got_b == TEXT_B, (
            "app B must receive exactly its own text and NO foreign keystroke — a "
            f"sibling's characters in this field are the silent-cancel defect. "
            f"{observed}"
        )
    finally:
        driver_a.teardown()
        driver_b.teardown()

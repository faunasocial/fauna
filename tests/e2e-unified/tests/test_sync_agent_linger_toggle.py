"""The headless-deployment linger offer — `sync-agent-linger-toggle` on
Settings → Folders (`docs/goal/architecture/apps/sync-agent.md` § Headless
deployment; the three ratified properties in § Implementation status today,
the `loginctl enable-linger` paragraph).

tui only: the offer is tui's alone, by design (the linux, windows and macOS
apps declare its absence). Linux only in effect: the test skips, with its
reason, wherever `loginctl` is absent or its reading is not a clean
`yes`/`no` — the `Unavailable` case, where the toggle must render nothing at
all, which this test then asserts instead of flipping anything.

What it witnesses, against the machine's own reading
(`loginctl show-user <user> --property=Linger --value`), never against the
app's word: the toggle's state equals the reading on page entry; after each
flip the two still agree (non-optimistic — a polkit refusal must leave the
toggle showing the truth, not the click); and the flip is reversible, so the
test turns it back and leaves the box exactly as it found it.

**Machine safety.** Linger is a machine-level, per-user setting, and on a dev
box the whole fleet — every session, build and test — may live inside the
user's systemd manager, which logind tears down when the last login session
closes while linger is OFF. So when the box starts with linger ON, the window
in which it reads OFF is kept to seconds and bounded even if this process is
killed outright (where no ``finally`` runs): before the first flip the test
arms a transient user timer that re-enables linger after
``_RESTORE_GUARD_S``, independent of pytest, and disarms it once the restore
is confirmed. If that guard cannot be armed the test skips rather than flip.
Re-enabling is harmless whatever happens (it only keeps things running), so a
guard that fires late changes nothing on a box that started ON. A box that
starts OFF needs no guard: enabling is the harmless direction.
"""

from __future__ import annotations

import os
import pwd
import shutil
import subprocess

import pytest

from helpers.app_surface import skip_environment
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

TOGGLE = "sync-agent-linger-toggle"

# The bound on the OFF window if the test is killed between the flip and the
# restore. Generous enough that a slow loaded-box flip-and-restore never races
# its own guard; short enough that the fleet is unexposed only briefly.
_RESTORE_GUARD_S = 120

# The toggle re-reads `loginctl` synchronously in the flip's own action
# handler, so a settled reading is one frame away; the budget only covers a
# loaded box (green runs pay none of it — the poll exits on the first match).
_SETTLE_S = 20


def _user() -> str:
    return pwd.getpwuid(os.getuid()).pw_name


def _loginctl_linger() -> str | None:
    """The machine's own reading: ``"yes"``, ``"no"``, or None when it is not
    a clean answer (the app's `Unavailable`)."""
    try:
        out = subprocess.run(
            ["loginctl", "show-user", _user(), "--property=Linger", "--value"],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    value = out.stdout.strip()
    return value if value in ("yes", "no") else None


def _set_linger_directly(want: str) -> None:
    """The last-resort restore, used only when the app could not put the box
    back itself — so the box is never left changed by a failing test."""
    verb = "enable-linger" if want == "yes" else "disable-linger"
    subprocess.run(["loginctl", verb], capture_output=True, timeout=15)


def _arm_restore_guard() -> str | None:
    """Arm a transient user timer that re-enables linger after
    ``_RESTORE_GUARD_S``; return its unit name, or None if it could not be armed."""
    unit = f"fauna-e2e-linger-restore-{os.getpid()}"
    try:
        out = subprocess.run(
            [
                "systemd-run",
                "--user",
                "--collect",
                f"--unit={unit}",
                f"--on-active={_RESTORE_GUARD_S}",
                "loginctl",
                "enable-linger",
            ],
            capture_output=True,
            text=True,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return unit if out.returncode == 0 else None


def _disarm_restore_guard(unit: str) -> None:
    subprocess.run(
        ["systemctl", "--user", "stop", f"{unit}.timer", f"{unit}.service"],
        capture_output=True,
        timeout=15,
    )


def _toggle_reading(driver) -> str | None:
    """The toggle's state mapped onto loginctl's vocabulary."""
    state = driver.get_attr(TOGGLE, "state")
    return {"on": "yes", "off": "no"}.get(state or "")


def _flip_and_check_agreement(driver, before: str) -> str:
    """Click the toggle once; return the reading both sides settled on.

    Passing means the toggle and the machine agree after the flip. On a box
    whose polkit lets the user linger themselves the reading moves; on one
    that refuses, it stays, and the toggle must stay with it — either way the
    toggle never shows the click instead of the truth."""
    driver.click(TOGGLE)
    try:
        wait_until(lambda: _toggle_reading(driver) not in (None, before), _SETTLE_S)
    except AssertionError:
        pass  # a refused flip leaves both sides where they were; checked below
    toggle = _toggle_reading(driver)
    machine = _loginctl_linger()
    assert toggle == machine, (
        f"after flipping from {before!r} the toggle reads {toggle!r} but "
        f"loginctl reads {machine!r} — the toggle must show what the computer "
        "actually did (non-optimistic)"
    )
    return machine


@pytest.mark.tui
@pytest.mark.feature("local-folder-sync")
def test_the_linger_switch_shows_what_loginctl_did_and_turns_back(logged_in_app):
    if shutil.which("loginctl") is None:
        skip_environment("no loginctl on this machine — there is no lingering concept to offer")

    original = _loginctl_linger()
    driver = logged_in_app.driver
    logged_in_app.backups.navigate_folders()
    driver.wait_for("page-heading", timeout=_SETTLE_S)

    if original is None:
        # `Unavailable`: the toggle renders nothing at all, never a dead
        # off-looking switch and never a guess either way.
        assert driver.is_absent(TOGGLE), (
            "loginctl gives no clean Linger reading here, yet the toggle rendered"
        )
        skip_environment("loginctl reads no clean Linger value here (Unavailable) — nothing to flip")

    driver.wait_for(TOGGLE, timeout=_SETTLE_S)
    assert _toggle_reading(driver) == original, (
        f"on page entry the toggle reads {_toggle_reading(driver)!r} but loginctl "
        f"reads {original!r}"
    )

    guard = None
    if original == "yes":
        guard = _arm_restore_guard()
        if guard is None:
            skip_environment(
                "could not arm the linger restore guard (no systemd user manager "
                "reachable) — refusing to turn linger off unguarded on this box"
            )

    try:
        flipped = _flip_and_check_agreement(driver, original)
        if flipped != original:
            # The flip took: turn it back through the app — the reversibility
            # half of the outcome — and check agreement once more.
            restored = _flip_and_check_agreement(driver, flipped)
            assert restored == original, (
                f"turning the switch back left loginctl at {restored!r}, "
                f"not the {original!r} it started at"
            )
    finally:
        if _loginctl_linger() != original:
            _set_linger_directly(original)
        if guard is not None and _loginctl_linger() == original:
            _disarm_restore_guard(guard)

    assert _loginctl_linger() == original, "the box's Linger setting was not left as found"

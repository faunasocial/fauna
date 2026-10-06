"""ExecCondition anti-flap guard for the sync-agent user unit — REAL session.

`apps/fauna-linux/src/sync_agent.rs::unit_contents` writes a systemd **user**
unit that guards the same per-channel target with BOTH `ConditionFileIsExecutable=`
(unit-level) AND `ExecCondition=` (service-level). The `ExecCondition` line
exists because `Restart=always` auto-restarts do NOT re-evaluate `Condition*=` —
only an initial / explicit / boot start does. Without it, a *running* always-on
agent whose channel is uninstalled (Flatpak's `flatpak uninstall` SIGKILLs the
instance) leaves the unit Restart-flapping forever: each auto-restart re-runs
`ExecStart` (`flatpak run` → "not installed" → exit 1) every `RestartSec=5`, and
because 5 × 5 s = 25 s outruns the default 10 s `StartLimitIntervalSec` window
the start limit never trips. `ExecCondition` re-runs on every start attempt
including auto-restarts, so the auto-restart condition-skips (journal: "Skipped
due to 'exec-condition'") and the unit settles cleanly `inactive`.

This is the FAST, flatpak-free regression for that mechanism: it drives the real
`systemd --user` manager directly with a throwaway unit shaped like the
production template (a stub `ExecStart` standing in for the channel binary /
`flatpak run`), in ~seconds rather than the ~minutes a full Flatpak build+seam
run costs. The two other halves of the coverage:
  - the *emitted string* (that the app writes `ExecCondition=` for every
    channel) is pinned by the Rust unit tests in `sync_agent.rs`;
  - the end-to-end proof (that the real app writes+drives this over the session
    bus) is `test_sync_agent_flatpak_seam.py` step 5.

`RestartSec=1` here (vs. production 5) only speeds the observation; it also
bounds the condition-only "bug" variant (the start limit trips at 5 restarts),
so nothing flaps past the test. Uses a throwaway unit name distinct from the
real `fauna-sync-agent.service`, and borrows-and-gives-back per the category
contract (conftest.py).
"""

import os
import subprocess
import time
from pathlib import Path

import pytest

# tier_3: a real, unmocked, out-of-process integration test against a real OS
# subsystem (the systemd user manager). No fauna binary, driver, or Docker image
# is involved, so it is lighter than the tier_4 Flatpak seam sibling; tier_3 is
# the taxonomy's default-bias bucket for "full real stack, nothing stubbed".
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_session,
    pytest.mark.linux,
    pytest.mark.timeout(300),
]

# Deliberately NOT `fauna-sync-agent.service` — this test writes its own unit
# and must never touch a real native install's unit slot.
UNIT = "fauna-sync-agent-flap-probe.service"
UNIT_PATH = Path.home() / ".config" / "systemd" / "user" / UNIT
TARGET = Path("/work/tmp") / f"fauna-flap-probe-target-{os.getpid()}"


def _systemctl(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["systemctl", "--user", *args], capture_output=True, text=True
    )


def _is_active() -> str:
    return _systemctl("is-active", UNIT).stdout.strip()


def _nrestarts() -> int:
    return int(_systemctl("show", UNIT, "-p", "NRestarts", "--value").stdout.strip() or "0")


def _write_unit(*, with_exec_condition: bool) -> None:
    """A unit shaped like `sync_agent.rs::unit_contents`: Condition on the
    target, Restart=always, and a stub ExecStart that succeeds while the target
    is present and fails once it is gone (mirrors `flatpak run` → "not
    installed"). `RestartSec=1` for a fast, bounded observation."""
    exec_condition = (
        f'ExecCondition=/usr/bin/test -x "{TARGET}"\n' if with_exec_condition else ""
    )
    UNIT_PATH.parent.mkdir(parents=True, exist_ok=True)
    UNIT_PATH.write_text(
        "[Unit]\n"
        "Description=fauna sync-agent flap probe\n"
        f"ConditionFileIsExecutable={TARGET}\n"
        "\n"
        "[Service]\n"
        "Type=simple\n"
        f"{exec_condition}"
        f"ExecStart=/bin/sh -c 'test -x \"{TARGET}\" && exec sleep 300 || exit 1'\n"
        "Restart=always\n"
        "RestartSec=1\n"
        "\n"
        "[Install]\n"
        "WantedBy=default.target\n"
    )
    _systemctl("daemon-reload")


def _teardown() -> None:
    _systemctl("stop", UNIT)
    _systemctl("reset-failed", UNIT)
    UNIT_PATH.unlink(missing_ok=True)
    _systemctl("daemon-reload")
    TARGET.unlink(missing_ok=True)


def _start_running_agent() -> None:
    """Target present → the unit starts and stays active (agent 'running')."""
    TARGET.write_text("#!/bin/sh\n")
    TARGET.chmod(0o755)
    _systemctl("reset-failed", UNIT)
    assert _systemctl("start", UNIT).returncode == 0
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and _is_active() != "active":
        time.sleep(0.5)
    assert _is_active() == "active", "probe unit should be active with the target present"


def _uninstall() -> None:
    """Mimic a channel uninstall of the *running* agent: the target binary
    disappears AND the running instance is SIGKILLed (what `flatpak uninstall`
    does, status 137)."""
    TARGET.unlink(missing_ok=True)
    _systemctl("kill", "--signal=SIGKILL", UNIT)


def test_execcondition_quiets_the_uninstall_restart_flap():
    """The production shape (ExecCondition present): after the running agent is
    uninstalled, the unit settles cleanly inactive with no ongoing restarts."""
    try:
        _write_unit(with_exec_condition=True)
        _start_running_agent()
        _uninstall()

        # ExecCondition re-checks on the auto-restart → skip → the unit settles
        # inactive (a condition-skipped start is not counted, so NRestarts does
        # not accumulate).
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline and _is_active() != "inactive":
            time.sleep(0.5)
        assert _is_active() == "inactive", (
            "with ExecCondition the unit must settle inactive after uninstall, "
            f"got is-active={_is_active()!r}"
        )
        # And STAY quiet — no accumulating restarts over 2× the flap period.
        settled = _nrestarts()
        time.sleep(5)
        assert _is_active() == "inactive", "unit must not come back active (no flap)"
        assert _nrestarts() == settled, (
            f"unit must not Restart-flap: NRestarts climbed {settled}→{_nrestarts()}"
        )
    finally:
        _teardown()


def test_without_execcondition_the_running_agent_uninstall_flaps():
    """Characterizes the systemd gap that motivates ExecCondition: with only
    `Condition*=`, the auto-restart re-runs ExecStart despite the now-unmet
    condition, so NRestarts climbs.

    If this ever FAILS (NRestarts stays 0), it means this systemd re-checks
    `Condition*=` on auto-restart — good news, but then ExecCondition would be
    redundant: revisit `sync_agent.rs::unit_contents`. It is not fragile on any
    current systemd (verified: auto-restarts skip Condition*= re-checks)."""
    try:
        _write_unit(with_exec_condition=False)
        _start_running_agent()
        _uninstall()

        # The auto-restart ignores the unmet Condition*= and re-runs the doomed
        # ExecStart; NRestarts increments. (RestartSec=1 bounds this — the start
        # limit trips at 5, so it cannot flap past the test.)
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline and _nrestarts() < 2:
            time.sleep(0.5)
        assert _nrestarts() >= 2, (
            "without ExecCondition the auto-restart must re-run ExecStart despite "
            f"the unmet condition (the flap this fix closes); NRestarts={_nrestarts()}"
        )
    finally:
        _teardown()

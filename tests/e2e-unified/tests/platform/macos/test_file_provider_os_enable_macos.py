"""tier_3: the macOS reading of `userEnabled`.

`docs/goal/behavior/on-demand-files-apple.md` § Apple File Provider binding, *The
one-time OS enable*. The box's own domains are read through the Debug app's `list`.
With none, a raw non-ref identifier is registered, listed and removed: a raw
identifier needs no provisioning, and `provision` writes a keychain this box must not
touch. An OS refusal is
the reading, so it is printed, not failed.
"""

import subprocess
import sys

import pytest

from helpers.waiting import wait_until
from tests.platform.macos.test_file_provider_read_path import fauna_fp_host_app  # noqa: F401

pytestmark = [
    pytest.mark.skipif(sys.platform != "darwin", reason="macOS only"),
    pytest.mark.tier_3,
]

_PROBE = "fauna-os-enable-probe"


def _run(fauna: str, *args: str) -> str:
    try:
        proc = subprocess.run([fauna, *args], capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:
        # `remove` has taken over 90 s to exit, the domain already gone.
        print(f"[os-enable] $ Fauna {args[0]} -> no exit within 60 s")
        return ""
    out = (proc.stdout + proc.stderr).strip()
    print(f"[os-enable] $ Fauna {args[0]} -> exit {proc.returncode}: {out}")
    return out


@pytest.mark.timeout(1200)
def test_the_os_answers_user_enabled_on_macos(fauna_fp_host_app):  # noqa: F811
    fauna = fauna_fp_host_app
    listed = _run(fauna, "list")
    assert "domains:" in listed or "list FAILED" in listed, listed
    if "userEnabled=" in listed:
        return
    answers = []

    def answered() -> bool:
        answers.append(_run(fauna, "register", _PROBE, _PROBE))
        return "being invalidated" not in answers[-1]

    # The OS answers Cocoa 4099 until the extension the fixture just replaced has
    # gone; only that answer is waited out.
    wait_until(answered, 120, interval=5.0, diagnose=lambda: answers[-1])
    if "register OK" not in answers[-1]:
        return
    try:
        assert f"domain {_PROBE} userEnabled=" in _run(fauna, "list")
    finally:
        _run(fauna, "remove", _PROBE)
        assert _PROBE not in _run(fauna, "list")

r"""tier_1: the android driver names the app's process by its pid AND its kernel
start time, so "not restarted" cannot be satisfied by a recycled pid.

`AndroidBridgeDriver.app_process_identity` is the in-process stand-in for an
out-of-process sync agent's pid in connect-and-sign-in outcome 15's
background-sync witness (`test_nest_cert_change_same_identity.py`): android's
background sync runs inside the app's own process. Only `adb` is replaced here.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1


def _stat(pid: str, comm: str, start: str) -> str:
    # Fields 3..21 then field 22 (starttime) and a few after it.
    before = ["S", "1", pid, pid, "0", "-1", "4194560"] + ["0"] * 12
    return f"{pid} ({comm}) {' '.join(before)} {start} 123456 789\n"


def _driver(monkeypatch, answer: str):
    d = AndroidBridgeDriver.__new__(AndroidBridgeDriver)
    asked = []

    def shell(script):
        asked.append(script)
        return answer

    monkeypatch.setattr(d, "_device_shell", shell)
    return d, asked


def test_the_identity_is_the_pid_and_the_start_ticks(monkeypatch):
    d, asked = _driver(monkeypatch, _stat("4242", AndroidBridgeDriver.APPLICATION_ID, "987654"))
    assert d.app_process_identity() == "4242@987654"
    assert AndroidBridgeDriver.APPLICATION_ID in asked[0]


def test_a_recycled_pid_reads_as_another_process(monkeypatch):
    first, _ = _driver(monkeypatch, _stat("4242", "social.fauna.fauna", "1000"))
    again, _ = _driver(monkeypatch, _stat("4242", "social.fauna.fauna", "2000"))
    assert first.app_process_identity() != again.app_process_identity()


def test_a_comm_with_spaces_and_parens_still_reads_field_22(monkeypatch):
    d, _ = _driver(monkeypatch, _stat("77", "odd (name) x", "31337"))
    assert d.app_process_identity() == "77@31337"


@pytest.mark.parametrize("answer", ["", "\n", "garbage without a paren\n", "12 (x) S 1\n"])
def test_no_process_or_an_unreadable_stat_is_none(monkeypatch, answer):
    d, _ = _driver(monkeypatch, answer)
    assert d.app_process_identity() is None

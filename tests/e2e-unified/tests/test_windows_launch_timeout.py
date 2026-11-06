r"""tier_1: windows' two launch paths answer a timeout through one builder.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention). The
first-launch gate after `POST /session` raised a bare line naming both suspects
while `recover()` printed the process state, the epoch-fence tally and the app's
log since its mark. The two now build their report in one place, with the driver
supplying the witnesses — so these tests drive each path with only its outward
calls replaced and read the same first line off both.
"""

from __future__ import annotations

import datetime
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402
from helpers import launch_load  # noqa: E402
from helpers.launch_load import MachineWindow  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]

#: The bridge's `o` format, a launch ago: the wall since it is what a share is of.
LAUNCHED_AT = (
    datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=90)
).strftime("%Y-%m-%dT%H:%M:%S.%f0Z")
STARVED_STATUS = {"running": True, "cpu_ms": 3100, "launched_at": LAUNCHED_AT, "wait_samples": 90, "wait_ready_samples": 86}
BUSY_STATUS = {"running": True, "cpu_ms": 60_000_000, "launched_at": LAUNCHED_AT, "wait_samples": 90, "wait_ready_samples": 4}
#: Alive, little CPU and not a thread Ready at any of the bridge's samples: a launch
#: that blocks.
BLOCKED_STATUS = {"running": True, "cpu_ms": 300, "launched_at": LAUNCHED_AT, "wait_samples": 90, "wait_ready_samples": 0}
EPOCH_LAUNCH = "[err] [bridge] launch epoch=4 pid=7 data_dir=X"


@pytest.fixture(autouse=True)
def _machine_reads_fine(monkeypatch):
    """The machine's window has its own tests; here it is one fixed reading."""
    monkeypatch.setattr(
        launch_load,
        "machine_window",
        lambda *a, **k: MachineWindow(
            ticks=18,
            fence_cpus=2,
            fence_cpus_max=2,
            fence_of=6,
            lane_k=2,
            priv_pct=64.0,
            phantom=3.1,
            busy=5.2,
            outside=3.0,
            steal_host=0.2,
        ),
    )


def _log_line(text: str) -> str:
    stamp = datetime.datetime.now(datetime.timezone.utc)
    return f"{stamp.strftime('%Y-%m-%dT%H:%M:%S.%f')}Z  INFO {text}\n"


def _driver(monkeypatch, tmp_path, *, status=None, status_raises=False, bridge_log=()):
    """A `WindowsBridgeDriver` with only its outward calls replaced."""
    d = win.WindowsBridgeDriver()
    d._data_dir = str(tmp_path)
    d._RELAUNCH_READY_BUDGET_S = 0.05

    def _get(path, *a, **k):
        if path != "/session/status":
            raise ConnectionError("no such route here")
        if status_raises:
            raise ConnectionError("bridge is gone")
        return dict(status or {})

    monkeypatch.setattr(d, "_get", _get)
    monkeypatch.setattr(d, "is_visible", lambda *a, **k: False)
    d._bridge_log = list(bridge_log)
    return d


def _write_log(tmp_path, *lines) -> None:
    logs = Path(tmp_path) / "logs"
    logs.mkdir(exist_ok=True)
    with open(logs / "fauna.log.2026-10-30", "a", encoding="utf-8") as fh:
        fh.writelines(lines)


def _stub_relaunch(monkeypatch, d, posted=lambda: None):
    """Everything `recover()` does around its readiness poll, stubbed."""
    d._session_body = {"app": "FaunaApp.exe", "args": ""}
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(d, "_delete", lambda *a, **k: {"closed": True})
    monkeypatch.setattr(d, "_post", lambda *a, **k: posted())
    monkeypatch.setattr(d, "_get_state_raw", lambda *a, **k: None)
    monkeypatch.setattr(d, "_relaunch_principal_slot_carry", lambda *a, **k: None)
    monkeypatch.setattr(d, "_begin_install_device_secret_row_carry", lambda *a, **k: None)
    monkeypatch.setattr(d, "_erase_credential_store_for_relaunch", lambda *a, **k: None)
    monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)


def test_the_first_launch_gate_raises_the_verdict(monkeypatch, tmp_path):
    d = _driver(monkeypatch, tmp_path, status=STARVED_STATUS)

    with pytest.raises(TimeoutError) as raised:
        d._await_first_launch_ready(posted_at=0.0, log_mark=0)

    first = str(raised.value).splitlines()[0]
    assert first.startswith("launch timeout (0 s): "), first
    assert "STARVED — the app ran 3.1 core-s" in first, first
    assert "and waited for a CPU 96 % of them" in first, first


def test_a_blocked_launch_reads_stalled_not_starved(monkeypatch, tmp_path):
    """Alive on little CPU with no thread Ready at any sample: it asked for none,
    so the launch is the app's — never STARVED (the rider's rule since 2026-10-30)."""
    d = _driver(monkeypatch, tmp_path, status=BLOCKED_STATUS)

    with pytest.raises(TimeoutError) as raised:
        d._await_first_launch_ready(posted_at=0.0, log_mark=0)

    report = str(raised.value)
    first = report.splitlines()[0]
    assert "STALLED, NOT STARVED — ran 0.3 core-s, waited for a CPU 0 % of" in first, first
    assert "a thread Ready at 0 of the bridge's 90 one-a-second thread-table reads" in report, report


def test_a_bridge_with_no_wait_samples_reads_unknown_under_the_line(monkeypatch, tmp_path):
    """Under the quarter-core line the wait is the decider; an older bridge that
    reports no `wait_samples` leaves it unread, and the verdict says so."""
    status = {k: v for k, v in STARVED_STATUS.items() if not k.startswith("wait_")}
    d = _driver(monkeypatch, tmp_path, status=status)

    with pytest.raises(TimeoutError) as raised:
        d._await_first_launch_ready(posted_at=0.0, log_mark=0)

    first = str(raised.value).splitlines()[0]
    assert "UNKNOWN — the process's CPU wait: the bridge reports no wait_samples" in first, first


def test_a_bridge_that_sampled_nothing_is_unread_too(monkeypatch, tmp_path):
    status = {**STARVED_STATUS, "wait_samples": 0, "wait_ready_samples": 0}
    d = _driver(monkeypatch, tmp_path, status=status)

    with pytest.raises(TimeoutError) as raised:
        d._await_first_launch_ready(posted_at=0.0, log_mark=0)

    first = str(raised.value).splitlines()[0]
    assert "UNKNOWN — the process's CPU wait: the bridge took no thread-table sample" in first, first


def test_the_gate_still_opens_on_a_published_state(monkeypatch, tmp_path):
    d = _driver(monkeypatch, tmp_path, status=STARVED_STATUS)
    monkeypatch.setattr(d, "_get", lambda *a, **k: {"page": "login"})

    d._await_first_launch_ready(posted_at=0.0, log_mark=0)


def test_the_gate_still_opens_on_the_collision_chooser(monkeypatch, tmp_path):
    d = _driver(monkeypatch, tmp_path, status=STARVED_STATUS)
    monkeypatch.setattr(d, "is_visible", lambda *a, **k: True)

    d._await_first_launch_ready(posted_at=0.0, log_mark=0)


def test_recover_prints_the_same_verdict_and_returns_false(monkeypatch, tmp_path, capsys):
    d = _driver(monkeypatch, tmp_path, status=STARVED_STATUS)
    _stub_relaunch(monkeypatch, d)

    assert d.recover() is False

    out = capsys.readouterr().out
    assert "launch timeout (0 s): STARVED — the app ran 3.1 core-s" in out, out


def test_recover_keeps_its_epoch_fence_and_sweep_notes(monkeypatch, tmp_path, capsys):
    status = {
        **STARVED_STATUS,
        "fenced_state_pushes": 3,
        "last_fenced_epoch": "2",
        "session_epoch": 4,
        "untracked_survivors": ["pid 9 [UNTRACKED]"],
    }
    d = _driver(monkeypatch, tmp_path, status=status)
    _stub_relaunch(monkeypatch, d)
    monkeypatch.setattr(d, "_fenced_push_count", lambda: 0)

    d.recover()

    out = capsys.readouterr().out
    assert "EPOCH-FENCED" in out, out
    assert "pid 9 [UNTRACKED]" in out, out


def test_the_relaunchs_log_mark_is_taken_before_the_post(monkeypatch, tmp_path, capsys):
    """What the new process writes while the POST is still in flight is its own
    record, not its predecessor's: a mark taken after the POST loses the very line
    that dates the launch handler."""
    _write_log(tmp_path, _log_line("the dead instance's retry churn"))
    d = _driver(monkeypatch, tmp_path, status=BUSY_STATUS)
    _stub_relaunch(
        monkeypatch,
        d,
        posted=lambda: _write_log(tmp_path, _log_line("fauna_ffi: fauna client logging initialised")),
    )

    d.recover()

    out = capsys.readouterr().out
    assert 'last line "fauna_ffi: fauna client logging initialised"' in out, out
    assert "the dead instance's retry churn" not in out, out


def test_an_exit_reads_faulted_with_its_phrase(monkeypatch, tmp_path):
    d = _driver(
        monkeypatch,
        tmp_path,
        status={"running": False, "exit_code": -1073740791, "cpu_ms": 900, "launched_at": LAUNCHED_AT},
    )

    report = d._launch_timeout_report(posted_at=0.0, log_mark=0)

    first = report.splitlines()[0]
    assert first.startswith("launch timeout (0 s): FAULTED — exit code -1073740791 (0xC0000409)"), first


def test_a_silent_bridge_reads_unknown(monkeypatch, tmp_path):
    d = _driver(monkeypatch, tmp_path, status_raises=True)

    first = d._launch_timeout_report(posted_at=0.0, log_mark=0).splitlines()[0]

    assert "UNKNOWN — the process: the bridge did not answer /session/status" in first, first


def test_a_bridge_with_no_cpu_reading_reads_unknown(monkeypatch, tmp_path):
    """An app whose handle stopped answering, or a bridge older than the field."""
    d = _driver(monkeypatch, tmp_path, status={"running": True, "launched_at": LAUNCHED_AT})

    first = d._launch_timeout_report(posted_at=0.0, log_mark=0).splitlines()[0]

    assert "UNKNOWN — the process's CPU time: " in first, first


def test_a_predecessors_fatal_is_not_this_launchs(monkeypatch, tmp_path):
    """The bridge outlives the app across relaunches, so its buffer holds the last
    instance's crash: `[bridge] launch epoch=` is where this launch's record starts."""
    d = _driver(
        monkeypatch,
        tmp_path,
        status=BUSY_STATUS,
        bridge_log=["[err] [fauna] FATAL the previous instance died", EPOCH_LAUNCH],
    )

    first = d._launch_timeout_report(posted_at=0.0, log_mark=0).splitlines()[0]

    assert "FAULTED" not in first, first


def test_this_launchs_fatal_is_a_fault(monkeypatch, tmp_path):
    d = _driver(
        monkeypatch,
        tmp_path,
        status=BUSY_STATUS,
        bridge_log=[EPOCH_LAUNCH, "[err] [fauna] FATAL this instance died"],
    )

    first = d._launch_timeout_report(posted_at=0.0, log_mark=0).splitlines()[0]

    assert "FAULTED — [fauna] FATAL this instance died" in first, first


def test_the_ack_path_keeps_its_own_shape(monkeypatch, tmp_path):
    """`ack_timeout_diagnostics` shares pieces with the launch report; it is no
    launch timeout, and its report never reads as one."""
    d = _driver(monkeypatch, tmp_path, status=STARVED_STATUS)

    assert "launch timeout" not in d.ack_timeout_diagnostics()

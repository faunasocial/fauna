r"""tier_1: a launch that times out names its suspect, from four witnesses.

Convention 6's launch-timeout rider
(`docs/goal/architecture/e2e-self-diagnosing-failures.md` § The convention). A
readiness gate that ends in a timeout used to say only that: windows' named both
suspects and neither, and a run that errored on it after 9293 s took a session a
day of reading by hand to settle. `launch_timeout_report` is the one builder every
driver's gate goes through, so the grammar cannot drift per app: the verdict is the
FIRST line, because the live-reason line carries 197 characters of it and a
`[e2e] ERROR` at setup is read from that line alone.

Everything here runs against fakes for every witness; the forced pair that proves
the real thing is `test_launch_timeout_forced_windows.py`.
"""

from __future__ import annotations

import datetime
import json
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.http_bridge import LaunchWitnesses, launch_timeout_report  # noqa: E402
from helpers import launch_load  # noqa: E402
from helpers.app_surface import skip_environment  # noqa: E402
from helpers.launch_load import MachineWindow  # noqa: E402

pytestmark = pytest.mark.tier_1

T0 = 1_790_000_000.0
#: What conftest._print_live_reason keeps of `TimeoutError: <first line>`.
LIVE_REASON_CHARS = 197


def _log_line(at: float, text: str) -> str:
    """One line in the shape `tracing`'s fmt layer writes `fauna.log` in."""
    stamp = datetime.datetime.fromtimestamp(at, tz=datetime.timezone.utc)
    return f"{stamp.strftime('%Y-%m-%dT%H:%M:%S.%f')}Z  INFO {text}"


def _machine(**over) -> MachineWindow:
    base = {
        "ticks": 18,
        "fence_cpus": 2,
        "fence_cpus_max": 2,
        "fence_of": 6,
        "lane_k": 2,
        "priv_pct": 64.0,
        "phantom": 3.1,
        "busy": 5.2,
        "outside": 3.0,
        "host_cores": 4.0,
        "steal_host": 0.2,
    }
    return MachineWindow(**{**base, **over})


def _witnesses(**over) -> LaunchWitnesses:
    base = {
        "budget_s": 90.0,
        "posted_at": T0,
        "deadline_at": T0 + 94.0,
        "alive": True,
        "cpu_s": 3.1,
        "cpu_wall_s": 92.0,
        "process_started_at": T0 + 0.2,
        "log_lines": [_log_line(T0 + 79.0, "fauna_ffi: fauna client logging initialised")],
        "machine": _machine(),
        "cpu_wait_share": 0.94,
        "cpu_wait_basis": "a thread Ready at 85 of 90 status reads",
    }
    return LaunchWitnesses(**{**base, **over})


def _first(witnesses: LaunchWitnesses) -> str:
    return launch_timeout_report(witnesses).splitlines()[0]


# --- one test per verdict ----------------------------------------------------


def test_starved_is_the_riders_grammar():
    """Alive, unfaulted, under a quarter of a core and waiting for one — the
    2026-10-29 shape."""
    assert _first(_witnesses()) == (
        "launch timeout (90 s): STARVED — the app ran 3.1 core-s in 92 s and waited "
        "for a CPU 94 % of them; fence 2 of 2, priv 64 %, phantom 3.1, steal_host 0.2"
    )


def test_faulted_on_an_exit_carries_the_exit_phrase():
    assert _first(
        _witnesses(alive=False, exit_phrase="exit code 73 (0x00000049)", cpu_s=0.4)
    ) == "launch timeout (90 s): FAULTED — exit code 73 (0x00000049)"


@pytest.mark.parametrize(
    "line",
    [
        "[err] [fauna] FATAL unhandled exception on the UI thread",
        "[err] thread 'main' panicked at libs/fauna-ffi/src/lib.rs:7:13:",
        "[err] the process ended with 0xC0000409",
    ],
)
def test_faulted_on_a_fatal_in_the_inherited_stderr(line):
    """A fatal is a fault while the process still stands: the exit is not needed."""
    first = _first(_witnesses(stderr_lines=[line]))
    assert first.startswith("launch timeout (90 s): FAULTED — "), first
    assert line.removeprefix("[err] ")[:30] in first


def test_faulted_on_a_fatal_in_the_apps_own_log():
    line = _log_line(T0 + 5.0, "panic: thread 'x' panicked at libs/fauna-log/src/lib.rs:1:1")
    first = _first(_witnesses(log_lines=[line]))
    assert first.startswith("launch timeout (90 s): FAULTED — "), first


def test_bridge_narration_is_not_the_apps_fatal():
    """The bridge reports on the same stream; its words are not the app's record."""
    first = _first(_witnesses(stderr_lines=["[err] [bridge] exited with 0xC0000409"]))
    assert "FAULTED" not in first, first


def test_stalled_names_the_last_line_and_when():
    """Given its CPU and still not up: the launch's own, with its tail beside it."""
    assert _first(_witnesses(cpu_s=40.0, cpu_wait_share=0.05)) == (
        "launch timeout (90 s): STALLED, NOT STARVED — ran 40.0 core-s, waited for a "
        'CPU 5 % of 92 s; last line "fauna_ffi: fauna client logging initialised" at +79 s'
    )


def test_stalled_with_no_log_says_so():
    first = _first(_witnesses(cpu_s=40.0, cpu_wait_share=0.05, log_lines=[]))
    assert first == (
        "launch timeout (90 s): STALLED, NOT STARVED — ran 40.0 core-s, waited for a "
        "CPU 5 % of 92 s; no log line"
    )


def test_a_blocked_launch_is_stalled_not_starved():
    """Under a quarter of a core because it asked for none: a launch that
    deadlocks, or waits on a lock or a socket, is the app's (the rider's rule since
    2026-10-30 — before it, this read STARVED)."""
    first = _first(_witnesses(cpu_s=0.3, cpu_wait_share=0.0))
    assert first == (
        "launch timeout (90 s): STALLED, NOT STARVED — ran 0.3 core-s, waited for a "
        'CPU 0 % of 92 s; last line "fauna_ffi: fauna client logging initialised" at +79 s'
    )


def test_given_its_cpu_the_wait_is_not_needed():
    """Over the quarter-core line the wait witness decides nothing, so its absence
    is not an UNKNOWN; the line keeps its old shape."""
    first = _first(_witnesses(cpu_s=40.0, cpu_wait_share=None, cpu_wait_unread="no reading"))
    assert first == (
        "launch timeout (90 s): STALLED, NOT STARVED — ran 40.0 core-s; "
        'last line "fauna_ffi: fauna client logging initialised" at +79 s'
    )


def test_unknown_when_the_wait_is_unread_under_the_line():
    """Under the line the wait is the decider: unread, the verdict says so rather
    than defaulting to either suspect (the first rider)."""
    first = _first(
        _witnesses(cpu_wait_share=None, cpu_wait_unread="the bridge reports no threads_ready")
    )
    assert first == (
        "launch timeout (90 s): UNKNOWN — the process's CPU wait: "
        "the bridge reports no threads_ready"
    )


def test_unknown_on_a_silent_bridge():
    """Convention 6's first rider: an inconclusive witness says so."""
    first = _first(
        _witnesses(alive=None, cpu_s=None, process_unread="the bridge did not answer")
    )
    assert first == (
        "launch timeout (90 s): UNKNOWN — the process: the bridge did not answer"
    )


def test_unknown_on_no_cpu_reading():
    first = _first(_witnesses(cpu_s=None, cpu_unread="this bridge reports no cpu_ms"))
    assert first == (
        "launch timeout (90 s): UNKNOWN — the process's CPU time: "
        "this bridge reports no cpu_ms"
    )


def test_unknown_on_an_unreadable_ledger():
    machine = MachineWindow(unread="no agent ledger at X")
    first = _first(_witnesses(machine=machine))
    assert first == "launch timeout (90 s): UNKNOWN — the agent ledger: no agent ledger at X"


def test_unknown_on_no_machine_window_at_all():
    first = _first(_witnesses(machine=None))
    assert first.startswith("launch timeout (90 s): UNKNOWN — the agent ledger: "), first


def test_a_silent_bridge_with_a_fatal_in_the_record_is_faulted():
    """A positive witness outranks a missing one."""
    first = _first(
        _witnesses(
            alive=None,
            cpu_s=None,
            process_unread="the bridge did not answer",
            stderr_lines=["[err] [fauna] FATAL boom"],
        )
    )
    assert first.startswith("launch timeout (90 s): FAULTED — "), first


# --- the decider -------------------------------------------------------------


def test_steal_host_high_with_a_healthy_share_is_not_starved():
    """`steal_host` is context and never the decider: on 2026-10-29 it read the
    guest's storm, not the host."""
    first = _first(_witnesses(cpu_s=60.0, machine=_machine(steal_host=9.0)))
    assert "STALLED, NOT STARVED" in first, first


def test_a_low_share_with_no_host_reading_is_still_starved():
    first = _first(
        _witnesses(machine=_machine(host_cores=None, steal_host=None, host_unread="refused"))
    )
    assert "STARVED" in first and first.endswith("steal_host ?"), first


@pytest.mark.parametrize(
    ("cpu_s", "verdict"),
    [(22.9, "STARVED"), (23.0, "STALLED, NOT STARVED")],
)
def test_a_quarter_of_a_core_is_the_line(cpu_s, verdict):
    assert f": {verdict} — " in _first(_witnesses(cpu_s=cpu_s, cpu_wall_s=92.0))


@pytest.mark.parametrize(
    ("cpu_wait_share", "verdict"),
    [(0.249, "STALLED, NOT STARVED"), (0.25, "STARVED")],
)
def test_a_quarter_of_the_window_waiting_is_the_other_line(cpu_wait_share, verdict):
    """Under the quarter-core line, STARVED needs the app to have WAITED for a CPU
    a quarter of the window: own CPU share says what it got, the wait says what it
    asked for and was not given."""
    assert f": {verdict} — " in _first(_witnesses(cpu_wait_share=cpu_wait_share))


# --- the line, the body, the guarantee ---------------------------------------


@pytest.mark.parametrize(
    "over",
    [
        {"alive": False, "exit_phrase": "exit code 1 — " + "x" * 400},
        {"stderr_lines": ["[err] [fauna] FATAL " + "y" * 400]},
        {"cpu_s": 40.0, "log_lines": [_log_line(T0 + 1.0, "z" * 400)]},
        {"alive": None, "cpu_s": None, "process_unread": "w" * 400},
        {"machine": MachineWindow(unread="v" * 400)},
        {"cpu_wait_share": None, "cpu_wait_unread": "u" * 400},
        {"cpu_s": 999999.9, "cpu_wall_s": 999999.0, "cpu_wait_share": 123.456},
    ],
    ids=["exit", "fatal", "stalled", "bridge", "ledger", "wait", "wide"],
)
def test_the_first_line_survives_the_live_reason_cut(over):
    report = launch_timeout_report(_witnesses(**over))
    first = report.splitlines()[0]
    assert len(f"TimeoutError: {first}") <= LIVE_REASON_CHARS, first


def test_the_witnesses_follow_the_verdict_as_offsets():
    report = launch_timeout_report(_witnesses())
    assert "first log line +79.0 s" in report
    assert "process started +0.2 s" in report
    assert "deadline +94.0 s" in report
    assert "0.03 of a core" in report
    assert "waited for a CPU 94 % of the window (a thread Ready at 85 of 90 status reads)" in report
    assert "fence 2 CPUs of the machine's 6" in report


def test_each_absence_is_stated():
    report = launch_timeout_report(
        _witnesses(
            log_lines=[],
            stderr_lines=[],
            machine=_machine(fence_cpus=None, fence_cpus_max=None, host_unread="refused"),
        )
    )
    assert "no line of its own" in report
    assert "no fence up" in report
    assert "the host: refused" in report
    unread = launch_timeout_report(
        _witnesses(cpu_s=40.0, cpu_wait_share=None, cpu_wait_unread="no schedstat")
    )
    assert "CPU wait unread: no schedstat" in unread


def test_the_drivers_notes_are_carried():
    report = launch_timeout_report(_witnesses(notes=["3 pushes were EPOCH-FENCED"]))
    assert "3 pushes were EPOCH-FENCED" in report


@pytest.mark.parametrize(
    "witnesses",
    [object(), None, LaunchWitnesses(budget_s=90.0, posted_at="x", deadline_at=None)],
    ids=["foreign", "none", "garbage"],
)
def test_it_never_raises(witnesses):
    """It runs inside a `raise`: an exception here would replace the real failure."""
    first = launch_timeout_report(witnesses).splitlines()[0]
    assert first.startswith("launch timeout ("), first
    assert "UNKNOWN" in first, first


# --- the ledger reader -------------------------------------------------------


def _row(at: float, **over) -> dict:
    stamp = datetime.datetime.fromtimestamp(at).astimezone()
    base = {
        "ts": stamp.isoformat(timespec="milliseconds"),
        "cpu_pct": 50.0,
        "user_pct": 20.0,
        "priv_pct": 30.0,
        "fence": {"cpus": 2, "of": 6, "by": "broker"},
        "owners": {
            "unattributed": {"user": 0.0, "kern": 1.0},
            "system": {"user": 0.5, "kern": 2.0},
            "desktop": {"user": 0.1, "kern": 0.1},
            "win-2": {"user": 1.0, "kern": 0.2},
        },
    }
    return {**base, **over}


def _write(path: Path, rows: list[dict], torn: bool = False) -> None:
    with open(path, "w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row) + "\n")
        if torn:
            fh.write('{"ts": "2026-10')


def _broker():
    """The fleet core broker, whose functions the reader runs over a row. It does
    not ship, so a public checkout skips the arithmetic and keeps the verdicts."""
    try:
        return launch_load._broker()
    except FileNotFoundError as exc:
        skip_environment(f"the fleet's tooling is not in this checkout: {exc}")


@pytest.fixture
def slot_dir(tmp_path, monkeypatch):
    _broker()
    monkeypatch.setenv("FAUNA_SLOT_DIR", str(tmp_path))
    # The host's reading has its own test: no real broker is ever asked here.
    monkeypatch.setattr(
        launch_load, "_host_reading", lambda *a, **k: (None, None, "not asked")
    )
    return tmp_path


def _ledger(slot: Path) -> Path:
    return slot / _broker()._PROBE_LEDGER


def test_the_window_reads_the_rotated_file_too_and_only_the_window(slot_dir):
    ledger = _ledger(slot_dir)
    _write(Path(f"{ledger}.1"), [_row(T0 - 500), _row(T0 + 10, priv_pct=40.0)])
    _write(
        ledger,
        [_row(T0 + 20, priv_pct=20.0, fence={"cpus": 4, "of": 6, "by": "broker"}), _row(T0 + 500)],
        torn=True,
    )

    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)

    assert window.unread == ""
    assert window.ticks == 2
    assert window.priv_pct == pytest.approx(30.0)
    assert (window.fence_cpus, window.fence_cpus_max, window.fence_of) == (2, 4, 6)
    assert window.lane_k == 2


def test_the_windows_arithmetic_is_the_brokers(slot_dir):
    """`phantom` and `outside` are the agent's own functions over the row, never a
    second copy of their arithmetic."""
    _write(_ledger(slot_dir), [_row(T0 + 10)])

    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)

    broker = _broker()
    row = _row(T0 + 10)
    assert window.phantom == pytest.approx(broker.phantom({"usage": row}))
    assert window.phantom == pytest.approx(3.0)
    assert window.busy == pytest.approx(5.0)
    assert window.outside == pytest.approx(
        broker.owners_outside(broker.ledger_usage(row), 5.0)
    )


def test_a_missing_ledger_is_unread_and_names_it(slot_dir):
    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)
    assert "no agent ledger at" in window.unread
    assert str(slot_dir) in window.unread


def test_a_ledger_with_no_tick_in_the_window_is_unread(slot_dir):
    _write(_ledger(slot_dir), [_row(T0 - 500)])
    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)
    assert "no tick" in window.unread


def test_a_checkout_without_the_fleet_tooling_says_so(monkeypatch, tmp_path):
    monkeypatch.setattr(launch_load, "_REPO_ROOT", tmp_path)
    monkeypatch.setattr(launch_load, "_broker_module", None)
    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)
    assert "this checkout has no" in window.unread
    assert str(tmp_path) in window.unread


def test_the_hosts_reading_is_read_for_the_same_ticks(tmp_path, monkeypatch):
    monkeypatch.setenv("FAUNA_SLOT_DIR", str(tmp_path))
    _write(_ledger(tmp_path), [_row(T0 + 10)])
    broker = _broker()
    asked = {}

    def fetch_ticks(addr, token, since, until=None, machines=None, timeout=30.0):
        asked.update(since=since, until=until, machines=machines, timeout=timeout)
        return [
            {"machines": {machines[0]: {"host_cores": 3.0, "steal_host": 1.0}}},
            {"machines": {machines[0]: {"host_cores": 5.0, "steal_host": None}}},
        ]

    monkeypatch.setattr(broker, "fetch_ticks", fetch_ticks)
    monkeypatch.setattr(broker, "read_token", lambda *a: "token")

    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)

    assert (window.host_cores, window.steal_host, window.host_unread) == (4.0, 1.0, "")
    assert (asked["since"], asked["until"]) == (T0, T0 + 90)
    assert asked["timeout"] == launch_load.HOST_READ_TIMEOUT_S


def test_a_silent_broker_costs_only_the_hosts_reading(tmp_path, monkeypatch):
    monkeypatch.setenv("FAUNA_SLOT_DIR", str(tmp_path))
    _write(_ledger(tmp_path), [_row(T0 + 10)])
    broker = _broker()

    def refuse(*a, **k):
        raise OSError("no route to host")

    monkeypatch.setattr(broker, "fetch_ticks", refuse)
    monkeypatch.setattr(broker, "read_token", lambda *a: "token")

    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)

    assert window.unread == ""
    assert window.ticks == 1
    assert (window.host_cores, window.steal_host) == (None, None)
    assert "no route to host" in window.host_unread


def test_no_token_is_a_stated_absence_of_the_hosts_reading(tmp_path, monkeypatch):
    monkeypatch.setenv("FAUNA_SLOT_DIR", str(tmp_path))
    _write(_ledger(tmp_path), [_row(T0 + 10)])
    monkeypatch.setattr(_broker(), "read_token", lambda *a: None)

    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)

    assert "no broker token" in window.host_unread


def test_the_reader_never_raises(monkeypatch):
    def boom():
        raise RuntimeError("the load itself broke")

    monkeypatch.setattr(launch_load, "_broker", boom)
    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",), vcpus=10)
    assert "the load itself broke" in window.unread


def test_vcpus_default_to_the_machines(slot_dir):
    _write(_ledger(slot_dir), [_row(T0 + 10)])
    window = launch_load.machine_window(T0, T0 + 90, apps=("windows",))
    assert window.busy == pytest.approx(0.5 * (os.cpu_count() or 1))

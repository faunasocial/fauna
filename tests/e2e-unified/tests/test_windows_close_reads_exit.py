r"""tier_1: a windows app that died of a crash before the harness closed it
fails the phase that found it.

Convention 6's close rider (`docs/goal/architecture/e2e-self-diagnosing-failures.md`
§ The convention). Measured 2026-10-10 on Windows: `VideoThumbnailView.TearDownPlayer`
disposed a `MediaPlayer` its `MediaPlayerElement` still held, every unload of a
playing slot killed the app with `0xC000027B` (STATUS_STOWED_EXCEPTION), and
`test_feed_video_playback.py --app windows` reported 5 passed while the app died
three times — each death after a test's last assertion, the harness's next close
or relaunch of the app none the wiser.

The bridge's close is a hard kill (`SessionManager.Quit`: `Process.Kill`, no
WM_CLOSE), so the close itself never raises a stowed exception — the app must
already be dead when it runs. And after the close the handle is gone
(`AppStatus` answers `exit_code: null`), so the exit is read BEFORE the
`DELETE /session`, never after. These tests drive each windows close path with
only its outward calls replaced: the read comes first, a crash exit is
recorded with the app's own crash reports, every other ending stays silent,
and the close and its cleanup still run.
"""

from __future__ import annotations

import datetime
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import windows as win  # noqa: E402
from helpers import app_deaths  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.windows]

#: The unsigned NTSTATUS values as the bridge reports them: .NET's
#: `Process.ExitCode` is a signed int32.
STOWED_EXCEPTION = 0xC000027B - 2**32  # -1073741189
STACK_BUFFER_OVERRUN = 0xC0000409 - 2**32
TERMINATE_PROCESS_KILL = -1  # what `Process.Kill` leaves
CONTROL_C_EXIT = 0xC000013A - 2**32

FIRST_CHANCE = [
    "[err] [fauna] FIRST-CHANCE System.Runtime.InteropServices.COMException (0x8001010E)",
    "   at Microsoft.UI.Xaml.Controls.MediaPlayerElement.SetMediaPlayer(MediaPlayer)",
]


def _instant(seconds_ago: float) -> str:
    stamp = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=seconds_ago)
    return stamp.strftime("%Y-%m-%dT%H:%M:%S.%f0Z")


def _crashed(code: int = STOWED_EXCEPTION, **extra) -> dict:
    return {
        "running": False,
        "exit_code": code,
        "launched_at": _instant(40),
        "exited_at": _instant(3),
        **extra,
    }


@pytest.fixture(autouse=True)
def _clean_registry(monkeypatch):
    """Each test reads what it recorded, and leaves nothing for the phase
    wrapper to raise on its behalf — nor a launch marked reported."""
    monkeypatch.setattr(app_deaths, "_reported", set())
    app_deaths.drain()
    yield
    app_deaths.drain()


def _driver(monkeypatch, tmp_path, *, status, status_raises=False, bridge_log=FIRST_CHANCE):
    """A launched `WindowsBridgeDriver` with only its outward calls replaced.
    `calls` records the bridge traffic in order; the status switches to the
    no-handle answer once the session is deleted, as the bridge's does."""
    d = win.WindowsBridgeDriver()
    d._session_body = {"app": "FaunaApp.exe", "args": "", "environment": {}}
    d._data_dir = str(tmp_path / "data")
    Path(d._data_dir).mkdir()
    d._owns_data_dir = True
    d._bridge_log = list(bridge_log)
    calls: list[tuple[str, str]] = []
    current = {"status": dict(status)}

    def _get(path, *a, **k):
        calls.append(("GET", path))
        if path != "/session/status":
            raise ConnectionError("no such route here")
        if status_raises:
            raise ConnectionError("bridge is gone")
        return dict(current["status"])

    def _delete(path, *a, **k):
        calls.append(("DELETE", path))
        current["status"] = {"running": False, "exit_code": None}
        return {"closed": True}

    monkeypatch.setattr(d, "_get", _get)
    monkeypatch.setattr(d, "_delete", _delete)
    monkeypatch.setattr(d, "app_log_text", lambda *a, **k: "")
    return d, calls


def _first_delete(calls) -> int:
    return next(i for i, (verb, _path) in enumerate(calls) if verb == "DELETE")


def test_teardown_reads_the_exit_before_its_close_and_records_the_crash(monkeypatch, tmp_path):
    d, calls = _driver(monkeypatch, tmp_path, status=_crashed())
    data_dir = Path(d._data_dir)

    d._stop_app()

    assert ("GET", "/session/status") in calls[: _first_delete(calls)], calls
    found = app_deaths.drain()
    assert len(found) == 1, found
    first = found[0].splitlines()[0]
    assert first.startswith(
        "the app died before teardown closed it: exit code -1073741189 (0xC000027B) — "
        "STATUS_STOWED_EXCEPTION"
    ), first
    assert "[fauna] FIRST-CHANCE System.Runtime.InteropServices.COMException" in found[0], found[0]
    assert "MediaPlayerElement.SetMediaPlayer" in found[0], found[0]
    assert "it exited +37 s after its launch, 3 s before this close" in found[0], found[0]
    # The close and its cleanup ran all the same.
    assert ("DELETE", "/session") in calls, calls
    assert not data_dir.exists()
    assert d._data_dir is None


@pytest.mark.parametrize(
    ("close", "label", "call"),
    [
        ("quit_app", "quit_app()", lambda d: d.quit_app()),
        ("kill_uncleanly", "kill_uncleanly()", lambda d: d.kill_uncleanly()),
    ],
)
def test_the_other_closes_read_first_too(monkeypatch, tmp_path, close, label, call):
    d, calls = _driver(monkeypatch, tmp_path, status=_crashed(STACK_BUFFER_OVERRUN))

    call(d)

    assert ("GET", "/session/status") in calls[: _first_delete(calls)], calls
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert found[0].startswith(
        f"the app died before {label} closed it: exit code -1073740791 (0xC0000409)"
    ), found[0]


def test_a_relaunch_relaunches_and_still_records_the_death(monkeypatch, tmp_path):
    """`recover()` is how the `app` fixture gets past a reset the dead app never
    acknowledged — the path that relaunched three dead apps unseen. The
    relaunch still happens, so the tests after it run on a live app."""
    d, calls = _driver(monkeypatch, tmp_path, status=_crashed())
    posted: list[str] = []
    monkeypatch.setattr(d, "_await_health", lambda *a, **k: True)
    monkeypatch.setattr(d, "_post", lambda path, *a, **k: posted.append(path) or {})
    monkeypatch.setattr(d, "_get_state_raw", lambda *a, **k: {"page": "login"})
    monkeypatch.setattr(d, "_relaunch_principal_slot_carry", lambda *a, **k: None)
    monkeypatch.setattr(d, "_begin_install_device_secret_row_carry", lambda *a, **k: None)
    monkeypatch.setattr(d, "_erase_credential_store_for_relaunch", lambda *a, **k: None)
    monkeypatch.setattr(d, "dismiss_system_dialogs", lambda *a, **k: None)
    monkeypatch.setattr(d, "is_visible", lambda *a, **k: False)

    assert d.recover() is True

    assert ("GET", "/session/status") in calls[: _first_delete(calls)], calls
    assert posted == ["/session"], posted
    found = app_deaths.drain()
    assert len(found) == 1, found
    assert found[0].startswith(
        "the app died before the relaunch in recover() closed it: exit code -1073741189 (0xC000027B)"
    ), found[0]


@pytest.mark.parametrize(
    "status",
    [
        pytest.param({"running": True, "exit_code": None}, id="alive"),
        pytest.param({"running": False, "exit_code": None}, id="no-tracked-handle"),
        pytest.param(_crashed(TERMINATE_PROCESS_KILL), id="killed-by-terminate-process"),
        pytest.param(_crashed(CONTROL_C_EXIT), id="control-c"),
        pytest.param(_crashed(0), id="quit-cleanly"),
        pytest.param(_crashed(73), id="ordinary-nonzero-exit"),
    ],
)
def test_every_ending_but_a_crash_stays_silent(monkeypatch, tmp_path, status):
    """A process the driver's own kill ended has no handle left to answer
    (`no-tracked-handle`); `Process.Kill` from anywhere else leaves -1; a clean
    quit, a signal and an ordinary exit code are not crashes."""
    d, calls = _driver(monkeypatch, tmp_path, status=status)

    d._stop_app()

    assert app_deaths.drain() == []
    assert ("DELETE", "/session") in calls, calls


def test_a_bridge_that_cannot_answer_leaves_the_close_to_run(monkeypatch, tmp_path):
    d, calls = _driver(monkeypatch, tmp_path, status=_crashed(), status_raises=True)

    d._stop_app()

    assert app_deaths.drain() == []
    assert ("DELETE", "/session") in calls, calls


def test_a_driver_that_never_launched_asks_nothing(monkeypatch, tmp_path):
    d, calls = _driver(monkeypatch, tmp_path, status=_crashed())
    d._session_body = None

    d._stop_app()

    assert ("GET", "/session/status") not in calls, calls
    assert app_deaths.drain() == []


def test_a_bridge_with_no_exit_instant_says_so(monkeypatch, tmp_path):
    status = {k: v for k, v in _crashed().items() if k != "exited_at"}
    d, _calls = _driver(monkeypatch, tmp_path, status=status)

    d._stop_app()

    found = app_deaths.drain()
    assert len(found) == 1, found
    assert "the bridge reports no exited_at" in found[0], found[0]


# ── The phase wrapper: the recorded death becomes the phase's failure ─────────


def test_a_death_found_in_a_passing_phase_fails_it():
    gen = app_deaths._phase("teardown")
    next(gen)
    app_deaths.record("the app died before teardown closed it: exit code -1073741189 (0xC000027B)")

    with pytest.raises(app_deaths.AppDiedBeforeClose) as raised:
        gen.send("hook result")

    text = str(raised.value)
    assert text.splitlines()[0].startswith(
        "the app died before teardown closed it: exit code -1073741189 (0xC000027B)"
    ), text
    assert app_deaths.drain() == []


def test_a_death_found_in_a_setup_names_the_test_before_it(monkeypatch):
    """The reset that opens a test is what navigates away from the state the
    test before it left; that test's report is already written, so the setup's
    failure names it — the verdict still the first line."""
    monkeypatch.setattr(app_deaths, "_last_finished", "tests/test_x.py::test_plays[windows]")
    gen = app_deaths._phase("setup")
    next(gen)
    app_deaths.record("the app died before the relaunch in recover() closed it: exit code 1\nmore")

    with pytest.raises(app_deaths.AppDiedBeforeClose) as raised:
        gen.send(None)

    lines = str(raised.value).splitlines()
    assert lines[0] == "the app died before the relaunch in recover() closed it: exit code 1", lines
    assert "the last test to finish before it was tests/test_x.py::test_plays[windows]" in lines[1], lines
    assert lines[2] == "more", lines


def test_a_phase_with_no_death_returns_its_result():
    gen = app_deaths._phase("call")
    next(gen)

    with pytest.raises(StopIteration) as stopped:
        gen.send("hook result")

    assert stopped.value.value == "hook result"


def test_a_death_found_in_a_failing_phase_carries_the_failure_as_its_context():
    """A setup that skipped "the bridge could not recover", or a test whose next
    step met the dead app, reads as the death — with the phase's own failure
    chained beneath it, never lost."""
    gen = app_deaths._phase("setup")
    next(gen)
    app_deaths.record("the app died before the relaunch in recover() closed it: exit code 1")
    original = TimeoutError("App did not acknowledge reset within 30.0s")

    with pytest.raises(app_deaths.AppDiedBeforeClose) as raised:
        gen.throw(original)

    assert raised.value.__context__ is original


def test_a_session_exit_is_never_replaced():
    gen = app_deaths._phase("call")
    next(gen)
    app_deaths.record("the app died before teardown closed it: exit code 1")

    with pytest.raises(pytest.exit.Exception):
        gen.throw(pytest.exit.Exception("stop the run"))
    # Still recorded (and printed when it was), never raised over the exit.
    assert len(app_deaths.drain()) == 1


def test_a_death_an_earlier_failure_reported_is_not_failed_again(monkeypatch, tmp_path):
    """Measured on the red run: an ack timeout that already said DEAD erred one
    test, then the relaunch that opened the next found the same dead launch and
    erred that one too. The ack verdict names the launch; a failed phase that
    carries it marks the death reported, and the later close only prints."""
    status = _crashed()
    d, _calls = _driver(monkeypatch, tmp_path, status=status)
    verdict = d.ack_timeout_diagnostics()
    assert app_deaths.tag(status["launched_at"]) in verdict, verdict

    gen = app_deaths._phase("setup")
    next(gen)
    with pytest.raises(TimeoutError):
        gen.throw(TimeoutError(f"App did not acknowledge command set_state within 60.0s{verdict}"))

    d._stop_app()

    assert app_deaths.drain() == []


def test_a_death_seen_only_in_a_swallowed_error_still_fails(monkeypatch, tmp_path):
    """The reset the `app` fixture recovers from carries the same DEAD verdict,
    and the fixture swallows it: no phase failed, so nothing is marked, and the
    relaunch's close records the death — the silent path this rider closes."""
    d, _calls = _driver(monkeypatch, tmp_path, status=_crashed())
    d.ack_timeout_diagnostics()  # built for a TimeoutError the fixture swallows

    d._stop_app()

    assert len(app_deaths.drain()) == 1


def test_conftest_registers_the_phase_wrappers(request):
    """Without the registration every recording above is read by nothing."""
    assert request.config.pluginmanager.get_plugin(app_deaths.PLUGIN_NAME) is app_deaths

"""The tui driver's `app_stderr_text()` reads the app's own tracing log.

`docs/goal/architecture/e2e-conventions.md` § point 6 — failures diagnose
themselves. tui installs a daily-rolling tracing file at
`<XDG_CONFIG_HOME>/fauna-tui/logs/` on every platform (`session::install_logging`
→ `fauna_log::init_with_stderr`), and a stderr copy only when stderr is not a
terminal. On POSIX the driver redirects stderr to `app.err`, so that copy is
there. On windows stderr IS the ConPTY terminal: the stderr layer is off and
`app.err` stays empty. The old fallback was the pty stream (`app.out`), which is
frames, not log lines — measured 2026-09-28 on two tui-on-win succession reds,
whose "app log" was one escape-coded line of screen paint and nothing the app
had logged. The on-disk file is the app's own words, so it comes before the
frame stream.

tier_1: a hand-built driver over temp files, no app, no nest.
"""

import os
import sys
from pathlib import Path

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers.base import AGENT_LOG_BANNER  # noqa: E402
from drivers.tui import TuiDriver  # noqa: E402


def _driver(tmp_path, *, err="", out="", tui_log=None, agent_log=None):
    launch = tmp_path / "launch"
    launch.mkdir()
    (launch / "app.err").write_text(err)
    (launch / "app.out").write_text(out)
    xdg = tmp_path / "xdg"
    if tui_log is not None:
        logs = xdg / "fauna-tui" / "logs"
        logs.mkdir(parents=True)
        (logs / "fauna.log.2026-09-28").write_text(tui_log)
    driver = TuiDriver.__new__(TuiDriver)
    driver._tmp_dir = str(launch)
    driver._xdg_config = str(xdg)
    if agent_log is not None:
        agent = tmp_path / "agent"
        (agent / "logs").mkdir(parents=True)
        (agent / "logs" / "fauna.log.2026-09-28").write_text(agent_log)
        driver._sync_agent_data_dir = str(agent)
    else:
        driver._sync_agent_data_dir = None
    return driver


def test_the_on_disk_log_beats_the_frame_stream_when_stderr_is_empty(tmp_path):
    driver = _driver(
        tmp_path,
        out="\x1b[1;1HDisconnected\x1b[2;1H┌Signing you in",
        tui_log="INFO fauna_tui: fauna-tui client logging initialised\n",
    )
    text = driver.app_stderr_text()
    assert "client logging initialised" in text
    assert "Signing you in" not in text


def test_captured_stderr_still_wins_where_there_is_one(tmp_path):
    """POSIX: `app.err` holds the same lines the file does, plus anything written
    before the subscriber (a panic) — it stays the first answer."""
    driver = _driver(tmp_path, err="panicked at src/main.rs\n", tui_log="file line\n")
    assert driver.app_stderr_text().startswith("panicked at src/main.rs")


def test_the_frame_stream_is_the_last_resort(tmp_path):
    driver = _driver(tmp_path, out="frames only")
    assert driver.app_stderr_text() == "frames only"


@pytest.mark.skipif(os.name != "nt", reason="the agent is detached only on windows")
def test_the_detached_agent_log_rides_behind_the_banner(tmp_path):
    driver = _driver(tmp_path, tui_log="app line\n", agent_log="agent line\n")
    app_part, agent_part = driver.app_stderr_text().split(f"\n{AGENT_LOG_BANNER}\n")
    assert "app line" in app_part and "agent line" in agent_part

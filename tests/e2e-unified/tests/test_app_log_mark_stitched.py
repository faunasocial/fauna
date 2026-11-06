"""Unit tests for reading "the app's log since this point" where the app's log
and its detached sync agent's are stitched into one text (tier_1).

windows, and tui on windows, spawn the agent DETACHED, so `app_stderr_text()`
is the app's own log, `AGENT_LOG_BANNER`, then the agent's own file
(`drivers/base.py::stitch_agent_log`). A character offset into that text names
no place once the app writes: the app's new lines land in the middle, before
the agent section, and the slice at the old length starts inside the agent's
part. `test_admin_dns_added_domain_shows_its_records_without_a_tick[tui]` went
red on Windows on exactly that, with the matrix load it waited for in tui's own
log. The mark is taken per section, so each part reads on from its own place.
"""

import pytest

from drivers.base import AGENT_LOG_BANNER, PlatformDriver, stitch_agent_log

pytestmark = pytest.mark.tier_1


class _Stitched:
    """A driver whose app and detached agent each keep a log of their own,
    behind the base class's `app_log_mark` / `app_log_since`."""

    app_log_mark = PlatformDriver.app_log_mark
    app_log_since = PlatformDriver.app_log_since

    def __init__(self) -> None:
        self.app: list[str] = []
        self.agent: list[str] = []

    def app_stderr_text(self) -> str:
        return stitch_agent_log("\n".join(self.app), "\n".join(self.agent))


def test_an_app_line_written_after_the_mark_is_read_though_the_agent_logged_first():
    driver = _Stitched()
    driver.app = ["app 1"]
    driver.agent = [f"agent {i}" for i in range(50)]
    mark = driver.app_log_mark()

    driver.app.append("app 2: loaded the DNS record matrix: 3 domain(s)")
    since = driver.app_log_since(mark)

    assert "app 2: loaded the DNS record matrix: 3 domain(s)" in since
    assert "app 1" not in since and "agent 0" not in since


def test_both_sections_read_on_from_their_own_place():
    driver = _Stitched()
    driver.app = ["app 1"]
    driver.agent = ["agent 1"]
    mark = driver.app_log_mark()
    assert driver.app_log_since(mark).strip() == ""

    driver.app.append("app 2")
    driver.agent.append("agent 2")
    since = driver.app_log_since(mark)

    assert "app 2" in since and "agent 2" in since
    assert "app 1" not in since and "agent 1" not in since
    assert since.index("app 2") < since.index(AGENT_LOG_BANNER) < since.index("agent 2")


def test_an_agent_log_that_appears_after_the_mark_is_read_whole():
    driver = _Stitched()
    driver.app = ["app 1"]
    mark = driver.app_log_mark()

    driver.agent = ["agent 1"]
    driver.app.append("app 2")
    since = driver.app_log_since(mark)

    assert "app 2" in since and "agent 1" in since and "app 1" not in since


def test_an_unstitched_log_reads_on_from_the_character_offset():
    driver = _Stitched()
    driver.app = ["app 1"]
    mark = driver.app_log_mark()
    driver.app.append("app 2")
    assert driver.app_log_since(mark) == "\napp 2"

"""Unit tests for reading "the app's log since this point" on web (tier_1).

Web's log is the bridge's 500-entry console ring, so once a session has logged
500 lines the text stops growing: a character offset taken from it no longer
names a place, the next read slices into whatever the ring holds by then, and a
line logged after the mark can fall before it. A test of the shared session's
late order then reads "nothing happened" where something did —
`test_admin_dns_added_domain_shows_its_records_without_a_tick[web]` went red on
exactly that, with the load it was waiting for sitting in the ring. The driver's
mark counts lines ever logged, so it holds however far the ring has moved.
"""

import pytest

pytestmark = pytest.mark.tier_1

from drivers.web import WebBridgeDriver

RING = 500


class _Ring(WebBridgeDriver):
    """The driver over a fake `/page/console`, no bridge or browser behind it."""

    def __init__(self) -> None:
        self.seen = 0

    def log(self, count: int) -> None:
        self.seen += count

    def _get(self, path: str, *args, **kwargs) -> dict:
        assert path == "/page/console"
        first = max(0, self.seen - RING)
        return {"lines": [f"line {i}" for i in range(first, self.seen)], "dropped": first}


def test_a_character_offset_into_a_wrapped_ring_loses_the_new_line():
    """The reason the mark exists: same-length ring, so the offset finds nothing."""
    driver = _Ring()
    driver.log(RING + 10)
    offset = len("\n".join(driver.console_log()))
    driver.log(1)
    assert driver.app_stderr_text()[offset:].count("\n") <= 1
    assert f"line {RING + 10}" in driver.app_log_since(driver.seen - 1)


def test_the_mark_finds_only_what_was_logged_after_it_on_a_wrapped_ring():
    driver = _Ring()
    driver.log(RING + 10)
    mark = driver.app_log_mark()
    driver.log(3)
    assert driver.app_log_since(mark).splitlines() == [
        f"line {RING + 10}", f"line {RING + 11}", f"line {RING + 12}",
    ]


def test_the_mark_holds_before_the_ring_has_wrapped():
    driver = _Ring()
    driver.log(10)
    mark = driver.app_log_mark()
    assert driver.app_log_since(mark) == ""
    driver.log(1)
    assert driver.app_log_since(mark) == "line 10"


def test_a_mark_the_ring_has_passed_returns_what_is_left_and_says_so():
    """Absence stays unsound on an evicting log: the reader must see the gap."""
    driver = _Ring()
    driver.log(10)
    mark = driver.app_log_mark()
    driver.log(RING + 5)
    since = driver.app_log_since(mark)
    assert "EVICTED" in since.splitlines()[0]
    assert since.splitlines()[-1] == f"line {RING + 14}"

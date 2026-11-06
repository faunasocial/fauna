r"""tier_1: a web page whose renderer crashed before the harness closed it
fails the phase that found it.

Convention 6's close rider (`docs/goal/architecture/e2e-self-diagnosing-failures.md`
§ The convention), web leg. The web app's "process" is the page the bridge's
browser holds; its crash is Playwright's `crash` event, which the bridge keeps
and `/page/console` answers as `crashed`. The driver reads it BEFORE each close
(`_stop_app`, `kill_uncleanly`, the relaunch in `_fresh_boot`) because the close
drops the page. Only the driver's outward calls are replaced here.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from drivers import web  # noqa: E402
from helpers import app_deaths  # noqa: E402

pytestmark = [pytest.mark.tier_1, pytest.mark.web]

LINES = ["[console.log] booted", "[bridge] page crashed (renderer gone)"]


@pytest.fixture(autouse=True)
def _clean_registry(monkeypatch):
    monkeypatch.setattr(app_deaths, "_reported", set())
    app_deaths.drain()
    yield
    app_deaths.drain()


def _driver(monkeypatch, *, crashed, raises=False, killed=False):
    d = web.WebBridgeDriver()
    d._app_killed = killed
    d._spa_url = "http://x/"
    calls: list[tuple[str, str]] = []

    def _get(path, params=None):
        calls.append(("GET", path))
        if raises:
            raise ConnectionError("bridge is gone")
        return {"lines": list(LINES), "dropped": 0, "crashed": crashed}

    def _post(path, body=None, **k):
        calls.append(("POST", path))
        return {}

    def _delete(path, *a, **k):
        calls.append(("DELETE", path))
        return {}

    monkeypatch.setattr(d, "_get", _get)
    monkeypatch.setattr(d, "_post", _post)
    monkeypatch.setattr(d, "_delete", _delete)
    monkeypatch.setattr(d, "_execute_js", lambda *a, **k: None)
    monkeypatch.setattr(d, "_await_new_document", lambda *a, **k: None)
    return d, calls


def test_kill_after_a_renderer_crash_records_it_before_the_close(monkeypatch):
    d, calls = _driver(monkeypatch, crashed=True)
    d.kill_uncleanly()
    found = app_deaths.drain()
    assert len(found) == 1
    assert found[0].splitlines()[0] == (
        "the app died before kill_uncleanly() closed it: the page's renderer crashed"
    )
    assert "page crashed (renderer gone)" in found[0]
    assert calls.index(("GET", "/page/console")) < calls.index(("POST", "/page/kill"))


def test_stop_app_records_a_crash_and_still_closes(monkeypatch):
    d, calls = _driver(monkeypatch, crashed=True)
    d._stop_app()
    assert "renderer crashed" in app_deaths.drain()[0]
    assert ("DELETE", "/session") in calls


def test_the_relaunch_records_a_crash(monkeypatch):
    d, _calls = _driver(monkeypatch, crashed=True)
    d._fresh_boot()
    assert "before the relaunch closed it" in app_deaths.drain()[0]


@pytest.mark.parametrize("close", ["kill", "stop", "boot"])
def test_a_live_page_is_silent(monkeypatch, close):
    d, _calls = _driver(monkeypatch, crashed=False)
    {"kill": d.kill_uncleanly, "stop": d._stop_app, "boot": d._fresh_boot}[close]()
    assert app_deaths.drain() == []


def test_a_page_the_harness_killed_is_silent(monkeypatch):
    d, calls = _driver(monkeypatch, crashed=True, killed=True)
    d._stop_app()
    assert app_deaths.drain() == []
    assert ("GET", "/page/console") not in calls


def test_a_silent_bridge_is_silent_and_the_close_goes_on(monkeypatch):
    d, calls = _driver(monkeypatch, crashed=True, raises=True)
    d.kill_uncleanly()
    assert app_deaths.drain() == []
    assert ("POST", "/page/kill") in calls

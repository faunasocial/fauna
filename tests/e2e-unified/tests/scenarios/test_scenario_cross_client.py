"""Scenario: Cross-app sync.

Alice logs in on both web and Linux. Posts on web, verifies on Linux.
"""
import pytest

from .conftest import (
    create_scenario,
    make_cached_app,
    make_web_app,
    start_spa_server,
    _resolve_linux_binary,
)

# Cross-app scenario driving web + linux. The client markers scope it so
# --client deselection drops it under a non-participating client (e.g. windows,
# which has no linux app) instead of failing trying to build them.
#
# tui NOT added (2026-08-21): this scenario's fixtures
# (`alice_web`/`alice_linux`) are hard-wired to two SPECIFIC apps by design —
# the docstring names them explicitly, not "any two apps". Widening it to a
# third `alice_tui` seat (or swapping linux→tui) is a real scope decision
# about what this demonstration scenario proves, not a mechanical marker add
# like the rest of the disposals. Leave for a session that wants to
# decide whether cross-app-sync scenario coverage should be per-pair or
# widened to a fixed representative trio (web + linux + tui, tui as lead app).
pytestmark = [pytest.mark.tier_3, pytest.mark.web, pytest.mark.linux]


@pytest.fixture(scope="module")
def scenario(request, nest_mode, tmp_path_factory, static_dir):
    state, cleanup = create_scenario(
        request, nest_mode, tmp_path_factory, "scenario-sync"
    )
    spa_url, spa_server = start_spa_server(static_dir, state.nest["url"])
    state.shared["spa_url"] = spa_url
    state.shared["spa_server"] = spa_server
    try:
        yield state
    finally:
        spa_server.shutdown()
        cleanup()


@pytest.fixture(scope="module")
def alice_web(scenario):
    layer = make_web_app(
        scenario.shared["spa_url"],
        scenario.nest["url"],
        scenario.alice_secret_hex,
        scenario.alice["actor_id_hex"],
    )
    yield layer
    layer.driver.teardown()


@pytest.fixture(scope="module")
def alice_linux(scenario, _driver_cache):
    if _resolve_linux_binary() is None:
        pytest.skip("Linux binary not found")
    layer = make_cached_app(
        _driver_cache, "linux",
        nest_url=scenario.nest["url"],
        secret_hex=scenario.alice_secret_hex,
        actor_id_hex=scenario.alice["actor_id_hex"],
        handle="alice",
        nest=scenario.nest,
    )
    yield layer
    # Don't teardown — _driver_cache owns the driver lifecycle


def test_01_alice_logs_in_web(scenario, alice_web):
    """Alice logs in on web and sees the feed."""
    assert alice_web.feed.is_visible(), (
        f"alice_web should see the feed after login: error={alice_web.error_text()!r}"
    )


def test_02_alice_logs_in_linux(scenario, alice_linux):
    """Same Alice logs in on the Linux desktop app."""
    assert alice_linux.feed.is_visible(), (
        f"alice_linux should see the feed after login: error={alice_linux.error_text()!r}"
    )


def test_03_alice_posts_on_web(scenario, alice_web):
    """Alice creates a post via web."""
    text = "Cross-app sync test post"
    alice_web.feed.create_post(text)
    assert alice_web.feed.first_post_text() == text
    scenario.shared["post_text"] = text


def test_04_alice_sees_post_on_linux(scenario, alice_linux):
    """Same post appears on Alice's Linux app."""
    expected = scenario.shared["post_text"]
    # Navigate away from feed and back to trigger a refresh —
    # the Linux app fetched the (empty) feed at startup before the post existed.
    import time
    alice_linux.driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    time.sleep(1)
    alice_linux.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    time.sleep(2)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        count = alice_linux.feed.post_count()
        if count > 0:
            for i in range(count):
                if alice_linux.feed.post_text(i) == expected:
                    return
        time.sleep(1)
    pytest.fail(f"Post '{expected}' not visible on Linux after 15s")

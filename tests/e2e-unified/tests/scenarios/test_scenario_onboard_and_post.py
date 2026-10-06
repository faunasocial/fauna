"""Scenario: Onboard and post.

Alice logs in on web and creates a post.
Bob logs in on the Linux desktop app and sees Alice's post.
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
# (`alice_web`/`bob_linux`) are hard-wired to two SPECIFIC apps by design —
# the docstring names them explicitly, not "any two apps". Widening it to a
# third seat (or swapping linux→tui for Bob) is a real scope decision about
# what this demonstration scenario proves, not a mechanical marker add like
# the rest of the disposals. Same reasoning as test_scenario_cross_client.py.
pytestmark = [pytest.mark.tier_3, pytest.mark.web, pytest.mark.linux]


@pytest.fixture(scope="module")
def scenario(request, nest_mode, tmp_path_factory, static_dir):
    state, cleanup = create_scenario(
        request, nest_mode, tmp_path_factory, "scenario-post"
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
def bob_linux(scenario, _driver_cache):
    if _resolve_linux_binary() is None:
        pytest.skip("Linux binary not found")
    layer = make_cached_app(
        _driver_cache, "linux",
        nest_url=scenario.nest["url"],
        secret_hex=scenario.bob_secret_hex,
        actor_id_hex=scenario.bob["actor_id_hex"],
        handle="bob",
        nest=scenario.nest,
    )
    yield layer
    # Don't teardown — _driver_cache owns the driver lifecycle


def test_01_alice_sees_feed(scenario, alice_web):
    """Alice logs in on web and sees the feed."""
    assert alice_web.feed.is_visible(), (
        f"alice_web should see the feed after login: error={alice_web.error_text()!r}"
    )


def test_02_alice_creates_post(scenario, alice_web):
    """Alice creates a post on the feed."""
    text = "Hello from Alice in the scenario test!"
    alice_web.feed.create_post(text)
    assert alice_web.feed.first_post_text() == text
    scenario.shared["post_text"] = text


def test_03_bob_sees_feed_on_linux(scenario, bob_linux):
    """Bob logs in on the Linux desktop app and sees the feed."""
    assert bob_linux.feed.is_visible(), (
        f"bob_linux should see the feed after login: error={bob_linux.error_text()!r}"
    )


def test_04_bob_sees_alice_post(scenario, bob_linux):
    """Bob's feed contains Alice's post."""
    expected = scenario.shared["post_text"]
    import time
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        count = bob_linux.feed.post_count()
        if count > 0:
            for i in range(count):
                if bob_linux.feed.post_text(i) == expected:
                    return
        time.sleep(1)
    pytest.fail(f"Bob did not see Alice's post '{expected}' within 15s")

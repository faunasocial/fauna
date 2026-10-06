"""WASM chunk loading assertions for the web app.

The split lands the core chunk plus per-route sibling chunks (onboarding,
media, folders, …) so the first paint pays only for core wasm. These tests
assert the lazy-load contract by reading
`performance.getEntriesByType('resource')` via the bridge.

The web bridge runs Playwright in a subprocess; we use the bridge's JS-eval
hook (`_execute_js`) instead of a direct `Page` reference, which would not
exist outside the bridge process.

What we verify here:
- The onboarding chunk is fetched when a route opens that needs it.
- The media chunk is NOT fetched from any of the user's first-paint routes
  (/feed, /conversations) — the central regression test for the split: it is
  the largest lazily-loaded chunk after onboarding (~3.3 MB uncompressed) and
  only the /media route needs it (`wasm-media.ts`).
"""
import json
import time

import pytest

from helpers.app_surface import declared_absence

pytestmark = [pytest.mark.web, pytest.mark.tier_3]


def _require_web(driver) -> None:
    """The wasm chunk split (core plus per-route chunks) is a web-only build
    artifact — the other apps link `fauna-*` crates directly and never fetch
    per-route wasm chunks over the network (testing.md § Cross-app e2e
    conventions, point 15 — the wasm/JS runtime is web's own build shape)."""
    if not driver.is_web():
        declared_absence(
            driver,
            capability="the per-route wasm chunk split (core plus per-route chunks)",
            doc="testing.md § Cross-app e2e conventions, point 15 (web's "
            "wasm/JS build is a web-only artifact shape; native apps link "
            "fauna-* crates directly)",
        )


def _wasm_resources(driver) -> list[str]:
    """URLs of all `_bg.wasm` resources fetched so far in this page."""
    raw = driver._execute_js(
        "JSON.stringify(performance.getEntriesByType('resource').map(e => e.name))"
    )
    if not raw:
        return []
    urls = json.loads(raw)
    return [u for u in urls if "_bg.wasm" in u]


def test_feed_does_not_eagerly_load_media(logged_in_app):
    """Visiting /feed must not pull the media chunk.

    This is the central regression test: a route chunk imported eagerly from
    a first-paint route (or from the layout) puts its whole download on every
    first paint.
    """
    driver = logged_in_app.driver
    _require_web(driver)

    driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    driver.wait_for("feed-view", timeout=10)

    seen = _wasm_resources(driver)
    assert any("fauna_wasm_bg.wasm" in u for u in seen), \
        f"core wasm not loaded; saw {seen}"
    assert not any("fauna_wasm_media_bg.wasm" in u for u in seen), \
        f"media wasm leaked into /feed; saw {seen}"


def test_conversations_does_not_eagerly_load_media(logged_in_app):
    """Visiting /conversations must not pull the media chunk."""
    driver = logged_in_app.driver
    _require_web(driver)

    driver.set_state({"nav": {"stack": [{"view": "conversations"}]}})
    driver.wait_for("new-conversation-button", timeout=10)

    seen = _wasm_resources(driver)
    assert not any("fauna_wasm_media_bg.wasm" in u for u in seen), \
        f"media wasm fetched eagerly on /conversations; saw {seen}"


def test_onboarding_loads_onboarding_wasm(app):
    """Visiting /onboarding fetches the onboarding chunk."""
    driver = app.driver
    _require_web(driver)

    driver.set_state({"nav": {"stack": [{"view": "onboarding"}]}})
    driver.wait_for("create-identity-button", timeout=10)

    # The chunk loads when the page initializes the onboarding machine —
    # poll for a few seconds since it's behind a dynamic import.
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if any("fauna_wasm_onboarding_bg.wasm" in u for u in _wasm_resources(driver)):
            return
        time.sleep(0.2)

    seen = _wasm_resources(driver)
    pytest.fail(f"onboarding wasm not loaded on /onboarding; saw {seen}")

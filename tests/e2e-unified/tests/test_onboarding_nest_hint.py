"""The nest hint on web: ``/app/onboarding?nest=<target>`` pre-fills the
handle-entry page's domain part (``onboarding.md`` § 2 Handle entry → *Nest
hint*).

The ``nest`` query parameter is what a nest's central-origin redirect carries
(``web-content-hosting.md`` § The nest-served `/app/` and the central origin).
The SPA hands the raw value to the shared onboarding machine's
``set_nest_hint``, which classifies it and pre-fills — or drops it silently.
It is a prefill and nothing more: the check still runs on the user's press.

Web-only by construction: the query parameter is a web entry point (a native
deep link carrying the same hint is later work, through the same machine call).
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer
from conftest import get_available_apps
from drivers import create_driver

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.web, pytest.mark.tier_3]

_HANDLE_VALUE_JS = "document.querySelector('[data-testid=\"handle-input\"]')?.value ?? null"


def _open_onboarding_with(spa_url: str, query: str):
    """A fresh browser context, unauthenticated, entering onboarding with
    ``query``; walks an imported identity to the handle-entry page."""
    driver = create_driver("web")
    driver.launch({"url": spa_url + "/app/"})
    driver.eval_js("localStorage.clear()")
    base = (driver._spa_url or "").rstrip("/")
    driver._post("/navigate", {"url": f"{base}/onboarding?{query}"})
    driver._ensure_agent()
    # The shared walk to handle entry (import the deterministic test key —
    # `go_to_handle_entry`'s route), never a page-local reimplementation.
    onboarding = ActionLayer(driver).onboarding
    onboarding.import_key(onboarding._IMPORT_KEY_FOR_HANDLE_TESTS)
    driver.wait_for("handle-input", timeout=15)
    return driver


def _handle_value(driver, want: str | None = None, timeout: float = 10.0) -> str | None:
    deadline = time.monotonic() + timeout
    value = driver.eval_js(_HANDLE_VALUE_JS)
    while want is not None and value != want and time.monotonic() < deadline:
        time.sleep(0.2)
        value = driver.eval_js(_HANDLE_VALUE_JS)
    return value


@pytest.mark.feature("nest-serves-the-app")
def test_the_nest_hint_prefills_the_domain_part_and_the_check_still_runs(spa_url):
    # A loopback target on a closed port: it classifies (a direct nest
    # address), and the check it later runs fails fast without the network.
    driver = _open_onboarding_with(spa_url, "nest=localhost:1")
    try:
        value = _handle_value(driver, want="@localhost:1")
        assert value == "@localhost:1", (
            f"the hint must pre-fill the domain part; handle-input is {value!r}"
        )
        # A prefill, not an auto-connect: nothing has been checked yet.
        assert not driver.is_enabled("handle-entry-continue-button"), (
            "the hint must not auto-check or auto-continue"
        )
        driver.clear_and_type("handle-input", "alice@localhost:1")
        driver.click("handle-check-button")
        deadline = time.monotonic() + 30
        message = ""
        while time.monotonic() < deadline and not message:
            if driver.is_visible("handle-message-area"):
                message = driver.get_text("handle-message-area") or ""
            time.sleep(0.3)
        assert message, "the check must still run on the user's press of Check"
    finally:
        driver.teardown()


@pytest.mark.feature("nest-serves-the-app")
def test_an_unclassifiable_nest_hint_is_dropped_silently(spa_url):
    driver = _open_onboarding_with(spa_url, "nest=evil.example%2Fpath%3Fx%3D1")
    try:
        assert _handle_value(driver) == "", "an unclassifiable hint must leave the field empty"
        error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
        assert not error, f"a dropped hint is never an error: {error!r}"
    finally:
        driver.teardown()

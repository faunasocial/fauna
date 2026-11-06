"""The nest hint on tui: a bare nest address on the command line
(``fauna-tui localhost:1``) pre-fills the handle-entry page's domain part
(``onboarding.md`` § 2 Handle entry → *Nest hint*; ``apps/tui.md`` § System
integration → *The nest hint*).

The tui twin of ``test_onboarding_nest_hint.py``: the same two tests and
assertions, entered through tui's own entry shape. tui hands the raw argument
to the shared onboarding machine's ``set_nest_hint`` when the first launch
decision lands on the wizard; the machine classifies it and pre-fills — or
drops it silently. It is a prefill and nothing more: the check still runs on
the user's press.
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
from conftest import _seeded_environment, get_available_apps
from drivers import create_driver

if "tui" not in get_available_apps():
    pytest.skip("tui client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tui, pytest.mark.tier_3]


def _launch_with_hint(tui_app_path, request, hint: str):
    """A fresh, signed-out tui launched with ``hint`` on its command line;
    walks an imported identity to the handle-entry page."""
    driver = create_driver("tui")
    driver.launch({
        "app_path": tui_app_path,
        "app_args": hint,
        "environment": _seeded_environment(request),
    })
    # The shared walk to handle entry (import the deterministic test key),
    # never a page-local reimplementation.
    ActionLayer(driver).onboarding.go_to_handle_entry()
    return driver


def _handle_value(driver, want: str | None = None, timeout: float = 10.0) -> str | None:
    # On tui an input's text is its value.
    deadline = time.monotonic() + timeout
    value = driver.get_text("handle-input")
    while want is not None and value != want and time.monotonic() < deadline:
        time.sleep(0.2)
        value = driver.get_text("handle-input")
    return value


@pytest.mark.feature("nest-serves-the-app")
def test_the_nest_hint_prefills_the_domain_part_and_the_check_still_runs(
    tui_app_path, request
):
    # A loopback target on a closed port: it classifies (a direct nest
    # address), and the check it later runs fails fast without the network.
    driver = _launch_with_hint(tui_app_path, request, "localhost:1")
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
def test_an_unclassifiable_nest_hint_is_dropped_silently(tui_app_path, request):
    driver = _launch_with_hint(tui_app_path, request, "evil.example/path?x=1")
    try:
        assert _handle_value(driver) == "", "an unclassifiable hint must leave the field empty"
        error = driver.get_text("error-message") if driver.is_visible("error-message") else ""
        assert not error, f"a dropped hint is never an error: {error!r}"
    finally:
        driver.teardown()

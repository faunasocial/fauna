"""The web bridge's ``GET /registry`` record carries ``text`` and ``attrs``.

The cross-app record contract (``drivers/base.py::registry_snapshot``) lets a
whole-frame comparison see a change that leaves the id set alone: a copy
button's caption turning "Copied", a composer field gaining text, a segment or
view toggle becoming the current choice. The actuation sweep's ``signature()``
compares exactly those fields, so a registry that omits them reports every such
control as "the frame did not change" (the web corpus of row 6659 read 26
violations, most of them this). These tests evaluate the bridge's own walker
(``web-bridge/server.py::_REGISTRY_SNAPSHOT_JS``) in a real Chromium over a
static page — no app, no nest.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_2

_SERVER = Path(__file__).resolve().parent.parent / "web-bridge" / "server.py"

_PAGE = """
<main data-testid="settings-view">
  <button data-testid="copy-btn">Copied</button>
  <textarea data-testid="post-field">hello</textarea>
  <input data-testid="secret-field" type="password" value="abc">
  <label><input data-testid="toggle" type="checkbox" checked> Dark</label>
  <button data-testid="segment" data-state="active" aria-selected="true">People</button>
  <a data-testid="tab" href="#x" aria-current="page">Feed</a>
  <div data-testid="declared" data-test-text="Title only"><button>Inner</button></div>
</main>
"""


def _walker() -> str:
    spec = importlib.util.spec_from_file_location("web_bridge_server_row_6659", _SERVER)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module._REGISTRY_SNAPSHOT_JS


@pytest.fixture(scope="module")
def records() -> dict[str, dict]:
    sync_api = pytest.importorskip("playwright.sync_api")
    from drivers.browser import launch_kwargs

    with sync_api.sync_playwright() as p:
        browser = p.chromium.launch(**launch_kwargs(p))
        try:
            page = browser.new_page()
            page.set_content(_PAGE)
            elements = page.evaluate(_walker())
        finally:
            browser.close()
    return {e["id"]: e for e in elements}


def test_text_follows_the_get_text_contract(records):
    assert records["copy-btn"]["text"] == "Copied"
    assert records["post-field"]["text"] == "hello"
    assert records["secret-field"]["text"] == "•••"
    assert records["toggle"]["text"] == "Dark"
    assert records["declared"]["text"] == "Title only"


def test_attrs_carry_the_painted_state_the_toggle_and_the_choice(records):
    assert records["toggle"]["attrs"]["checked"] == "true"
    segment = records["segment"]["attrs"]
    assert segment["state"] == "active"
    assert segment["selected"] == "true"
    assert records["tab"]["attrs"]["current"] == "page"
    # The addressing and gating markers are not state.
    assert "testid" not in segment and "offline-gate-declared" not in segment
    assert "test-text" not in records["declared"].get("attrs", {})


def test_a_toggle_is_actuable_and_never_editable(records):
    """A checkbox is clicked, never typed into — the shape tui (a toggle has no
    text field) and windows (Toggle pattern, no writable Value) publish. The
    actuation sweep's fill step takes every new ``editable`` record for a form
    field, so a checkbox read ``editable`` sent the sweep pressing Enter in
    Settings' push-notifications toggle and never clicking it."""
    assert records["toggle"]["actuable"] is True
    assert records["toggle"]["editable"] is False
    # A text-typed input stays a field.
    assert records["secret-field"]["editable"] is True
    assert records["secret-field"]["actuable"] is False

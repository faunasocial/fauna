"""Unit tests for ``PlatformDriver.is_absent`` — the honest form of a NEGATIVE
visibility read (e2e-conventions.md convention 6's rider, "a NEGATIVE
visibility read is vacuous on a scrollable surface").

``assert not driver.is_visible(x)`` asks each bridge's own ``/element/visible``,
and the bridges do not agree on what it means. windows' answers
``!IsOffscreen`` — a VIEWPORT predicate — so a defect that really painted ``x``
below the fold passes the assertion it was written to fail. Every other bridge
answers a rendered-or-not question with no viewport in it (android:
``findAll(id).isNotEmpty()``; linux: ``is_mapped()``; apple/tui: the registry's
visible slots; web: Playwright's ``is_visible``).

``is_absent`` is the one primitive that means "the app has not put this element
in its tree", spelled per driver:

  * **base default — ``not is_visible``.** Unchanged on every app but windows,
    so adopting it is behaviour-preserving there. It is NOT ``count == 0``:
    web's ``/element/count`` is ``locator.count()``, which counts a hidden-in-DOM
    element, so ``count == 0`` would red a correct web build.
  * **windows — ``count == 0``.** ``Actions.Count`` and ``Actions.IsVisible``
    share ``WalkScope`` + ``FindAll``; ``IsVisible`` just adds ``&&
    !IsOffscreen``. So the count is the same question minus the viewport
    confound, and it issues no UIA scroll (which can take the FlaUI bridge down).

tier_1: pure Python, no nest binary, no client driver, no bridge process.
"""
import json
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.base import PlatformDriver


class _Bridge:
    """Stand-in for ``HttpBridgeDriver._get``: records every route asked, and
    answers ``/element/visible`` and ``/element/count`` from fixed values."""

    def __init__(self, *, visible, count):
        self.visible = visible
        self.count = count
        self.calls: list[tuple[str, dict]] = []

    def __call__(self, path, params=None):
        self.calls.append((path, params or {}))
        if path == "/element/visible":
            return {"visible": self.visible}
        if path == "/element/count":
            return {"count": self.count}
        raise AssertionError(f"is_absent must not touch {path}")

    @property
    def routes(self) -> list[str]:
        return [p for p, _ in self.calls]


def _bare(cls, bridge):
    """A real driver class with its transport replaced — built without
    ``__init__`` so no bridge process, port or app is involved."""
    d = object.__new__(cls)
    d._get = bridge
    return d


class _Stub(PlatformDriver):
    """Concrete ``PlatformDriver`` that COUNTS its reads, so the base default's
    contract — it asks ``is_visible`` and never ``count`` — is observable."""

    def __init__(self, *, visible, count):
        self._visible = visible
        self._count = count
        self.count_reads = 0
        self.visible_reads = 0

    def is_visible(self, element_id, *, scope=None):
        self.visible_reads += 1
        return self._visible

    def count(self, element_id, *, scope=None):
        self.count_reads += 1
        return self._count

    # --- remaining abstract methods: inert stubs, never reached here ---
    def launch(self, config): ...
    def teardown(self): ...
    def find_element(self, element_id, index=0, *, scope=None): ...
    def click(self, element_id, index=0, *, scope=None): ...
    def type_text(self, element_id, text, *, scope=None): ...
    def clear_and_type(self, element_id, text, *, scope=None): ...
    def press_key(self, element_id, key, *, scope=None): ...
    def get_text(self, element_id, index=0, *, scope=None): ...
    def get_attr(self, element_id, attribute, index=0, *, scope=None): ...
    def wait_for(self, element_id, timeout=10.0, *, scope=None): ...
    def set_input_files(self, element_id, files): ...
    def screenshot(self, name): ...
    def scroll(self, direction="down", *, scope=None): ...
    def select(self, element_id, value, *, scope=None): ...
    def get_state(self, path=None): ...
    def set_state(self, patch): ...


# --- the base default ----------------------------------------------------


def test_base_default_is_not_visible():
    assert _Stub(visible=False, count=0).is_absent("x") is True
    assert _Stub(visible=True, count=1).is_absent("x") is False


def test_base_default_never_reads_count():
    """web's ``count`` includes a hidden-in-DOM element, so an element that is
    in the DOM but hidden (visible=False, count=1) must still read as ABSENT —
    that is what the assertion ``assert not is_visible(x)`` has always meant
    there, and what adopting ``is_absent`` must preserve."""
    d = _Stub(visible=False, count=1)

    assert d.is_absent("x") is True
    assert d.count_reads == 0, "the default must not consult count()"
    assert d.visible_reads == 1


# --- windows: tree membership, not the viewport --------------------------


def test_windows_an_offscreen_but_painted_element_is_NOT_absent():
    """The whole point. windows' ``/element/visible`` is ``!IsOffscreen``, so an
    element the defect really did paint below the fold answers visible=False
    while ``count`` reads 1 the whole time. ``not is_visible`` calls that
    'absent'; ``is_absent`` must not."""
    from drivers.windows import WindowsBridgeDriver

    bridge = _Bridge(visible=False, count=1)
    d = _bare(WindowsBridgeDriver, bridge)

    assert d.is_absent("forged-badge") is False


def test_windows_an_element_out_of_the_tree_is_absent():
    from drivers.windows import WindowsBridgeDriver

    bridge = _Bridge(visible=False, count=0)
    d = _bare(WindowsBridgeDriver, bridge)

    assert d.is_absent("forged-badge") is True


def test_windows_reads_only_the_count_route_and_never_scrolls():
    """No ``/element/visible`` (that is the viewport predicate being avoided) and
    no scroll — a UIA ScrollIntoView deep in a scroll container can take the
    FlaUI bridge down outright (``is_nav_tab_revealed`` records it)."""
    from drivers.windows import WindowsBridgeDriver

    bridge = _Bridge(visible=False, count=0)
    d = _bare(WindowsBridgeDriver, bridge)

    d.is_absent("forged-badge")

    assert bridge.routes == ["/element/count"]


def test_windows_scope_rides_the_count_read():
    """A scoped absence ('no badge on THIS card') must stay scoped, or it
    silently widens to the whole page."""
    from drivers.windows import WindowsBridgeDriver

    bridge = _Bridge(visible=False, count=0)
    d = _bare(WindowsBridgeDriver, bridge)

    d.is_absent("forged-badge", scope="post-card[1]")

    (path, params), = bridge.calls
    assert path == "/element/count"
    assert json.loads(params["scope"]) == [{"id": "post-card", "index": 1}]


# --- the action layer: `app.is_absent(...)` is `driver.is_absent(...)` ---------


def test_the_action_layer_forwards_is_absent_with_its_scope():
    """Many negative reads are spelled ``app.is_absent(...)`` — the same reach as
    ``app.is_visible`` / ``app.count`` — so the action layer must hand the
    element and the scope to the driver unchanged, or a scoped absence silently
    widens to the whole page."""
    from actions import ActionLayer
    from drivers.http_bridge import HttpBridgeDriver

    class _RecordingAbsentDriver(HttpBridgeDriver):
        """``is_absent`` records its arguments, so the forwarding is observable
        independent of any driver's implementation."""

        def is_absent(self, element_id, *, scope=None):
            self.absent_calls.append((element_id, scope))
            return True

    # Built without ``__init__`` — no bridge process, port or app.
    driver = _RecordingAbsentDriver.__new__(_RecordingAbsentDriver)
    driver.absent_calls = []
    app = ActionLayer(driver)

    assert app.is_absent("badge") is True
    assert app.is_absent("badge", scope="post-card[1]") is True
    assert driver.absent_calls == [("badge", None), ("badge", "post-card[1]")]


# --- who overrides -------------------------------------------------------


def _bridge_driver_classes():
    from drivers.android import AndroidBridgeDriver
    from drivers.inprocess_agent import InProcessAgentDriver
    from drivers.ios import IosInProcessDriver
    from drivers.linux import LinuxBridgeDriver
    from drivers.macos import MacosInProcessDriver
    from drivers.tui import TuiDriver
    from drivers.web import WebBridgeDriver

    return (
        AndroidBridgeDriver,
        InProcessAgentDriver,
        IosInProcessDriver,
        LinuxBridgeDriver,
        MacosInProcessDriver,
        TuiDriver,
        WebBridgeDriver,
    )


def test_only_windows_overrides_is_absent():
    """The override is an AUDITED act, not a default: it is right only where the
    bridge's ``/element/visible`` carries a viewport predicate AND its
    ``/element/count`` is the same lookup minus it. windows measured both
    (``Actions.cs`` ``Count`` vs ``IsVisible``); the other bridges were read
    (android ``ElementOps.isVisible == findAll().isNotEmpty()``, linux
    ``is_mapped()``, apple/tui registry slots, web Playwright ``is_visible``) and
    none has the viewport confound. An app that turns out to have it adds its
    own override — and updates this pin — deliberately."""
    from drivers.windows import WindowsBridgeDriver

    assert WindowsBridgeDriver.is_absent is not PlatformDriver.is_absent
    for cls in _bridge_driver_classes():
        assert cls.is_absent is PlatformDriver.is_absent, (
            f"{cls.__name__} overrides is_absent — that is only right if its "
            "/element/visible carries a viewport predicate (see this test's docstring)"
        )


def test_web_a_hidden_in_dom_element_is_absent_and_count_is_never_asked():
    """web through its REAL driver class: hidden-in-DOM (visible=False, count=1)
    must read as absent, and the route asked must be ``/element/visible`` only."""
    from drivers.web import WebBridgeDriver

    bridge = _Bridge(visible=False, count=1)
    d = _bare(WebBridgeDriver, bridge)
    d._page_id = "1"

    assert d.is_absent("hidden-toggle") is True
    assert bridge.routes == ["/element/visible"]

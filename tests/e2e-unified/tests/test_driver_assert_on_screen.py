"""Unit tests for ``PlatformDriver.assert_on_screen()`` — a geometry regression
guard over the live ``get_attr(id, "frame")`` read (window-space "x,y,w,h",
apple-bridge today). Catches the
"geo-parked off-screen" class of bug where an element stays registered and
individually-hittable-by-id while a layout overflow has shoved its frame to a
negative origin (the documented ~467pt controlsBar incident that geo-parked
media rows at x=-36) — a bug ``is_visible()`` alone does not catch, since the
registry's own on-screen check is exactly what missed it.

tier_1: pure Python, no nest binary, no client driver.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.base import PlatformDriver


class _FakeDriver(PlatformDriver):
    """Minimal concrete ``PlatformDriver``: only ``get_attr`` (what
    ``assert_on_screen`` reads) and ``diagnose``'s own accessors are real and
    configurable; the rest are inert stubs never touched on this path."""

    def __init__(self, *, frame=None, raise_on=()):
        self._frame = frame
        self._raise_on = set(raise_on)

    def _maybe_raise(self, name):
        if name in self._raise_on:
            raise NotImplementedError(f"{name} not implemented on this platform")

    def is_visible(self, element_id, *, scope=None):
        self._maybe_raise("is_visible")
        return self._frame is not None

    def count(self, element_id, *, scope=None):
        return 1 if self._frame is not None else 0

    def get_text(self, element_id, index=0, *, scope=None):
        return ""

    def get_attr(self, element_id, attribute, *, scope=None):
        self._maybe_raise("get_attr")
        assert attribute == "frame"
        return self._frame

    # --- remaining abstract methods: inert stubs (assert_on_screen never calls them) ---
    def launch(self, config): ...
    def teardown(self): ...
    def find_element(self, element_id, index=0, *, scope=None): ...
    def click(self, element_id, index=0, *, scope=None): ...
    def type_text(self, element_id, text, *, scope=None): ...
    def clear_and_type(self, element_id, text, *, scope=None): ...
    def press_key(self, element_id, key, *, scope=None): ...
    def wait_for(self, element_id, timeout=10.0, *, scope=None): ...
    def set_input_files(self, element_id, files): ...
    def screenshot(self, name): return Path("/dev/null")


class TestAssertOnScreen:
    def test_passes_for_a_normal_on_screen_frame(self):
        # Realized, positive origin, non-zero size — no exception.
        _FakeDriver(frame="12,34,200,40").assert_on_screen("media-row")

    def test_passes_for_a_frame_at_the_window_origin(self):
        # x=0,y=0 is a legitimate on-screen position, not "unrealized".
        _FakeDriver(frame="0,0,100,20").assert_on_screen("nav-bar")

    def test_raises_when_frame_is_empty(self):
        # Empty value = sentinel not realized (registry semantics, see the
        # Swift-side /element/attr?attr=frame doc comment).
        with pytest.raises(AssertionError, match="no live frame"):
            _FakeDriver(frame="").assert_on_screen("media-row")

    def test_raises_when_frame_is_none(self):
        with pytest.raises(AssertionError, match="no live frame"):
            _FakeDriver(frame=None).assert_on_screen("media-row")

    def test_raises_on_negative_x_geo_park(self):
        # The exact incident this helper exists to catch: a layout overflow
        # geo-parks the row at a negative x while it stays registered.
        with pytest.raises(AssertionError, match="geo-parked"):
            _FakeDriver(frame="-36,120,200,40").assert_on_screen("media-row")

    def test_raises_on_negative_y_geo_park(self):
        with pytest.raises(AssertionError, match="geo-parked"):
            _FakeDriver(frame="10,-50,200,40").assert_on_screen("media-row")

    def test_raises_on_zero_width(self):
        with pytest.raises(AssertionError, match="zero/negative size"):
            _FakeDriver(frame="10,20,0,40").assert_on_screen("media-row")

    def test_raises_on_zero_height(self):
        with pytest.raises(AssertionError, match="zero/negative size"):
            _FakeDriver(frame="10,20,200,0").assert_on_screen("media-row")

    def test_raises_on_malformed_frame_string(self):
        with pytest.raises(AssertionError, match="did not parse"):
            _FakeDriver(frame="not-a-frame").assert_on_screen("media-row")

    def test_get_attr_not_implemented_propagates(self):
        # Platforms without the frame endpoint (per get_attr's own docstring)
        # should surface the same NotImplementedError get_attr raises, not a
        # confusing AssertionError — callers guard the same way they already
        # guard any other get_attr(..., "frame") use.
        with pytest.raises(NotImplementedError):
            _FakeDriver(raise_on=("get_attr",)).assert_on_screen("media-row")

    def test_error_message_includes_diagnose_snapshot(self):
        # e2e rule 6 — a failure must diagnose itself, same discipline as
        # every other wait/assert site in this harness.
        with pytest.raises(AssertionError, match=r"\[media-row: visible="):
            _FakeDriver(frame="-36,120,200,40").assert_on_screen("media-row")

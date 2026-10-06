"""Unit tests for ``PlatformDriver.diagnose()`` — the shared self-diagnosing
element snapshot (e2e rule 6) that wait/poll sites fold into their timeout
messages so a failure classifies itself instead of forcing a debugger re-run.

tier_1: pure Python, no nest binary, no client driver.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.base import PlatformDriver


class _FakeDriver(PlatformDriver):
    """Minimal concrete ``PlatformDriver``: only the four accessors
    ``diagnose()`` reads are real (and configurable); the rest are inert stubs
    that ``diagnose()`` never touches. ``raise_on`` names accessors that should
    raise, to exercise the per-probe guard."""

    def __init__(self, *, visible=True, count=0, text="",
                 attrs=None, raise_on=()):
        self._visible = visible
        self._count = count
        self._text = text
        self._attrs = attrs or {}
        self._raise_on = set(raise_on)

    def _maybe_raise(self, name):
        if name in self._raise_on:
            raise NotImplementedError(f"{name} not implemented on this platform")

    def is_visible(self, element_id, *, scope=None):
        self._maybe_raise("is_visible")
        return self._visible

    def count(self, element_id, *, scope=None):
        self._maybe_raise("count")
        return self._count

    def get_text(self, element_id, index=0, *, scope=None):
        self._maybe_raise("get_text")
        return self._text

    def get_attr(self, element_id, attribute, *, scope=None):
        self._maybe_raise("get_attr")
        return self._attrs.get(attribute)

    # --- remaining abstract methods: inert stubs (diagnose never calls them) ---
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


class TestDiagnose:
    def test_includes_element_id(self):
        assert _FakeDriver().diagnose("post-card").startswith("[post-card:")

    def test_reports_visible_count_text(self):
        out = _FakeDriver(visible=True, count=3, text="hello world").diagnose("post-card")
        assert "visible=True" in out
        assert "count=3" in out
        assert "'hello world'" in out  # text via repr

    def test_not_visible_count_zero_reads_as_never_rendered(self):
        out = _FakeDriver(visible=False, count=0, text="").diagnose("event-card")
        assert "visible=False" in out
        assert "count=0" in out

    def test_attrs_read_only_when_requested(self):
        d = _FakeDriver(attrs={"state": "idle"})
        assert "state='idle'" in d.diagnose("recipient-resolve-status", attrs=("state",))
        assert "state=" not in d.diagnose("recipient-resolve-status")

    def test_multiple_attrs(self):
        d = _FakeDriver(attrs={"state": "error", "rail": "fauna"})
        out = d.diagnose("recipient-resolve-status", attrs=("state", "rail"))
        assert "state='error'" in out and "rail='fauna'" in out

    def test_attr_probe_failure_is_reported_not_masked(self):
        # The whole point of the per-probe guard: a platform that doesn't
        # implement an accessor must NOT crash diagnose() — the failure is
        # reported in place, and the timeout it was diagnosing still surfaces.
        out = _FakeDriver(raise_on=("get_attr",)).diagnose(
            "recipient-resolve-status", attrs=("state",))
        assert "state=<NotImplementedError" in out
        assert "visible=" in out and "count=" in out  # other probes unaffected

    def test_visibility_probe_failure_does_not_abort_others(self):
        out = _FakeDriver(raise_on=("is_visible",), count=5).diagnose("snapshot-item")
        assert "visible=<NotImplementedError" in out
        assert "count=5" in out

    def test_text_truncated_to_80_chars(self):
        out = _FakeDriver(text="x" * 200).diagnose("feed-post-text")
        assert "x" * 80 in out
        assert "x" * 81 not in out

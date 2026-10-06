"""Unit tests for the bulk indexed-read seam — ``PlatformDriver.get_texts()``
and ``get_attrs()``.

The per-element read contract (``get_text(id, index=i)`` / ``get_attr(id, attr,
index=i)``) costs one whole-tree find PER CALL on the native bridges, so a
helper that walks N same-id rows pays O(N) calls x O(N) walk. Measured on
windows against a 61-bubble conversations thread: ~1.25 s per call, ~390 s of
one test's 403 s spent inside two such loops.

These tests pin the two halves of the seam:

  * the **base default** — every driver, including ones whose bridge serves no
    bulk route, answers exactly what the per-element loop answered, so the
    seam is a speed-up and never a behaviour change; and
  * the **one-RPC override** — a bridge that declares support issues a single
    round trip, and one that 404s the route still answers correctly.

tier_1: pure Python, no nest binary, no client driver, no bridge process.
"""
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.base import PlatformDriver


class _CountingDriver(PlatformDriver):
    """Concrete ``PlatformDriver`` over a fixed row list that COUNTS its
    per-element reads — the base default's whole job is to make the same reads
    the hand-written loop made, so the count is part of the contract."""

    def __init__(self, texts=(), attrs=None):
        self._texts = list(texts)
        self._attrs = attrs or {}
        self.text_reads: list[int] = []
        self.attr_reads: list[tuple[str, int]] = []
        self.count_reads = 0

    def count(self, element_id, *, scope=None):
        self.count_reads += 1
        return len(self._texts)

    def get_text(self, element_id, index=0, *, scope=None):
        self.text_reads.append(index)
        return self._texts[index]

    def get_attr(self, element_id, attribute, index=0, *, scope=None):
        self.attr_reads.append((attribute, index))
        return self._attrs.get(attribute, [None] * len(self._texts))[index]

    # --- remaining abstract methods: inert stubs, never reached here ---
    def launch(self, config): ...
    def teardown(self): ...
    def find_element(self, element_id, index=0, *, scope=None): ...
    def click(self, element_id, index=0, *, scope=None): ...
    def type_text(self, element_id, text, *, scope=None): ...
    def clear_and_type(self, element_id, text, *, scope=None): ...
    def press_key(self, element_id, key, *, scope=None): ...
    def is_visible(self, element_id, *, scope=None): ...
    def wait_for(self, element_id, timeout=10.0, *, scope=None): ...
    def set_input_files(self, element_id, files): ...
    def screenshot(self, name): ...
    def scroll(self, direction="down", *, scope=None): ...
    def select(self, element_id, value, *, scope=None): ...
    def get_state(self, path=None): ...
    def set_state(self, patch): ...


def test_get_texts_default_matches_the_per_element_loop():
    """The base default answers exactly what ``[get_text(i) for i in
    range(count())]`` answered — same values, same order, same reads."""
    d = _CountingDriver(texts=["alpha", "beta", "gamma"])

    assert d.get_texts("row") == ["alpha", "beta", "gamma"]
    assert d.text_reads == [0, 1, 2]
    assert d.count_reads == 1


def test_get_attrs_default_matches_the_per_element_loop():
    d = _CountingDriver(
        texts=["a", "b", "c"],
        attrs={"selected": ["false", "true", "false"]},
    )

    assert d.get_attrs("row", "selected") == ["false", "true", "false"]
    assert d.attr_reads == [("selected", 0), ("selected", 1), ("selected", 2)]


def test_bulk_reads_of_an_absent_element_are_empty_not_an_error():
    """Zero matches is ``[]`` — the same answer the loop gave (``range(0)``).
    A bulk read must never turn "nothing on screen" into an exception, or
    every caller grows a guard the per-element loop never needed."""
    d = _CountingDriver(texts=[])

    assert d.get_texts("row") == []
    assert d.get_attrs("row", "selected") == []
    assert d.text_reads == []


def test_get_attrs_preserves_none_for_an_unset_attribute():
    """``None`` (attribute absent / doesn't apply) is a real answer in the
    per-element contract and must survive the bulk read — collapsing it to
    ``""`` would read as "set, but empty"."""
    d = _CountingDriver(texts=["a", "b"], attrs={"selected": ["true", None]})

    assert d.get_attrs("row", "selected") == ["true", None]


# --- the one-RPC override ------------------------------------------------


class _RecordingBridge:
    """Stand-in for ``HttpBridgeDriver._get``: records every route it is asked
    for, and can be told to 404 (``LookupError``) a route the way a bridge
    without it does."""

    def __init__(self, *, payloads=None, absent=()):
        self.payloads = payloads or {}
        self.absent = set(absent)
        self.calls: list[tuple[str, dict]] = []

    def __call__(self, path, params=None):
        self.calls.append((path, params or {}))
        if path in self.absent:
            raise LookupError(f"Unknown route: GET {path}")
        return self.payloads[path]

    @property
    def routes(self) -> list[str]:
        return [p for p, _ in self.calls]


def _bridge_driver(bridge, *, supports_bulk):
    """A bare ``HttpBridgeDriver`` with its transport replaced — constructed
    without ``__init__`` so no bridge process, port or app is involved."""
    from drivers.http_bridge import HttpBridgeDriver

    class _Driver(HttpBridgeDriver):
        _supports_bulk_reads = supports_bulk

    d = _Driver.__new__(_Driver)
    d._get = bridge
    return d


def test_bulk_capable_bridge_reads_a_whole_column_in_one_round_trip():
    """The point of the seam: N rows, ONE request. A per-element loop over 61
    bubbles is 62 round trips; this is 1."""
    bridge = _RecordingBridge(payloads={
        "/element/texts": {"texts": ["alpha", "beta", "gamma"]},
        "/element/attrs": {"values": ["false", "true", None]},
    })
    d = _bridge_driver(bridge, supports_bulk=True)

    assert d.get_texts("dm-message-text") == ["alpha", "beta", "gamma"]
    assert d.get_attrs("dm-message-timestamp", "selected") == ["false", "true", None]

    assert bridge.routes == ["/element/texts", "/element/attrs"]
    assert bridge.calls[1][1]["attr"] == "selected"


def test_a_bridge_without_the_route_falls_back_to_the_per_element_loop():
    """A bridge that declares support but 404s the route (an app built before
    the route landed, a partial roll-out) must still ANSWER. The fallback is
    the inherited loop, so the answer is identical — only slower."""
    bridge = _RecordingBridge(
        payloads={
            "/element/count": {"count": 2},
            "/element/text": {"text": "row"},
        },
        absent=("/element/texts",),
    )
    d = _bridge_driver(bridge, supports_bulk=True)

    assert d.get_texts("row") == ["row", "row"]
    assert bridge.routes == [
        "/element/texts", "/element/count", "/element/text", "/element/text",
    ]


def test_a_driver_that_does_not_declare_support_never_probes_the_route():
    """No declaration, no round trip: a bridge that will never serve the route
    must not pay a failing request per call (the ``_supports_scroll_into_view``
    rule, applied here)."""
    bridge = _RecordingBridge(payloads={
        "/element/count": {"count": 1},
        "/element/attr": {"value": "true"},
    })
    d = _bridge_driver(bridge, supports_bulk=False)

    assert d.get_attrs("row", "selected") == ["true"]
    assert "/element/attrs" not in bridge.routes


def test_scope_rides_the_bulk_read_the_same_way_it_rides_a_single_one():
    """A scoped bulk read must stay scoped, or it silently widens to the whole
    frame and answers about rows the caller never asked about."""
    bridge = _RecordingBridge(payloads={"/element/texts": {"texts": ["x"]}})
    d = _bridge_driver(bridge, supports_bulk=True)

    d.get_texts("row", scope="panel")

    assert "scope" in bridge.calls[0][1]

"""Unit tests for the scope path parser."""

import sys
from pathlib import Path

# Add parent dir so we can import drivers.scope

import pytest

pytestmark = pytest.mark.tier_1
sys.path.insert(0, str(Path(__file__).parent.parent))

from drivers.scope import ScopeStep, parse_scope, scope_to_wire, scope_from_wire


class TestParseScope:
    def test_none_returns_empty(self):
        assert parse_scope(None) == []

    def test_empty_string_returns_empty(self):
        assert parse_scope("") == []

    def test_single_element(self):
        assert parse_scope("post-card") == [ScopeStep("post-card", 0)]

    def test_single_element_with_index(self):
        assert parse_scope("post-card[2]") == [ScopeStep("post-card", 2)]

    def test_single_element_index_zero(self):
        assert parse_scope("post-card[0]") == [ScopeStep("post-card", 0)]

    def test_two_levels(self):
        result = parse_scope("post-card[2]/tag-chip")
        assert result == [ScopeStep("post-card", 2), ScopeStep("tag-chip", 0)]

    def test_two_levels_both_indexed(self):
        result = parse_scope("conversation-view[1]/message-bubble[3]")
        assert result == [
            ScopeStep("conversation-view", 1),
            ScopeStep("message-bubble", 3),
        ]

    def test_three_levels(self):
        result = parse_scope("a/b[1]/c[2]")
        assert result == [
            ScopeStep("a", 0),
            ScopeStep("b", 1),
            ScopeStep("c", 2),
        ]

    def test_underscores_in_id(self):
        assert parse_scope("my_element[0]") == [ScopeStep("my_element", 0)]

    def test_alphanumeric_id(self):
        assert parse_scope("card2[1]") == [ScopeStep("card2", 1)]

    def test_invalid_empty_segment(self):
        import pytest
        with pytest.raises(ValueError, match="Invalid scope segment"):
            parse_scope("post-card//tag-chip")

    def test_invalid_no_id(self):
        import pytest
        with pytest.raises(ValueError, match="Invalid scope segment"):
            parse_scope("[2]")

    def test_invalid_negative_index(self):
        import pytest
        with pytest.raises(ValueError, match="Invalid scope segment"):
            parse_scope("post-card[-1]")

    def test_invalid_unclosed_bracket(self):
        import pytest
        with pytest.raises(ValueError, match="Invalid scope segment"):
            parse_scope("post-card[2")

    def test_invalid_non_numeric_index(self):
        import pytest
        with pytest.raises(ValueError, match="Invalid scope segment"):
            parse_scope("post-card[abc]")


class TestScopeToWire:
    def test_empty_returns_none(self):
        assert scope_to_wire([]) is None

    def test_single_step(self):
        result = scope_to_wire([ScopeStep("post-card", 2)])
        assert result == [{"id": "post-card", "index": 2}]

    def test_multiple_steps(self):
        steps = [ScopeStep("post-card", 2), ScopeStep("tag-chip", 0)]
        result = scope_to_wire(steps)
        assert result == [
            {"id": "post-card", "index": 2},
            {"id": "tag-chip", "index": 0},
        ]


class TestScopeFromWire:
    def test_none_returns_empty(self):
        assert scope_from_wire(None) == []

    def test_empty_returns_empty(self):
        assert scope_from_wire([]) == []

    def test_single_step(self):
        result = scope_from_wire([{"id": "post-card", "index": 2}])
        assert result == [ScopeStep("post-card", 2)]

    def test_index_defaults_to_zero(self):
        result = scope_from_wire([{"id": "post-card"}])
        assert result == [ScopeStep("post-card", 0)]

    def test_roundtrip(self):
        original = "post-card[2]/tag-chip[1]"
        steps = parse_scope(original)
        wire = scope_to_wire(steps)
        roundtripped = scope_from_wire(wire)
        assert roundtripped == steps

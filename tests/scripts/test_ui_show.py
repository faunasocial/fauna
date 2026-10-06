"""Pin `scripts/ui-show.py` — the row-addressed reader for the UI spec.

`tests/e2e-unified/ui.yaml` is the canonical UI spec all seven apps must match
exactly, and at 867,925 B it is 3.31x the 262,144-byte Read-tool ceiling: no
session can open it. It is named beside `docs/goal/` in the authority table for
*how should things be*, and the UI rules ask a session to conform to it and to
judge whether it is internally consistent — instructions that have been
unfollowable for as long as the file has been this size.

A file answers the ceiling either by getting smaller **or** by growing a reader
that takes one row at a time. This is the second route: nothing downstream
changes, every existing loader keeps reading one file, and the spec becomes
readable again a page/component/element at a time.

Two properties carry the whole design and are pinned hardest below:

1. **Verbatim.** A slice is the file's own bytes, never a re-serialization.
   Rule A is "match ui.yaml exactly", so a reader that round-trips through a
   YAML dumper — dropping comments, re-quoting, re-wrapping — would hand back
   something the spec does not say. Comments in this file carry load-bearing
   scope notes ("Every element listed here MUST be implemented by all 7 apps").

2. **Total reachability.** Every addressable node is under the output cap, and
   any node that is not falls back to a child index whose addresses each
   resolve. Without this the reader would merely move the truncation, which is
   the failure it exists to end.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_SCRIPT = Path(__file__).resolve().parents[2] / "scripts" / "ui-show.py"


def _load():
    spec = importlib.util.spec_from_file_location("ui_show", _SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


ui = _load()

RAW = ui.UI_YAML.read_text(encoding="utf-8").splitlines(keepends=True)


# ---------------------------------------------------------------------------
# Verbatim — property 1.
# ---------------------------------------------------------------------------


def test_a_page_slice_is_the_files_own_bytes():
    """Not a re-serialization: the slice must be findable in the file as-is."""
    node = ui.resolve("pages.feed")
    text = "".join(RAW[node.start - 1 : node.end])
    assert node.text() == text
    assert text in "".join(RAW)


def test_a_slice_keeps_the_comments_that_carry_scope():
    """A YAML round-trip would drop these, and they are load-bearing prose."""
    text = ui.resolve("pages.feed").text()
    assert "MUST be implemented by all 7 apps" in text


def test_a_slice_starts_at_its_own_key_and_stops_before_the_next():
    """The boundary is indentation, so a node never swallows its sibling."""
    node = ui.resolve("pages.feed")
    assert RAW[node.start - 1].startswith("  feed:")
    after = RAW[node.end]
    assert after.strip() == "" or not after.startswith("    ")


# ---------------------------------------------------------------------------
# Addressing.
# ---------------------------------------------------------------------------


def test_every_top_level_section_is_addressable():
    """The nine sections of the spec, each reachable by name."""
    for section in ui.sections():
        assert ui.resolve(section).start >= 1


def test_a_bare_key_resolves_when_it_is_unique():
    """A session thinks "show me the feed page", not "pages.feed"."""
    assert ui.resolve("feed").address == "pages.feed"


def test_an_ambiguous_bare_key_reports_every_candidate():
    """117 keys name both a page/component and an element. Report, never guess."""
    with pytest.raises(ui.Ambiguous) as e:
        ui.resolve("settings-logs")
    assert set(e.value.candidates) == {"pages.settings-logs", "elements.settings-logs"}


def test_a_section_name_wins_over_a_member_of_the_same_name():
    """`pages` and `elements` also occur as member keys (global.elements).

    Without this precedence the two largest sections would be unaddressable by
    their own names, which is the opposite of a reader.
    """
    assert ui.resolve("elements").address == "elements"
    assert ui.resolve("pages").address == "pages"
    assert ui.resolve("global.elements").address == "global.elements"


def test_an_unknown_address_names_the_nearest_thing():
    with pytest.raises(ui.NotFound) as e:
        ui.resolve("pages.no-such-page")
    assert "no-such-page" in str(e.value)


def test_a_qualified_address_beats_a_bare_collision():
    """`elements.error-message` is exact even though the name appears widely."""
    assert ui.resolve("elements.error-message").address == "elements.error-message"


# ---------------------------------------------------------------------------
# Total reachability — property 2, the one that makes the spec readable again.
# ---------------------------------------------------------------------------


def test_every_addressable_node_fits_the_cap_or_has_a_child_index():
    """The whole spec is reachable without truncation. This is the deliverable.

    A node over the cap is not an error and not a truncation — it renders as an
    index of its children, each with an address that resolves.
    """
    for section in ui.sections():
        for node in ui.children(section):
            out = ui.render(node)
            assert len(out) <= ui.OUTPUT_CAP, f"{node.address} rendered {len(out)} chars"


def test_the_one_oversized_page_falls_back_to_a_child_index():
    """pages.settings is 33,606 B — over the cap, and the reason this exists."""
    node = ui.resolve("pages.settings")
    assert node.size() > ui.OUTPUT_CAP
    out = ui.render(node)
    assert len(out) <= ui.OUTPUT_CAP
    assert "pages.settings." in out


def test_every_address_a_child_index_prints_actually_resolves():
    """The round-trip property: an index that lies is worse than no index."""
    node = ui.resolve("pages.settings")
    for child in ui.children("pages.settings"):
        assert ui.resolve(child.address).start >= 1
    assert node.size() > 0


def test_a_section_too_large_to_index_says_how_to_narrow():
    """`elements` holds 1,688 keys — even the name list overflows the cap."""
    out = ui.render(ui.resolve("elements"))
    assert len(out) <= ui.OUTPUT_CAP
    assert "--find" in out


# ---------------------------------------------------------------------------
# Search.
# ---------------------------------------------------------------------------


def test_find_locates_a_known_element_by_substring():
    hits = ui.find("error-message")
    assert "elements.error-message" in [h.address for h in hits]


def test_find_is_bounded_and_says_when_it_truncated():
    out = ui.render_find("e")
    assert len(out) <= ui.OUTPUT_CAP


# ---------------------------------------------------------------------------
# The table of contents.
# ---------------------------------------------------------------------------


def test_the_toc_fits_the_cap_and_names_every_section():
    out = ui.render_toc()
    assert len(out) <= ui.OUTPUT_CAP
    for section in ui.sections():
        assert section in out


def test_the_toc_reports_the_breach_it_exists_to_answer():
    """A cold reader must learn immediately why they cannot just open the file.

    Asserted against the file's live size, never a pinned literal: ui.yaml grows
    with every page, and a literal (``"867,"`` / ``"3.3"``) turned the whole
    script-selftest gate red the day the spec crossed 890 KB.
    """
    out = ui.render_toc()
    total = ui.UI_YAML.stat().st_size
    assert total > ui.CEILING, "the breach this TOC exists to explain has closed"
    assert f"{total:,} B" in out
    assert f"{total / ui.CEILING:.2f}x the {ui.CEILING:,} B Read-tool ceiling" in out

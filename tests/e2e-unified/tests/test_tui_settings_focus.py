"""Settings rail focus — exactly one row ever paints focused
(`apps/fauna-tui/src/settings/root.rs`).

Until 2026-10-04 most rail rows were id-less (`Element::gesture_button
(String::new(), ...)`); since then each is `settings-nav-row[<key>]` (ui.yaml
`navigation.sub_page_nav_rows`). The bug this file pins lived in the id-less
era but is about focus identity, not ids: a live human found tui painting EVERY
id-less row `REVERSED`-highlighted simultaneously the moment focus landed on
any one of them — `ui.rs`'s paint code used to compare focus by `element.id`
STRING, and every blank id is `""`, so every blank-id row matched at once
(the same would hold for any two elements sharing an id). Fixed by `App::focused_index`, a POSITION comparison
(`app.rs::focused_index`), replacing the id-string comparison in
`ui.rs::element_lines` — pinned at the unit layer by
`ui::tests::elements_sharing_an_id_do_not_all_paint_focused`.

Focus itself is not a registry attribute, so this file drives the fix through three additive, minimal, non-ui.yaml surfaces this
track added rather than reading pixels (no e2e driver on any app does that):

- the `switch_pane` test command, which calls the SAME `App::enter_page_zone`
  a real `Right` keystroke calls. Needed because a `nav` patch is zone-
  agnostic (`App::apply` never touches `self.zone`) and a fresh authenticated
  session starts in `Zone::Sidebar` (`App::new`) — without it the rail's own
  focus ring never has the keyboard, and `focused_line_count` reads 0
  regardless of the bug.
- the `focus_move` test command (`automation.rs::dispatch_command`), which
  calls the SAME `App::focus_next`/`focus_prev` a real Tab/Down or BackTab/Up
  keystroke calls (`app.rs::handle_key`) — the real human path, minus only
  `KeyEvent` parsing, which this bug class never lived in (`/element/key` has
  no consumers and is a documented no-op — `fauna-e2e-agent::handle`).
- `focused_line_count`, published automation state that reuses the EXACT
  paint-time `element_lines` call `ui::render_page` paints through, counting
  how many lines currently carry the focus-highlight style — the e2e-visible
  twin of the unit-layer invariant above.

tier_3 (a real authenticated `fauna-tui` process against a real nest) — the
bug lives in paint/registry wiring no mock can stand in for.
"""
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3, pytest.mark.tui]

# Comfortably more than the root's focusable rows (the copy button, the
# account row and ~26 settings-nav-row rows — settings/root.rs), so a full
# forward/backward traversal visits every row at least once and wraps around
# at least once.
_TRAVERSAL_STEPS = 40


def _open_settings_root(app):
    d = app.driver
    d.set_state({"nav": {"stack": [{"view": "settings"}]}})
    d.wait_for("account-settings-link", timeout=10)
    # A `nav` patch is zone-agnostic (`App::apply` never touches `self.zone`),
    # and a fresh authenticated session starts in `Zone::Sidebar` (`App::new`)
    # — so without this, the rail's own focus ring (`Zone::Page`) never gets
    # the keyboard at all, and `focused_line_count` would read 0 for every
    # step below. `switch_pane` calls the SAME `App::enter_page_zone` a real
    # `Right` keystroke calls.
    d.call_command("switch_pane", {"pane": "page"})


def _walk_and_assert_single_focus(app, direction: str):
    d = app.driver
    for step in range(_TRAVERSAL_STEPS):
        count = d.get_state("focused_line_count")
        assert count == 1, (
            f"expected exactly one focused row after {step} focus_{direction}() "
            f"steps into the Settings rail, got {count}: {app.error_text()!r}"
        )
        d.call_command("focus_move", {"direction": direction, "times": 1})


def test_settings_rail_never_paints_more_than_one_row_focused(logged_in_app):
    """Walking the focus ring the full length of the Settings rail never
    paints more than one row focused.

    Red before the fix: the moment focus landed on any blank-id row,
    `focused_line_count` would have read the full blank-id row count (many),
    not 1 — verified by temporarily reverting `ui.rs::element_lines`'s focus
    comparison to the pre-fix `element.id` string check (see the fix commit's
    diff) and re-running this test, which fails as expected.
    """
    _open_settings_root(logged_in_app)
    _walk_and_assert_single_focus(logged_in_app, "next")


def test_settings_rail_focus_wraps_backward_too(logged_in_app):
    """The same invariant holds walking BackTab/Up (`focus_prev`) — the ring's
    other real direction, and a distinct code path (`app.rs::focus_prev`) from
    the forward walk above."""
    _open_settings_root(logged_in_app)
    _walk_and_assert_single_focus(logged_in_app, "prev")

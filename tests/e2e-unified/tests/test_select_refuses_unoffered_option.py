"""A picker must refuse a value the app never rendered.

``e2e-conventions.md`` § convention 11, the twin rule (ratified 2026-08-03).
Convention 11 has always forbidden an agent *dropping* a command; this pins the
mirror image, which is worse because it makes the test **pass**: an agent that
writes ``select(id, value)`` straight through to the state machine, without
checking ``value`` against the options that frame painted, drives the app to a
state no keystroke can reach. That is convention 8's forbidden "API-only
mutation path" wearing a UI costume, and it makes the entire failure mode *"the
option list is wrong"* structurally untestable through ``select``.

Not hypothetical. The first version of
``test_upload_into_a_freshly_created_empty_folder`` PASSED against the un-fixed
code it was written to pin, because it selected a folder that Media had never
offered — the defect (an empty folder could never be an upload target) was *half* an option-list bug, and a live user found it, not the
1147-test suite.

**Why this asserts an invariant rather than a status code.** Each app refuses by
a different mechanism, and that is correct rather than drift: the four that
actuate a real widget (linux's ``StringObject`` model lookup, web's Playwright
``select_option``, windows' ``ComboBoxItem`` scan, android's ``By.text``) cannot
express the bug at all — they must *find* the option before they can click it —
while a value-writeback registry (tui, apple) has to check membership
explicitly. So the portable contract is the observable one: **the call raises,
and the picker's value does not move.** A test keyed to 409, or to an exception
type, would pass on one app and mean nothing on the other six.
"""

import pytest

from actions.media import MEDIA_FILTER_ALL

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# A set name no frame can have painted: `fauna.folders.list` never returns it,
# and it is not the `__all__` sentinel. Deliberately NOT random — a fixed value
# keeps a failure's message reproducible, and nothing here depends on freshness.
NEVER_PAINTED_SET = "no-such-folder-this-frame-never-painted"


def test_select_refuses_an_option_the_app_never_rendered(logged_in_app):
    """Selecting an unrendered option must fail loudly and change nothing."""
    app = logged_in_app
    d = app.driver
    app.media.navigate()

    assert d.is_visible("media-folder-filter"), (
        "the Media page must render its folder filter before this contract "
        f"can be asserted: {d.diagnose('media-folder-filter')}"
    )
    before = d.get_text("media-folder-filter")

    # The refusal itself. Any exception counts — see the module docstring for
    # why the *type* is deliberately not part of the contract.
    with pytest.raises(Exception) as refusal:
        d.select("media-folder-filter", NEVER_PAINTED_SET)

    # A refusal that says nothing is barely better than a silent accept
    # (convention 6: failures diagnose themselves). Every app's message quotes
    # the value it was asked for.
    assert NEVER_PAINTED_SET in str(refusal.value), (
        "the refusal must name the value it rejected, or a reader cannot tell "
        f"it apart from a transport fault: {refusal.value}"
    )

    # The load-bearing half: a refused select must not have moved the picker.
    # An app that raised *after* writing the value through would still leave
    # every downstream assertion testing an unreachable state.
    after = d.get_text("media-folder-filter")
    assert after == before, (
        f"a refused select moved the picker anyway ({before!r} -> {after!r}) — "
        "the raise is not enough; the mutation must not happen"
    )

    # And the picker still works: the refusal is a targeted guard, not a wedge.
    app.media.set_filter(MEDIA_FILTER_ALL)
    assert d.get_text("media-folder-filter") == MEDIA_FILTER_ALL, (
        "an offered option must still actuate after a refusal: "
        f"{d.diagnose('media-folder-filter')}"
    )

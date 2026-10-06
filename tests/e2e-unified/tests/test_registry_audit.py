"""Whole-frame registry assertions — the rulings, decided without an app.

`helpers/registry_audit.py` turns two `registry_snapshot()` frames into a verdict
about what losing the nest did to a surface (`docs/goal/architecture/
e2e-conventions.md` § point 17). These are tier_1 for the same reason
`test_frame_invariants.py` is: the comparison is a pure function over two lists
of records, so *what counts as a violation* can be pinned here, cheaply and
deterministically, while the tier_3 offline-gate suite supplies the real frames.

That split is what makes the checker trustworthy. Each assertion below is
red-verified in the only sense a pure function admits — a case shaped exactly
like the defect it exists to catch, next to the legitimate case one letter away
from it that must NOT fire. Two rulings in particular are load-bearing and a
future session should not "tighten" either:

* **A control absent from one of the two frames is not a finding.** It changed
  for reasons this comparison cannot attribute, and attributing it anyway is how
  a whole-frame checker earns a reputation for noise and gets deleted.
* **Read-only labels are out of scope.** They are the bulk of any frame and
  their enabled-ness is inherited decoration; quantifying over them would make
  every assertion here about text.
"""

import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_1]

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import registry_audit  # noqa: E402


def row(
    element_id,
    *,
    enabled=True,
    index=0,
    scope="",
    actuable=True,
    editable=False,
    declares_enabled=True,
):
    """One `registry_snapshot()` record, in the shape apple's `/registry` emits."""
    return {
        "id": element_id,
        "index": index,
        "scope": scope,
        "enabled": enabled,
        "declares_enabled": declares_enabled,
        "actuable": actuable,
        "editable": editable,
    }


# --- what counts as a control -------------------------------------------------


def test_labels_are_not_controls_but_editable_fields_are():
    frame = [
        row("page-heading", actuable=False),
        row("nest-id-input", actuable=False, editable=True),
        row("nests-add-submit-button"),
    ]
    ids = {r["id"] for r in registry_audit.actuable_rows(frame)}
    assert ids == {"nest-id-input", "nests-add-submit-button"}


def test_describe_spells_a_control_the_way_the_driver_addresses_it():
    assert registry_audit.describe(row("like-button", index=2, scope="post-card[1]")) == (
        "post-card[1]/like-button[2]"
    )
    assert registry_audit.describe(row("save-button")) == "save-button[0]"


# --- the delta ----------------------------------------------------------------


def test_delta_reports_each_direction_and_ignores_unchanged_controls():
    before = [row("gated", enabled=True), row("local", enabled=True), row("dead", enabled=False)]
    after = [row("gated", enabled=False), row("local", enabled=True), row("dead", enabled=False)]
    delta = registry_audit.enablement_delta(before, after)
    assert [r["id"] for r in delta["newly_disabled"]] == ["gated"]
    assert delta["newly_enabled"] == []
    assert delta["common"] == 3


def test_a_control_present_in_only_one_frame_is_not_compared():
    # The ruling: appearing/vanishing is not attributable to the nest going
    # away, so it is silent in both directions rather than guessed at.
    before = [row("was-here", enabled=True)]
    after = [row("is-new", enabled=True)]
    delta = registry_audit.enablement_delta(before, after)
    assert delta == {"newly_disabled": [], "newly_enabled": [], "common": 0}


def test_the_same_id_under_two_scopes_is_two_controls():
    before = [
        row("like-button", scope="post-card[0]", enabled=True),
        row("like-button", scope="post-card[1]", enabled=True),
    ]
    after = [
        row("like-button", scope="post-card[0]", enabled=False),
        row("like-button", scope="post-card[1]", enabled=True),
    ]
    delta = registry_audit.enablement_delta(before, after)
    assert [registry_audit.describe(r) for r in delta["newly_disabled"]] == [
        "post-card[0]/like-button[0]"
    ]
    assert delta["common"] == 2


def test_repeated_ids_are_matched_by_occurrence_index():
    before = [row("destination-remove", index=0), row("destination-remove", index=1)]
    after = [
        row("destination-remove", index=0, enabled=False),
        row("destination-remove", index=1, enabled=True),
    ]
    delta = registry_audit.enablement_delta(before, after)
    assert [registry_audit.describe(r) for r in delta["newly_disabled"]] == [
        "destination-remove[0]"
    ]


# --- the assertion ------------------------------------------------------------


def _ok_pair():
    """A surface that behaved: one gated control closed, a local one stayed live."""
    before = [row("pairing-toggle", enabled=True), row("factory-reset", enabled=True)]
    after = [row("pairing-toggle", enabled=False), row("factory-reset", enabled=True)]
    return before, after


def test_a_well_behaved_surface_passes_and_returns_its_delta():
    before, after = _ok_pair()
    delta = registry_audit.assert_offline_gate_reach(before, after, surface="admin-nest")
    assert [r["id"] for r in delta["newly_disabled"]] == ["pairing-toggle"]


def test_an_inverted_gate_is_caught_and_the_message_names_the_control():
    # The defect shape: losing the nest made a control LIVE. A gate only ever
    # subtracts, so this is an inverted verdict or an enablement wired to the
    # wrong half of the connection state.
    before = [row("pairing-toggle", enabled=True), row("inverted", enabled=False)]
    after = [row("pairing-toggle", enabled=False), row("inverted", enabled=True)]
    with pytest.raises(AssertionError) as err:
        registry_audit.assert_offline_gate_reach(before, after, surface="admin-nest")
    assert "inverted[0]" in str(err.value)


def test_every_offender_is_named_never_a_truncated_sample():
    # A capped violation list reads as "these are the problems" when it is not.
    before = [row(f"c{i}", enabled=False) for i in range(9)]
    after = [row(f"c{i}", enabled=True) for i in range(9)]
    # One control must go the other way, or the "gate did nothing" assert fires
    # first and this would be testing the wrong message.
    before.append(row("gated", enabled=True))
    after.append(row("gated", enabled=False))
    with pytest.raises(AssertionError) as err:
        registry_audit.assert_offline_gate_reach(before, after, surface="admin-nest")
    for i in range(9):
        assert f"c{i}[0]" in str(err.value)


def test_an_allow_listed_id_may_come_alive_offline():
    # The escape hatch a reconnect affordance would need — by ID, and empty at
    # every call site today.
    before = [row("gated", enabled=True), row("reconnect-button", enabled=False)]
    after = [row("gated", enabled=False), row("reconnect-button", enabled=True)]
    delta = registry_audit.assert_offline_gate_reach(
        before, after, surface="admin-nest",
        allow_newly_enabled=frozenset({"reconnect-button"}),
    )
    assert [r["id"] for r in delta["newly_enabled"]] == ["reconnect-button"]


def test_a_gate_that_does_nothing_measurable_is_caught():
    # The reason the reach half exists: a per-control assertion passes against
    # this frame whenever the control it picked is disabled for its own reasons.
    frame = [row("pairing-toggle", enabled=True), row("factory-reset", enabled=True)]
    with pytest.raises(AssertionError) as err:
        registry_audit.assert_offline_gate_reach(frame, list(frame), surface="admin-nest")
    assert "did" in str(err.value) and "nothing measurable" in str(err.value)


def test_two_frames_sharing_no_control_fail_loudly_rather_than_vacuously():
    # Same page both times is a precondition, not an assumption: an empty
    # comparison that returns green is the vacuous assertion this refuses to be.
    with pytest.raises(AssertionError) as err:
        registry_audit.assert_offline_gate_reach(
            [row("was-here")], [row("is-new")], surface="admin-nest"
        )
    assert "share no actuable control" in str(err.value)


def test_an_app_with_no_registry_surface_is_refused_not_passed():
    # `None` is "I could not look". Reading it as coverage is the one failure
    # mode a whole-frame checker must never have.
    with pytest.raises(AssertionError) as err:
        registry_audit.assert_offline_gate_reach(None, [row("x")], surface="admin-nest")
    assert "publishes no structured registry" in str(err.value)

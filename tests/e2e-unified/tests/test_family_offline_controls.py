"""family-safety outcome 66 — offline, the guardian's Save and Remove are greyed
out, and saving for a child you no longer supervise says so and sends nothing.

`docs/goal/behavior/family-safety.md` § Implementation status today (the fourth
pillar's guardian host — "three things the build settled"): (1) a save for a
ward the caller no longer guards dispatches nothing and says so
(`features.editor_ward_missing`) — there is no policy to carry, and a default
one would reset the reach knobs; (3) offline, the guardian host's Save and
Remove and `family-policy-save-button` are disabled with the standard reason,
because `fauna.family.policy.update` is `OnlineOnly`
(`account-offline-mutation.md` § The offline-mutation contract for the class).

The offline half reads a gated control beside a live sibling at the same moment
(Cancel is pure local UI) — `test_offline_gate.py`'s rule: a gate that greyed
everything would pass a one-sided check. The nest is taken down and brought back
on the same port and data dir, as `test_feature_limits.py`'s self-host twin does.

The ex-ward half resolves the link from the admin side while the guardian's
editor is open (the guardian's own graduate gesture closes the ward's editor
with it, so it cannot set up this state); the save is the guardian's gesture
under test. Own pair: graduation ends the pairing.
"""

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.admin_wire import graduate_ward
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_family import _admit_own_pair
from tests.test_feature_limits import OPS_DAY_CELL, P2P_ROW, _open_ward_feature_limits

pytestmark = pytest.mark.tier_3

DISCONNECT_GATE_S = 90.0
RECONNECT_GATE_S = 120.0

SAVE = "feature-policy-editor-save-button"
REMOVE = "feature-policy-editor-remove-button"
CANCEL = "feature-policy-editor-cancel-button"
POLICY_SAVE = "family-policy-save-button"


@pytest.mark.feature("family-safety")
def test_family_guardian_writes_grey_out_offline_and_a_save_for_an_ex_ward_sends_nothing(
    admin_app, request, nest_instance
):
    app = admin_app
    editor = app.feature_policy_editor
    guardian_identity, ward_identity, _guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "offctl"
    )

    try:
        # ── A guardian-authored limit exists, so Remove renders. ──
        _open_ward_feature_limits(app, request, nest_instance, guardian_identity, ward_handle)
        app.family.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        editor.set_cell(OPS_DAY_CELL, "3")
        editor.save()
        assert editor.wait_for_status(S.features.editor_saved) == S.features.editor_saved, (
            f"precondition: the guardian's limit did not save: status={editor.status()!r} "
            f"error={app.error_text()!r}"
        )
        app.family.wait_for_feature_limit_summary(P2P_ROW, S.features.authored_limited_one)
        editor.cancel()
        app.family.open_feature_limit_editor(P2P_ROW)
        editor.wait_open()
        assert editor.remove_present(), "a document exists, so Remove renders"
        for control in (SAVE, REMOVE, POLICY_SAVE):
            assert app.driver.is_enabled(control), (
                f"{control} must be live while the nest IS reachable"
            )

        # ── Offline: Save, Remove and the policy save grey out; Cancel stays live. ──
        stop_nest(nest_instance, graceful=True)
        try:
            wait_until(
                lambda: not any(
                    app.driver.is_enabled(c) for c in (SAVE, REMOVE, POLICY_SAVE)
                ),
                DISCONNECT_GATE_S,
                diagnose=lambda: (
                    "with no nest the guardian's Save / Remove / policy save stayed enabled: "
                    f"{ {c: app.driver.is_enabled(c) for c in (SAVE, REMOVE, POLICY_SAVE)} } "
                    f"error={app.error_text()!r}"
                ),
            )
            assert app.driver.is_enabled(CANCEL), (
                "Cancel is local UI and must stay live offline — a gate that greys "
                "everything is over-claiming"
            )
        finally:
            start_nest_in_place(nest_instance)
        wait_until(
            lambda: app.driver.is_enabled(SAVE) and app.driver.is_enabled(REMOVE),
            RECONNECT_GATE_S,
            diagnose=lambda: f"Save / Remove never re-enabled: error={app.error_text()!r}",
        )

        # ── The child graduates while the editor is open; the save says so. ──
        graduate_ward(nest_instance, ward_identity["actor_id_hex"])
        editor.set_cell(OPS_DAY_CELL, "2")
        editor.save()
        # The refusal lands on the editor's verdict line or on the page's
        # `error-message` (tui puts it on the latter) — either says it.
        missing = S.features.editor_ward_missing
        wait_until(
            lambda: editor.status() == missing or app.error_text() == missing,
            RPC_ROUNDTRIP_S + UI_SETTLE_S,
            diagnose=lambda: (
                "a save for a child the guardian no longer supervises must say so and "
                f"send nothing; status={editor.status()!r} error={app.error_text()!r}"
            ),
        )
        editor.close_if_open()
    finally:
        editor.close_if_open()

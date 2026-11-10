"""family-safety outcome 62 — a policy value the app cannot read is shown as the
strictest choice, so a save never weakens the policy.

`docs/goal/behavior/family-safety.md` § Implementation status today: "an
*unrecognized* value renders as the strictest option (`hold` / `block`), never
the permissive one … within a major version a client may be **older than its
nest**, so a newer nest may store a value the client cannot name."

The nest stands in for a newer one through its ``test-hooks`` endpoint ``POST
/api/v1/test/family/policy_knob`` (`bins/fauna-nest/src/guardian_policy_test_hook.rs`),
because the only write path, ``fauna.family.policy.update``, refuses a value it
cannot name. The status read serves the stored string as it is, so what the
guardian's editor shows is the app's own parse. The save is the guardian's
gesture under test (convention 8): it changes a DIFFERENT knob, and the policy
read back over the wire never carries ``allow`` for an unreadable knob — the
two required knobs come back strict, and the optional bridge-DM knob, which an
untouched save leaves unchanged, keeps the newer nest's word or comes back
strict. Own pair: the policy row is left holding values no reset path
writes.
"""

import json
import urllib.request

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.family_wire import family_status, ward_row
from helpers.waiting import wait_until
from tests.test_family import _admit_own_pair

pytestmark = pytest.mark.tier_3

#: A knob word no build names — the value a newer nest might store.
UNREADABLE = "quarantine-row-4466"


def _store_raw_knob(nest_instance, ward_hex: str, knob: str, value: str) -> int:
    """The nest's ``test-hooks`` endpoint: store ``value`` verbatim in one knob
    of the ward's policy row; returns the rows written."""
    body = json.dumps({"actor_id": ward_hex, "knob": knob, "value": value}).encode()
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/family/policy_knob",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"policy-knob hook returned {resp.status}"
        return json.loads(resp.read())["written"]


@pytest.mark.feature("family-safety")
def test_family_unreadable_knob_values_render_strictest_and_a_save_never_weakens_them(
    admin_app, request, nest_instance
):
    from conftest import _login_app_as

    app = admin_app
    guardian_identity, ward_identity, _guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "failclosed"
    )
    ward_hex = ward_identity["actor_id_hex"]
    for knob in ("unknown_sender_mail", "feed_sources", "unknown_peer_dm"):
        written = _store_raw_knob(nest_instance, ward_hex, knob, UNREADABLE)
        assert written == 1, f"the ward's policy row takes {knob}: wrote {written}"

    # The wire serves the stored word as it is — the render below is the app's.
    served = ward_row(family_status(nest_instance, guardian_identity), ward_identity)["policy"]
    assert served["unknown_sender_mail"] == UNREADABLE, served
    assert served["feed_sources"] == UNREADABLE, served
    assert served["contact_approval"] is False, served

    app.driver.reset()
    _login_app_as(app, request, nest_instance, guardian_identity)
    app.family.navigate()
    wait_until(
        lambda: (app.family.reload() or True) and ward_handle in app.family.ward_handles(),
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: f"ward {ward_handle!r} not listed. error={app.error_text()!r}",
    )
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.policy_editor_visible(), (
        f"the policy editor did not load. error={app.error_text()!r}"
    )

    strict = {
        "unknown sender mail": (app.family.unknown_sender_label,
                                app.family.UNKNOWN_SENDER_LABELS["hold"]),
        "feed sources": (app.family.feed_sources_label,
                         app.family.FEED_SOURCES_LABELS["block"]),
        "unknown peer DM": (app.family.unknown_peer_dm_label,
                            app.family.UNKNOWN_PEER_DM_LABELS["hold"]),
    }
    for name, (read, want) in strict.items():
        wait_until(
            lambda read=read, want=want: read() == want,
            UI_SETTLE_S,
            diagnose=lambda name=name, read=read, want=want: (
                f"an unreadable {name} value must render as the strictest option "
                f"{want!r}, shows {read()!r}. error={app.error_text()!r}"
            ),
        )

    # The guardian saves a DIFFERENT knob; the unreadable ones go back strict.
    assert app.family.contact_approval_state() == "off"
    app.family.set_contact_approval(True)
    app.family.save_policy()
    assert not app.has_error(), f"the save raised: {app.error_text()!r}"

    def _saved() -> dict | None:
        policy = ward_row(family_status(nest_instance, guardian_identity), ward_identity)["policy"]
        return policy if policy["contact_approval"] is True else None

    wait_until(
        lambda: _saved() is not None,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "the guardian's save never reached the nest: "
            f"{ward_row(family_status(nest_instance, guardian_identity), ward_identity)['policy']!r} "
            f"error={app.error_text()!r}"
        ),
    )
    saved = _saved()
    assert saved["unknown_sender_mail"] == "hold", (
        f"a save must never weaken an unreadable mail knob: {saved['unknown_sender_mail']!r}"
    )
    assert saved["feed_sources"] == "block", (
        f"a save must never weaken an unreadable feed-sources knob: {saved['feed_sources']!r}"
    )
    # The bridge-DM knob rides the wire as an optional field, absent meaning
    # "leave unchanged" (family-safety.md § Policy-update compatibility), so an
    # app may keep the newer nest's word on a save that never touched it — the
    # nest fails it closed to hold at enforcement — or send the hold it
    # rendered. Either keeps the policy; only `allow` (or absence, which the
    # status read uses for `allow`) would weaken it.
    assert saved.get("unknown_peer_dm") in (UNREADABLE, "hold"), (
        f"a save must never weaken an unreadable bridge-DM knob: {saved.get('unknown_peer_dm')!r}"
    )

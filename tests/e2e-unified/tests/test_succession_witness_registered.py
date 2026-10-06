"""Tier_3: **this seat registered an in-group succession witness at all.**

The narrow, per-app half of the member story, split out from the ceremony
journey on purpose. ``test_identity_succession_ceremony.py`` proves the whole
thing — a real succession, a real member, a participant row that re-points —
and needs a *second real-engine driver seat* to do it, which only linux and tui
have (``succession_member_app``'s ``skip_unbuilt``). That seat is a build, and
gating the *witness's existence* on it is what let five apps ship with no
witness at all for a month: every one of them looked exactly like an app whose
member-seat fixture happened to skip.

So this asserts the one thing every app can answer for itself, with no peer, no
ceremony and no second seat: **``data.succession_witness`` is published, off the
shared renderer, and its anchor store is readable.** A client with no witness
registered answers ``None`` — indistinguishable, from the outside, from a client
whose members never see a succession, which is exactly the silence this pins.

What it deliberately does NOT assert: that any statement was seen or settled.
``statements.seen`` is 0 here and should be — nothing happened. The reading
order for a real run is the report's own
(``fauna_client_recovery::witness::state_json``), exercised by the ceremony
journey.

Owner: ``succession-propagation.md`` § Implementation status today — the ✅
in-group-statement bullet, which holds the per-app witness count.
"""

from __future__ import annotations

import pytest

pytestmark = [pytest.mark.tier_3]


# The FFI apps (windows / macOS / iOS / android) register the witness inside
# `FfiNestClient::build_conversations_session`, so it exists only once the REAL
# conversations session is up — and on those apps that is a LAUNCH-TIME gate
# (`FAUNA_E2E_REAL_CONVERSATIONS`, set by conftest's
# `_apply_real_conversations_env` for tests carrying this marker). Without it
# windows times out on the readiness poll and macOS/iOS/android run against the
# MOCK backend, where a null report would indict the app rather than the missing
# flag. linux/web/tui use the runtime toggle and are unaffected by the marker.
# ⚠ `_apply_real_conversations_env` is session-wide, so run this module in its
# own pytest invocation when the set includes an FFI app (`real_faunamls_app`'s
# docstring owns that rule).
@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_a_conversations_seat_registers_a_succession_witness(real_faunamls_app):
    app = real_faunamls_app

    report = app.driver.get_state("data.succession_witness")
    assert report is not None, (
        "this seat published no `data.succession_witness` at all — it registered "
        "no SuccessionWitness, so every in-group succession statement its member "
        "receives will degrade to a bare add ('a stranger joined the group') "
        "with nothing anywhere saying so. See succession-propagation.md "
        "§ Implementation status today, the witness bullet"
    )

    # The three halves the shared renderer always emits. Their PRESENCE is the
    # assertion: each is a separately-owned reading, and an app that hand-rolled
    # the shape instead of calling `witness::state_json` is what a missing key
    # would mean.
    for key in ("statements", "harvest", "peers", "anchor_store"):
        assert key in report, (
            f"`data.succession_witness` is missing `{key}` — the shape is the "
            f"shared renderer's (fauna_client_recovery::witness::state_json) and "
            f"no app derives it locally; got keys {sorted(report)!r}"
        )

    # The anchor store is the member's own `fauna.state.peer-anchors` plane. "Unreadable" here
    # indicts this seat's account plane rather than any peer — the distinction
    # `AnchorStoreState` exists to draw — and it would make every later
    # verification silently anchorless.
    assert report["anchor_store"]["state"] in ("read", "not_read"), (
        "this seat's anchor store is not readable, so the witness has nowhere "
        f"to remember a chain head: {report['anchor_store']!r}"
    )

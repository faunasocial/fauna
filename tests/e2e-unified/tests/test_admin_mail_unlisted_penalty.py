"""E2E coverage for the admin-mail Spam group's unlisted-recipient-penalty
knob (``admin-mail-unlisted-recipient-penalty``) — Plan 3 (recipient-whitelist
client UI; tracked internally).

A plain numeric text input (points; 0 = off — the catch-all recipient
penalty) in the admin-mail Spam group, saved by the existing
``admin-mail-spam-save-button`` full-PUT (``fauna.bridges.put_spam_policy``) —
the same shape and save gesture as every other Spam-group knob
(``docs/goal/behavior/mail-policy-config.md`` § Spam). Mirrors
``test_admin_mail.py``'s ``test_bayesian_weight_save_round_trips`` for the
render/round-trip half (same ``admin_app`` fixture, same
``fauna.bridges.get_mail_config`` ground-truth twin).

tier_3: a real client driver against a real ``fauna-nest`` binary (same tier
as ``test_admin_mail.py`` — there is no tier_2 mocked-backend pattern for the
admin-mail surface).

The full-PUT no-clobber guarantee (saving one Spam field must not zero a
sibling field the caller didn't touch) is proven at the shared-Rust layer by
the regression test ``save_spam_preserves_unlisted_penalty_no_clobber``
(``libs/fauna-client-mail-settings/src/admin_policy.rs``). The second test
here re-asserts the SAME guarantee at the UI level: set the penalty, save,
set an UNRELATED Spam knob, save again, and confirm the penalty is STILL
what we set — not clobbered by the second full-PUT.

Implemented on windows (Task 9), linux (Task 8), macOS (Task 10 — the shared
FaunaKit ``AdminMailView`` Spam group serves macOS/iOS, though only the macos
marker is registered here so far), and web (Task 7). Android (Task 11) is
built + Compose-content-tested (``AdminMailContentTest.kt``'s
``saveSpamGathersEditedUnlistedRecipientPenalty``) with an exact
``admin-mail-unlisted-recipient-penalty`` ID match — the marker below
declares the coverage contract; the tier_3 run itself stays host-emulator-
gated until a real device run can execute it.
"""

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    # tui lifted the FULL admin-mail page (M8, 2026-07-18) incl. the Spam group's
    # unlisted-recipient-penalty knob + the shared put_spam_policy no-clobber PUT.
    pytest.mark.tui,
    pytest.mark.android,
]

PENALTY = "admin-mail-unlisted-recipient-penalty"
BAYESIAN_WEIGHT = "admin-mail-spam-bayesian-weight"


def _admin_client(nest_instance):
    """Same shape as ``test_admin_mail.py``'s ``_admin_client`` — the
    ground-truth read path for the round-trip."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll ``predicate`` until true or the deadline (the form hydrates + saves
    asynchronously: admin nav -> MailPolicyMachine -> WS-RPC -> render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


@pytest.mark.feature("admin-mail-policy")
def test_unlisted_penalty_save_round_trips(admin_app, nest_instance):
    """Setting the unlisted-recipient-penalty via the UI writes through the
    shared MailPolicyMachine -> ``fauna.bridges.put_spam_policy`` -> nest,
    proven by reading the Admin ``get_mail_config`` twin (the same read the
    page hydrates from), then re-read on the page itself."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    assert _wait(lambda: admin_app.admin.mail_field_present(PENALTY)), (
        f"{PENALTY} missing — Spam group not rendered. "
        f"error: {admin_app.error_text()!r}"
    )

    admin_app.admin.set_mail_field(PENALTY, "1000")
    admin_app.admin.save_mail_spam()

    client = _admin_client(nest_instance)
    with client:
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "unlisted_recipient_penalty"
            ]
            == 1000
        ), (
            "put_spam_policy did not persist unlisted_recipient_penalty=1000 "
            f"(get_mail_config still shows the old value). "
            f"error: {admin_app.error_text()!r}"
        )

    assert _wait(lambda: admin_app.admin.mail_field_text(PENALTY).strip() == "1000"), (
        "the page did not re-render the persisted unlisted-recipient-penalty "
        f"(1000). error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after the round-trip save: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-mail-policy")
def test_unlisted_penalty_survives_other_knob_save(admin_app, nest_instance):
    """A sibling Spam-group full-PUT must not clobber a previously-saved
    unlisted-recipient-penalty — the UI-level mirror of the shared-Rust
    regression test ``save_spam_preserves_unlisted_penalty_no_clobber``,
    exactly the shared-Rust Task 1 fix this test exercises through the UI."""
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )
    assert _wait(lambda: admin_app.admin.mail_field_present(PENALTY)), (
        f"{PENALTY} missing — Spam group not rendered. "
        f"error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        # --- 1. Set the penalty and save. ---
        admin_app.admin.set_mail_field(PENALTY, "1000")
        admin_app.admin.save_mail_spam()

        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "unlisted_recipient_penalty"
            ]
            == 1000
        ), (
            "put_spam_policy did not persist unlisted_recipient_penalty=1000 "
            f"before the no-clobber check. error: {admin_app.error_text()!r}"
        )

        # --- 2. Save a DIFFERENT Spam knob — a second full-PUT. The penalty
        # must survive it (not reset to 0). ---
        assert _wait(lambda: admin_app.admin.mail_field_present(BAYESIAN_WEIGHT)), (
            f"{BAYESIAN_WEIGHT} missing — Spam group not rendered. "
            f"error: {admin_app.error_text()!r}"
        )
        admin_app.admin.set_mail_field(BAYESIAN_WEIGHT, "700")
        admin_app.admin.save_mail_spam()

        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "bayesian_weight_milli"
            ]
            == 700
        ), (
            "the sibling Spam save (bayesian_weight_milli=700) did not persist. "
            f"error: {admin_app.error_text()!r}"
        )
        assert _wait(
            lambda: client.call("fauna.bridges.get_mail_config", {})["spam"][
                "unlisted_recipient_penalty"
            ]
            == 1000
        ), (
            "unlisted_recipient_penalty was clobbered by a sibling Spam save "
            f"(expected it to survive at 1000). error: {admin_app.error_text()!r}"
        )

    # The page re-rendered the surviving value (round-tripped, not just kept
    # the typed text from step 1).
    assert _wait(lambda: admin_app.admin.mail_field_text(PENALTY).strip() == "1000"), (
        "the page did not re-render the surviving unlisted-recipient-penalty "
        f"(1000) after the sibling save. error: {admin_app.error_text()!r}"
    )
    assert not admin_app.error_text().strip(), (
        f"unexpected error after the no-clobber save sequence: {admin_app.error_text()!r}"
    )

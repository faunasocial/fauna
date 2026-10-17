"""Direct admission through the app UI — the third account-creation path
(`public-mode.md` § Registration & Identity: "an admin admitting a user
directly via `fauna.admin.users.create`"), reachable for the first time
without driving the RPC by hand.

Until this section existed the path was RPC-only: every harness user comes
from `create_actor_and_register` calling the kind directly, and no app
rendered a form for it — a ratified account-creation path a real admin could
not use (the one-surface invariant: a capability not in the app UI is not
configurable). tui led (`testing.md` § Default app and nest mode); linux, web,
android, macOS, iOS and windows followed — all 7 apps render the section as
of windows (2026-08-26, the last app), and the action layer no longer
declares any `skip_unbuilt` gate for it.

The journey: the admin types a known actor id + the handle they are admitting
it under (there is no set-later — `fauna.admin.users.clear_handle` can only
clear), picks a tier ("admission is always choosing a tier"), admits. The
assertions are the two durable nest truths the admit must produce, both
deadline-polled (testing.md convention 14 — a green run pays nothing):

1. the admitted actor AUTHENTICATES — the exact thing an unadmitted actor
   cannot (`fauna.auth.not_registered`, auto-registration is removed);
2. acting as their own working session, they find themselves by the handle
   the admin typed (`fauna.search.query`, `content_type: "profile"` — the
   admit handler's `index_profile` write), which proves the handle crossed
   UI → wire → nest end to end. The mail-send capability that handle buys is
   pinned nest-side (`conformance_admin.rs::create_with_a_handle_*` + the
   From-handle gate's own arms in `email_handlers.rs`); what only this test
   can catch is the app leg dropping or swapping the handle on the way.

Test taxonomy: tier_3 — real nest binary, real WS-RPC, the admin driving the
real app UI.
"""

import time

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import mint_token_via_handshake
from conftest import MAIL_PRIMARY_DOMAIN
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# Generous ceiling for the admit → authenticate/index round trip under live
# machine load; a green run polls past it the moment the state lands.
ADMIT_BUDGET_S = 45.0


def _poll(deadline: float, fn, last_err: list):
    """Run `fn` until it returns truthy or the deadline passes; keep the last
    exception for the failure message (convention 6 — diagnose yourself)."""
    while time.time() < deadline:
        try:
            got = fn()
            if got:
                return got
        except Exception as e:  # noqa: BLE001 — polled probe, kept for the report
            last_err[:] = [e]
        time.sleep(0.5)
    return None


def _profile_hits(nest_url: str, sk: SigningKey, query: str) -> list:
    """`fauna.search.query` for `query`, as the keypair's own session —
    profile hits only. The hit's `content_id` is a DERIVED key
    (`blake3("profile:<actor-hex>")` — `db::content_id_for_document`), and an
    admitted profile's bio is empty so its snippet is too: neither field can
    name the actor, so identity rides on the QUERY being a globally-unique
    token (the handle embeds 10 hex chars of the actor id)."""
    with WsRpcAdminClient(
        nest_url,
        actor_id=bytes(sk.verify_key),
        signing_key=bytes(sk),
    ) as me:
        reply = me.call("fauna.search.query", {"query": query, "content_type": "profile"})
    return [r for r in reply.get("results", []) if r.get("content_type") == "profile"]


def test_every_admit_control_is_reachable_on_the_hub(admin_app):
    """An admin can actually get at every control of the Admit section.

    Deliberately NOT `feature`-marked: a layout regression guard, not a catalog
    outcome (`features-lint` rule 3 would demand a coverage-contract line for it).
    The outcome it protects is the journey below — an admin admits a user.

    The journey below drives the form by id, and an id-driven write reaches a
    control the window has clipped to nothing (UIA `ValuePattern`/`Invoke` need no
    pixels) — so windows' horizontal Admit row, which overflowed its column at the
    600-DIP minimum window and hid the handle input and the Admit button, passed
    the journey while no admin could have used the form. This is the reachability
    check the journey cannot make: `wait_for` on every control, all failures in one
    message (e2e rule 6). `wait_for` scrolls the vertical scroller, so a control
    below the fold passes; only one no vertical scroll can reach fails.
    """
    admin_app.admin.navigate_users()
    admin_app.driver.wait_for("admin-users-admit-actor-input", timeout=30)

    stranded = []
    for control_id in (
        "admin-users-admit-handle-input",
        "admin-users-admit-tier-select",
        "admin-users-admit-button",
    ):
        try:
            admin_app.driver.wait_for(control_id, timeout=10)
        except TimeoutError as e:
            stranded.append(str(e))
    assert not stranded, "Admit controls a user cannot reach:\n  " + "\n  ".join(stranded)


@pytest.mark.feature("admin-users")
def test_admit_with_a_handle_yields_a_working_account(admin_app, nest_instance):
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    handle = f"admitted-{actor_hex[:10]}"

    admin_app.admin.navigate_users()
    admin_app.admin.admit_user(actor_hex, handle=handle, tier="free")

    deadline = time.time() + ADMIT_BUDGET_S

    # (1) The admitted actor authenticates. Before the admit lands, the
    # handshake is refused `fauna.auth.not_registered` — so a token IS the
    # admission, not a smoke signal.
    last: list = []
    token = _poll(deadline, lambda: mint_token_via_handshake(nest_instance["url"], sk), last)
    assert token, (
        f"the admitted actor never authenticated within {ADMIT_BUDGET_S}s — "
        f"the admit did not land. last refusal: {last!r}; "
        f"page error: {admin_app.error_text()!r}"
    )

    # (2) ... and, as their own working session, finds themselves under the
    # handle the admin typed. The handle is a globally-unique token, so a
    # profile hit for it can only be this admission — which is exactly what a
    # handle-DROPPING leg cannot produce (a handle-less admit indexes
    # nothing; the admit handler's `index_profile` runs only in its
    # with-handle arm), so this arm discriminates the defect class this test
    # exists for.
    last2: list = []
    found = _poll(deadline, lambda: _profile_hits(nest_instance["url"], sk, handle), last2)
    assert found, (
        f"the admitted actor's profile never indexed under {handle!r} within the "
        f"budget — the handle did not survive the UI → wire crossing. "
        f"last error: {last2!r}; page error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("family-safety")
def test_admit_with_a_guardian_yields_a_supervised_ward(admin_app, nest_instance):
    """The Admit section's guardian picker (`family-safety.md` § The guardianship
    link, *Set at admission*): the admin picks a guardian beside the tier and
    admits; the admitted account's OWN `fauna.family.status` names that guardian
    (`supervised_by`), and the guardian's names the ward. A picker that did
    nothing admits an ordinary account — the failure this reads back to catch.

    The guardian is fixture setup (the picker offers every non-suspended
    account); the admit itself goes through the UI."""
    from helpers.admin_wire import admit_adult

    admin_app.admin.require_admit_guardian_supported()
    guardian_handle = f"admitguardian-{int(time.time() * 1000)}"
    guardian = admit_adult(nest_instance, guardian_handle)
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    handle = f"admitward-{actor_hex[:10]}"

    admin_app.admin.navigate_users()
    admin_app.admin.admit_user(actor_hex, handle=handle, tier="free", guardian=guardian_handle)

    last: list = []
    token = _poll(time.time() + ADMIT_BUDGET_S,
                  lambda: mint_token_via_handshake(nest_instance["url"], sk), last)
    assert token, (
        f"the admitted ward never authenticated within {ADMIT_BUDGET_S}s. "
        f"last refusal: {last!r}; page error: {admin_app.admin.users_action_error_text()!r}"
    )

    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk),
    ) as me:
        ward_status = me.call("fauna.family.status", {})
    supervised_by = ward_status.get("supervised_by") or {}
    assert supervised_by.get("actor_id") is not None and (
        bytes(supervised_by["actor_id"]).hex() == guardian["actor_id_hex"]
    ), (
        f"the admitted account must be supervised by {guardian_handle!r}; its "
        f"family status reads {ward_status!r}; page error: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )

    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(guardian["signing_key"].verify_key),
        signing_key=bytes(guardian["signing_key"]),
    ) as g:
        guardian_status = g.call("fauna.family.status", {})
    ward_ids = {bytes(w["actor_id"]).hex() for w in guardian_status.get("wards", [])}
    assert actor_hex in ward_ids, (
        f"the guardian's family status must list the admitted ward: {guardian_status!r}"
    )


@pytest.mark.feature("admin-users")
def test_a_mistyped_person_id_is_caught_on_the_page_and_nothing_is_sent(admin_app, nest_instance):
    """A person id that is not 64 hex characters is refused on the page with
    the form's own hint, and nothing reaches the nest (`admin.md` § 2. Users —
    a malformed actor id is a local hint with nothing dispatched).

    The typo is one character off a real key. The hint is the LOCAL message,
    never the nest's refusal text, which is what tells "caught on the page"
    apart from "sent and refused"; and the real key stays unregistered — the
    handshake is refused `not_registered` — so no admission happened."""
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()
    typo = actor_hex[:-1] + "g"

    admin_app.admin.navigate_users()
    admin_app.admin.admit_user(typo, handle=f"typo-{actor_hex[:10]}", tier="free")

    hint = S.admin.users_page.admit_actor_hint
    last: list = []
    shown = _poll(time.time() + ADMIT_BUDGET_S,
                  lambda: admin_app.admin.users_action_error_text() == hint, last)
    assert shown, (
        f"a mistyped id must be caught on the page with {hint!r}; the page shows "
        f"{admin_app.admin.users_action_error_text()!r} (global banner: "
        f"{admin_app.error_text()!r}; last probe error: {last!r})"
    )
    assert not admin_app.has_error(), (
        f"the hint belongs on the Users page's own line, not the global banner: "
        f"{admin_app.error_text()!r}"
    )
    with pytest.raises(Exception) as refused:
        mint_token_via_handshake(nest_instance["url"], sk)
    assert "not_registered" in str(refused.value), (
        f"the real key must not have been admitted: {refused.value!r}"
    )


@pytest.mark.feature("admin-users")
def test_admitting_with_the_handle_left_blank_signs_in_and_cannot_send_from_the_nest(
        admin_app, nest_instance):
    """The handle left blank admits a handle-less account: it signs in, and the
    nest refuses it a `From:` on the nest's own mail domain until it has a
    handle (`admin.md` § 2. Users; the From-handle gate in
    `bins/fauna-nest/src/email_handlers.rs`).

    The send is the admitted member's own act over their own session — the
    observable the outcome names. A send can fail for other reasons, so the
    assertion reads the refusal's code and its no-handle reason."""
    sk = SigningKey.generate()
    actor_hex = bytes(sk.verify_key).hex()

    admin_app.admin.navigate_users()
    admin_app.admin.admit_user(actor_hex, handle=None, tier="free")

    last: list = []
    token = _poll(time.time() + ADMIT_BUDGET_S,
                  lambda: mint_token_via_handshake(nest_instance["url"], sk), last)
    assert token, (
        f"the handle-less admission never signed in within {ADMIT_BUDGET_S}s. "
        f"last refusal: {last!r}; page error: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )

    sender = f"nobody-{actor_hex[:10]}@{MAIL_PRIMARY_DOMAIN}"
    raw = (
        f"From: {sender}\r\nTo: someone@external.test\r\n"
        f"Subject: handle-less\r\nMessage-ID: <{actor_hex[:16]}@{MAIL_PRIMARY_DOMAIN}>\r\n"
        f"\r\nhello\r\n"
    ).encode()
    with WsRpcAdminClient(
        nest_instance["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk),
    ) as me:
        with pytest.raises(RpcCallError) as refused:
            me.call("fauna.email.send", {
                "recipients": ["someone@external.test"],
                "raw_rfc5322": raw,
            })
    assert refused.value.code == "fauna.email.permission_denied", (
        f"a handle-less member must be refused the nest's own From:; got {refused.value!r}"
    )
    assert "set a handle" in str(refused.value.details), (
        f"the refusal must be the no-handle arm: {refused.value.details!r}"
    )
